"""Long-running background process tool plugin (#368).

Tools are synchronous calls; this one starts a process that outlives the
call (a dev server, a local API), waits until it is ready, and later stops
exactly what it started — never anything else on the machine.

Params:
  - ``action`` (required): ``start`` | ``stop`` | ``status``.
  - ``name`` (required, str): the service's name, unique per state
    directory. Letters, digits, ``_``, ``.`` and ``-``.

``start`` also takes:
  - ``command`` (required): an argv list, or with ``shell: true`` a string
    run by ``/bin/sh -c``.
  - ``shell`` (optional, bool, default false).
  - ``cwd`` (optional, str), ``env`` (optional, mapping merged over the
    inherited environment).
  - ``ports`` (optional, list[int]): ports the service will listen on.
  - ``ready`` (optional): an ``http(s)://`` URL (ready on any response
    below 500; certificate verification is off, for local dev
    certificates) or a TCP port on localhost. Its port is claimed like
    ``ports`` when it is a port or a localhost URL.
  - ``ready_timeout_ms`` (optional, default 60000, never longer than the
    effect's own timeout).
  - ``log`` (optional, str, default ``<state_dir>/<name>.log``): stdout and
    stderr are appended here.
  - ``grace_ms`` (optional, default 5000): SIGTERM-to-SIGKILL grace when a
    failed or cancelled start tears the process group down again.

``stop`` takes ``grace_ms`` (same default); ``status`` takes nothing else.

Ownership is provable before any signal: a record in the state directory
(``runtime.plugins.service.state_dir``, default
``~/.config/circuitry/services``) holds the process group id, the group
leader's start time and the machine's boot time, written under an
``flock`` on ``<state_dir>/.lock``. A pid cannot be reused while a process
group with that id exists, so a dead leader whose group still has members
is still ours; a live leader whose start time differs is a different
process and is never signalled; a different boot time means the record is
stale. A stale record is dropped. A port held by a process this plugin did
not start is reported (pid, command and cwd via ``lsof`` when it is
installed) and never signalled.

POSIX only (process groups, ``flock``).
"""

from __future__ import annotations

import json
import os
import re
import shutil
import signal
import socket
import ssl
import subprocess
import threading
import time
import urllib.error
import urllib.request
from collections.abc import Iterator
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

from ..core.cancellation import get_token
from ..preflight import CheckResult
from .base import ToolResult, _as_bool

_ACTIONS = ("start", "stop", "status")
_NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,99}$")
_LOCAL_HOSTS = ("localhost", "127.0.0.1", "::1")

_DEFAULT_READY_TIMEOUT_MS = 60_000
_DEFAULT_GRACE_MS = 5_000
#: After the group is gone (or after SIGKILL), how long to keep polling.
_SETTLE_SECONDS = 5.0
_POLL_SECONDS = 0.1
_LOG_TAIL_LINES = 40
_LOG_TAIL_CHARS = 4_000
#: Linux derives ``btime`` from the current time minus uptime, so two reads
#: can differ by a second; a reboot moves it by far more than this.
_BOOT_TIME_TOLERANCE_SECONDS = 5
_HELPER_TIMEOUT_SECONDS = 10
#: ``ps``/``lsof`` output that never depends on the caller's locale or zone,
#: so a start time recorded by one run compares equal in the next.
_HELPER_ENV = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "LC_ALL": "C", "TZ": "UTC"}

#: Leaders this interpreter started: reaped here, so a leader that exits
#: while this process is still running does not linger as a zombie that
#: keeps its process group alive.
_started: dict[int, subprocess.Popen[bytes]] = {}
_started_lock = threading.Lock()


def default_state_dir() -> Path:
    """``~/.config/circuitry/services``, beside the global config — read at
    call time so a patched ``GLOBAL_CONFIG_DIR`` (tests) is honoured."""
    from ..cli import config as config_module

    return Path(config_module.GLOBAL_CONFIG_DIR) / "services"


def _helper(cmd: list[str]) -> subprocess.CompletedProcess[str] | None:
    """Run a short ``ps``/``lsof``/``sysctl`` query. Deliberately not
    ``run_tracked``: stopping a service must still work while a cancelled
    run cleans up."""
    try:
        return subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=_HELPER_TIMEOUT_SECONDS,
            env=_HELPER_ENV,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None


def _is_linux_proc() -> bool:
    return Path("/proc/self/stat").exists()


def boot_time() -> int:
    """The machine's boot time, in seconds since the epoch."""
    if _is_linux_proc():
        for line in Path("/proc/stat").read_text(encoding="ascii").splitlines():
            if line.startswith("btime "):
                return int(line.split()[1])
        raise RuntimeError("service: no btime in /proc/stat.")
    proc = _helper(["sysctl", "-n", "kern.boottime"])
    match = re.search(r"sec\s*=\s*(\d+)", proc.stdout if proc else "")
    if match is None:
        raise RuntimeError("service: cannot read the boot time (sysctl -n kern.boottime).")
    return int(match.group(1))


def _pid_exists(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def process_start_time(pid: int) -> str | None:
    """*pid*'s start time as an opaque string, or ``None`` when no such
    process exists. Raises when the process exists but its start time
    cannot be read — ownership is then unprovable."""
    if _is_linux_proc():
        try:
            stat = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8", errors="replace")
        except (FileNotFoundError, ProcessLookupError):
            return None
        # Field 22 (starttime); field 2 (comm) may contain spaces and ')'.
        fields = stat[stat.rindex(")") + 2 :].split()
        return fields[19]
    proc = _helper(["ps", "-o", "lstart=", "-p", str(pid)])
    value = proc.stdout.strip() if proc is not None and proc.returncode == 0 else ""
    if value:
        return value
    if not _pid_exists(pid):
        return None
    raise RuntimeError(f"service: cannot read the start time of pid {pid} (ps -o lstart=).")


def _reap(pid: int) -> None:
    """Collect *pid*'s exit status if it is a child of this interpreter."""
    with _started_lock:
        proc = _started.get(pid)
    if proc is not None:
        if proc.poll() is not None:
            with _started_lock:
                _started.pop(pid, None)
        return
    try:
        os.waitpid(pid, os.WNOHANG)
    except (ChildProcessError, OSError):
        pass


def _group_exists(pgid: int) -> bool:
    _reap(pgid)
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


@dataclass(frozen=True)
class Ownership:
    ours: bool
    reason: str


def check_ownership(record: dict[str, Any]) -> Ownership:
    """Whether *record*'s process group still provably is the one this
    plugin started. ``ours=False`` means the record is stale: drop it and
    never signal its pgid."""
    try:
        pgid = int(record["pgid"])
        recorded_start = str(record["start_time"])
        recorded_boot = int(record["boot_time"])
    except (KeyError, TypeError, ValueError):
        return Ownership(False, "the record is incomplete")
    if pgid <= 1:
        return Ownership(False, "the record names no process group")
    if abs(boot_time() - recorded_boot) > _BOOT_TIME_TOLERANCE_SECONDS:
        return Ownership(False, "the machine has rebooted since it was started")
    _reap(pgid)
    start = process_start_time(pgid)
    if start is not None:
        if start != recorded_start:
            return Ownership(
                False,
                f"pid {pgid} now belongs to a different process "
                f"(started {start!r}, recorded {recorded_start!r})",
            )
        return Ownership(True, "its group leader is running")
    if _group_exists(pgid):
        return Ownership(True, "its group leader exited; members of its process group remain")
    return Ownership(False, "its process group no longer exists")


def _describe_holder(pid: int, port: int) -> dict[str, Any]:
    command: str | None = None
    ps = _helper(["ps", "-o", "command=", "-p", str(pid)])
    if ps is not None and ps.returncode == 0 and ps.stdout.strip():
        command = ps.stdout.strip()
    cwd: str | None = None
    if _is_linux_proc():
        try:
            cwd = os.readlink(f"/proc/{pid}/cwd")
        except OSError:
            cwd = None
    if cwd is None and shutil.which("lsof"):
        out = _helper(["lsof", "-a", "-p", str(pid), "-d", "cwd", "-Fn"])
        for line in (out.stdout if out else "").splitlines():
            if line.startswith("n"):
                cwd = line[1:]
                break
    try:
        pgid: int | None = os.getpgid(pid)
    except OSError:
        pgid = None
    return {"port": port, "pid": pid, "pgid": pgid, "command": command, "cwd": cwd}


def _port_accepts(port: int, *, timeout: float = 0.5) -> bool:
    for family, host in ((socket.AF_INET, "127.0.0.1"), (socket.AF_INET6, "::1")):
        try:
            with socket.socket(family, socket.SOCK_STREAM) as sock:
                sock.settimeout(timeout)
                sock.connect((host, port))
                return True
        except OSError:
            continue
    return False


def port_holders(port: int) -> list[dict[str, Any]]:
    """Processes listening on TCP *port*: pid, command and cwd from
    ``lsof`` when it is installed; otherwise (or when ``lsof`` cannot see
    the listener) a connect check, with ``pid``/``command``/``cwd`` null."""
    if shutil.which("lsof"):
        out = _helper(["lsof", "-nP", f"-iTCP:{port}", "-sTCP:LISTEN", "-Fp"])
        pids = sorted(
            {
                int(line[1:])
                for line in (out.stdout if out else "").splitlines()
                if line.startswith("p") and line[1:].isdigit()
            }
        )
        if pids:
            return [_describe_holder(pid, port) for pid in pids]
    if _port_accepts(port):
        return [{"port": port, "pid": None, "pgid": None, "command": None, "cwd": None}]
    return []


def _probe_ready(ready: dict[str, Any]) -> bool:
    if "port" in ready:
        return _port_accepts(int(ready["port"]))
    url = str(ready["url"])
    context = ssl.create_default_context()
    context.check_hostname = False
    context.verify_mode = ssl.CERT_NONE
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}), urllib.request.HTTPSHandler(context=context)
    )
    try:
        with opener.open(url, timeout=2) as response:
            return int(response.status) < 500
    except urllib.error.HTTPError as exc:
        return int(exc.code) < 500
    except (OSError, ValueError):
        return False


def _log_tail(path: Path, offset: int) -> str:
    try:
        with path.open("rb") as fh:
            fh.seek(offset)
            data = fh.read().decode("utf-8", errors="replace")
    except OSError:
        return ""
    tail = "\n".join(data.splitlines()[-_LOG_TAIL_LINES:])
    return tail[-_LOG_TAIL_CHARS:]


def _parse_port(value: Any, *, what: str) -> int:
    try:
        port = int(str(value).strip())
    except (TypeError, ValueError) as exc:
        raise ValueError(f"service: {what} must be a port number, got {value!r}.") from exc
    if not 1 <= port <= 65535:
        raise ValueError(f"service: {what} {port} is out of range (1-65535).")
    return port


def _parse_ms(params: dict[str, Any], key: str, default: int) -> int:
    value = params.get(key)
    if value is None or value == "":
        return default
    try:
        ms = int(str(value).strip())
    except ValueError as exc:
        raise ValueError(f"service: params[{key!r}] must be milliseconds, got {value!r}.") from exc
    if ms < 0:
        raise ValueError(f"service: params[{key!r}] must not be negative.")
    return ms


def _parse_ready(value: Any) -> dict[str, Any] | None:
    if value is None or value == "":
        return None
    text = str(value).strip()
    if "://" in text:
        parsed = urlparse(text)
        if parsed.scheme not in ("http", "https") or not parsed.hostname:
            raise ValueError(f"service: params['ready'] URL must be http(s)://host..., got {text!r}.")
        return {"url": text}
    return {"port": _parse_port(text, what="params['ready']")}


def _ready_port(ready: dict[str, Any] | None) -> int | None:
    """The port a readiness check would be satisfied by — claimed like
    ``ports`` so a foreign listener can never pass for our service."""
    if ready is None:
        return None
    if "port" in ready:
        return int(ready["port"])
    parsed = urlparse(str(ready["url"]))
    if parsed.hostname not in _LOCAL_HOSTS:
        return None
    return parsed.port or (443 if parsed.scheme == "https" else 80)


def _format_holder(holder: dict[str, Any]) -> str:
    port = holder["port"]
    if holder.get("service"):
        return f"port {port} is claimed by service {holder['service']!r} (pgid {holder.get('pgid')})"
    if holder.get("pid") is None:
        return (
            f"port {port} is held by a process this plugin did not start "
            "(install lsof to see its pid, command and cwd)"
        )
    return (
        f"port {port} is held by a process this plugin did not start: pid {holder['pid']}, "
        f"command {holder.get('command') or '?'!r}, cwd {holder.get('cwd') or '?'!r}"
    )


@dataclass(frozen=True)
class ServicePlugin:
    name: str = "service"
    #: ``runtime.plugins.service.state_dir``; ``None`` means
    #: :func:`default_state_dir`.
    state_dir: str | None = None

    # -- state directory -------------------------------------------------

    def _dir(self) -> Path:
        if self.state_dir:
            return Path(self.state_dir).expanduser()
        return default_state_dir()

    @contextmanager
    def _locked(self) -> Iterator[Path]:
        import fcntl

        state_dir = self._dir()
        state_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
        fd = os.open(state_dir / ".lock", os.O_RDWR | os.O_CREAT, 0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX)
            yield state_dir
        finally:
            fcntl.flock(fd, fcntl.LOCK_UN)
            os.close(fd)

    @staticmethod
    def _record_path(state_dir: Path, name: str) -> Path:
        return state_dir / f"{name}.json"

    def _load(self, state_dir: Path, name: str) -> dict[str, Any] | None:
        path = self._record_path(state_dir, name)
        try:
            record = json.loads(path.read_text(encoding="utf-8"))
        except FileNotFoundError:
            return None
        except (OSError, ValueError):
            return {"name": name}  # unreadable: incomplete, so stale
        return record if isinstance(record, dict) else {"name": name}

    def _save(self, state_dir: Path, record: dict[str, Any]) -> None:
        path = self._record_path(state_dir, str(record["name"]))
        tmp = path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(record, indent=2, sort_keys=True), encoding="utf-8")
        os.replace(tmp, path)

    def _drop(self, state_dir: Path, name: str) -> None:
        self._record_path(state_dir, name).unlink(missing_ok=True)

    def _live_record(
        self, state_dir: Path, name: str
    ) -> tuple[dict[str, Any] | None, str | None]:
        """The record for *name* when it still proves ours; otherwise drop
        it and return why it was stale."""
        record = self._load(state_dir, name)
        if record is None:
            return None, None
        ownership = check_ownership(record)
        if ownership.ours:
            return record, None
        self._drop(state_dir, name)
        return None, ownership.reason

    def _other_claims(self, state_dir: Path, name: str) -> dict[int, dict[str, Any]]:
        """Port -> live record of another service of ours that claims it."""
        claims: dict[int, dict[str, Any]] = {}
        for path in sorted(state_dir.glob("*.json")):
            other = path.stem
            if other == name:
                continue
            record, _ = self._live_record(state_dir, other)
            if record is not None:
                for port in record.get("ports") or []:
                    claims[int(port)] = record
        return claims

    # -- the tool --------------------------------------------------------

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        if os.name != "posix":
            raise RuntimeError("service: requires a POSIX system (process groups, flock).")
        action = str(params.get("action") or "").strip().lower()
        if action not in _ACTIONS:
            raise ValueError(
                f"service: params['action'] must be one of {', '.join(_ACTIONS)}, got {action!r}."
            )
        name = str(params.get("name") or "").strip()
        if not _NAME_RE.match(name):
            raise ValueError(
                "service: params['name'] is required: letters, digits, '_', '.' and '-' "
                f"(starting with a letter or digit), got {name!r}."
            )
        if action == "start":
            return self._start(name, params, timeout_seconds=timeout_seconds)
        if action == "stop":
            grace = _parse_ms(params, "grace_ms", _DEFAULT_GRACE_MS) / 1000
            return self._stop(name, grace_seconds=grace)
        return self._status(name)

    def _start(self, name: str, params: dict[str, Any], *, timeout_seconds: int) -> ToolResult:
        shell = _as_bool(params.get("shell"))
        command = params.get("command")
        if shell:
            if not isinstance(command, str) or not command.strip():
                raise ValueError("service: with shell: true, params['command'] must be a string.")
            argv = ["/bin/sh", "-c", command]
        else:
            if (
                not isinstance(command, list)
                or not command
                or not all(isinstance(a, (str, int, float)) for a in command)
            ):
                raise ValueError(
                    "service: params['command'] must be a non-empty argv list "
                    "(or a string with shell: true)."
                )
            argv = [str(a) for a in command]
        if any("\x00" in a for a in argv):
            raise ValueError("service: params['command'] contains a null byte.")

        raw_ports = params.get("ports")
        if raw_ports is None or raw_ports == "":
            raw_ports = []
        if not isinstance(raw_ports, list):
            raw_ports = [raw_ports]
        ports = [_parse_port(p, what="params['ports'] entry") for p in raw_ports]
        ready = _parse_ready(params.get("ready"))
        ready_port = _ready_port(ready)
        if ready_port is not None and ready_port not in ports:
            ports.append(ready_port)
        ready_timeout = min(
            _parse_ms(params, "ready_timeout_ms", _DEFAULT_READY_TIMEOUT_MS) / 1000,
            float(timeout_seconds),
        )
        grace = _parse_ms(params, "grace_ms", _DEFAULT_GRACE_MS) / 1000

        env_param = params.get("env") or {}
        if not isinstance(env_param, dict):
            raise ValueError("service: params['env'] must be a mapping.")
        env = {**os.environ, **{str(k): str(v) for k, v in env_param.items()}}
        cwd_param = params.get("cwd")
        cwd = str(Path(str(cwd_param)).expanduser().resolve()) if cwd_param else os.getcwd()
        if not Path(cwd).is_dir():
            raise ValueError(f"service: params['cwd'] {cwd!r} is not a directory.")

        get_token().check()
        with self._locked() as state_dir:
            log_param = params.get("log")
            log_path = (
                Path(str(log_param)).expanduser().resolve()
                if log_param
                else state_dir / f"{name}.log"
            )
            existing, _ = self._live_record(state_dir, name)
            if existing is not None:
                started = False
                record = existing
                proc: subprocess.Popen[bytes] | None = None
                log_offset = 0
            else:
                claims = self._other_claims(state_dir, name)
                holders: list[dict[str, Any]] = []
                for port in ports:
                    if port in claims:
                        holders.append(
                            {
                                "port": port,
                                "pid": claims[port].get("pgid"),
                                "pgid": claims[port].get("pgid"),
                                "command": claims[port].get("command"),
                                "cwd": claims[port].get("cwd"),
                                "service": claims[port].get("name"),
                            }
                        )
                    else:
                        holders.extend(port_holders(port))
                if holders:
                    detail = "; ".join(_format_holder(h) for h in holders)
                    message = (
                        f"service {name!r}: not started — {detail}. Nothing was "
                        "signalled; stop that process yourself or pick another port."
                    )
                    return ToolResult(
                        value=None,
                        raw={"action": "start", "name": name, "port_holders": holders},
                        stderr=message,
                        ok=False,
                    )
                log_path.parent.mkdir(parents=True, exist_ok=True)
                with log_path.open("ab") as log_fh:
                    log_offset = log_fh.tell()
                    try:
                        proc = subprocess.Popen(
                            argv,
                            cwd=cwd,
                            env=env,
                            stdin=subprocess.DEVNULL,
                            stdout=log_fh,
                            stderr=subprocess.STDOUT,
                            start_new_session=True,
                        )
                    except OSError as exc:
                        raise RuntimeError(f"service {name!r}: cannot start {argv[0]!r}: {exc}") from exc
                with _started_lock:
                    _started[proc.pid] = proc
                started = True
                try:
                    start_time = process_start_time(proc.pid)
                    if start_time is None:
                        raise RuntimeError(f"pid {proc.pid} vanished before it could be recorded")
                    record = {
                        "name": name,
                        "pgid": proc.pid,
                        "start_time": start_time,
                        "boot_time": boot_time(),
                        "ports": ports,
                        "log": str(log_path),
                        "command": argv,
                        "cwd": cwd,
                        "ready": ready,
                        "started_at": datetime.now(timezone.utc).isoformat(),
                    }
                    self._save(state_dir, record)
                except BaseException:
                    self._terminate_group(proc.pid, grace_seconds=grace)
                    raise

        try:
            outcome = self._wait_ready(
                record, proc, ready if started else record.get("ready"), timeout=ready_timeout
            )
        except BaseException:
            # Cancelled (Ctrl-C/SIGTERM) while waiting: stop what we just started.
            if started:
                self._stop(name, grace_seconds=grace)
            raise
        value = {
            "name": name,
            "running": True,
            "ready": outcome is None,
            "already_running": not started,
            "pgid": record["pgid"],
            "ports": record.get("ports") or [],
            "log": record.get("log"),
            "started_at": record.get("started_at"),
        }
        if outcome is None:
            return ToolResult(value=value, raw={"action": "start", **value})
        tail = _log_tail(Path(str(record.get("log"))), log_offset)
        if started:
            self._stop(name, grace_seconds=grace)
            value["running"] = False
        message = f"service {name!r}: {outcome}" + (
            f"\n--- log tail ({record.get('log')}) ---\n{tail}" if tail else ""
        )
        return ToolResult(
            value=None,
            raw={"action": "start", **value, "error": outcome, "log_tail": tail},
            stderr=message,
            ok=False,
        )

    def _wait_ready(
        self,
        record: dict[str, Any],
        proc: subprocess.Popen[bytes] | None,
        ready: dict[str, Any] | None,
        *,
        timeout: float,
    ) -> str | None:
        """``None`` once ready; otherwise why it never got there. *proc* is
        ``None`` for a service an earlier call started."""
        token = get_token()
        deadline = time.monotonic() + timeout
        pgid = int(record["pgid"])
        while True:
            if proc is not None and proc.poll() is not None:
                return f"exited with code {proc.returncode} before it was ready"
            if proc is None and not _group_exists(pgid):
                return "exited before it was ready"
            if ready is None or _probe_ready(ready):
                return None
            if time.monotonic() >= deadline:
                target = ready.get("url") or f"port {ready.get('port')}"
                return f"not ready ({target}) within {timeout:g}s"
            token.sleep_or_raise(_POLL_SECONDS * 2)

    def _terminate_group(self, pgid: int, *, grace_seconds: float) -> tuple[bool, str | None]:
        """SIGTERM *pgid*'s group, wait up to *grace_seconds*, then SIGKILL
        what is left. Returns (gone, last signal sent)."""
        sent: str | None = None
        try:
            os.killpg(pgid, signal.SIGTERM)
            sent = "SIGTERM"
        except ProcessLookupError:
            return True, None
        deadline = time.monotonic() + grace_seconds
        while time.monotonic() < deadline:
            if not _group_exists(pgid):
                return True, sent
            time.sleep(_POLL_SECONDS)
        if not _group_exists(pgid):
            return True, sent
        try:
            os.killpg(pgid, signal.SIGKILL)
            sent = "SIGKILL"
        except ProcessLookupError:
            return True, sent
        deadline = time.monotonic() + _SETTLE_SECONDS
        while time.monotonic() < deadline:
            if not _group_exists(pgid):
                return True, sent
            time.sleep(_POLL_SECONDS)
        return not _group_exists(pgid), sent

    def _stop(self, name: str, *, grace_seconds: float) -> ToolResult:
        with self._locked() as state_dir:
            record, stale_reason = self._live_record(state_dir, name)
            if record is None:
                value: dict[str, Any] = {
                    "name": name,
                    "stopped": False,
                    "running": False,
                    "signal": None,
                    "ports_free": True,
                    "port_holders": [],
                    "stale_record": stale_reason,
                }
                return ToolResult(value=value, raw={"action": "stop", **value})
            pgid = int(record["pgid"])
            gone, sent = self._terminate_group(pgid, grace_seconds=grace_seconds)
            if not gone:
                message = f"service {name!r}: process group {pgid} survived SIGKILL."
                return ToolResult(
                    value=None,
                    raw={"action": "stop", "name": name, "pgid": pgid, "signal": sent},
                    stderr=message,
                    ok=False,
                )
            self._drop(state_dir, name)
        ports = [int(p) for p in record.get("ports") or []]
        holders: list[dict[str, Any]] = []
        deadline = time.monotonic() + _SETTLE_SECONDS
        while True:
            holders = [h for port in ports for h in port_holders(port)]
            if not holders or time.monotonic() >= deadline:
                break
            time.sleep(_POLL_SECONDS)
        value = {
            "name": name,
            "stopped": True,
            "running": False,
            "pgid": pgid,
            "signal": sent,
            "ports_free": not holders,
            "port_holders": holders,
            "stale_record": None,
        }
        return ToolResult(value=value, raw={"action": "stop", **value})

    def _status(self, name: str) -> ToolResult:
        with self._locked() as state_dir:
            record, stale_reason = self._live_record(state_dir, name)
        ports = [int(p) for p in (record or {}).get("ports") or []]
        holders = [h for port in ports for h in port_holders(port)]
        pgid = int(record["pgid"]) if record else None
        for holder in holders:
            holder["ours"] = pgid is not None and holder.get("pgid") == pgid
        ready_spec = (record or {}).get("ready")
        ready: bool | None = False
        if record is not None:
            # No readiness check was given at start: nothing to report.
            ready = _probe_ready(ready_spec) if ready_spec else None
        value = {
            "name": name,
            "running": record is not None,
            "ready": ready,
            "pgid": pgid,
            "ports": ports,
            "log": (record or {}).get("log"),
            "started_at": (record or {}).get("started_at"),
            "port_holders": holders,
            "stale_record": stale_reason,
        }
        return ToolResult(value=value, raw={"action": "status", **value})

    def check(self) -> CheckResult:
        if os.name != "posix":
            return CheckResult(ok=False, missing=[], message="service requires a POSIX system.")
        if not _is_linux_proc() and shutil.which("ps") is None:
            return CheckResult(ok=False, missing=["binary:ps"])
        if shutil.which("lsof") is None:
            return CheckResult(
                ok=True,
                missing=[],
                message=(
                    "lsof not found: a port held by another process is still detected, "
                    "but without its pid, command and cwd."
                ),
            )
        return CheckResult(ok=True, missing=[])
