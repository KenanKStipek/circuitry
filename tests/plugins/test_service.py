"""The ``service`` tool plugin (#368): start, own and stop long-running
background processes.

Every test runs throwaway local HTTP servers (``python -m http.server``) on
free ports and a per-test state directory; a process a test starts on its
own (a "foreign" one the plugin must never touch) is killed by the test's
own teardown, and so is any process group a record still names.
"""

from __future__ import annotations

import json
import os
import shutil
import signal
import socket
import ssl
import subprocess
import sys
import textwrap
import threading
import time
import urllib.request
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest

from circuitry.core.cancellation import RunCancelledBySignal, get_token
from circuitry.plugins import service as service_mod
from circuitry.plugins.capabilities import NETWORK, SHELL, capabilities_of
from circuitry.plugins.factory import build_plugin
from circuitry.plugins.service import ServicePlugin, boot_time, process_start_time

pytestmark = pytest.mark.skipif(os.name != "posix", reason="service is POSIX only")

HAS_LSOF = shutil.which("lsof") is not None


def _free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def _http_server(port: int) -> list[str]:
    return [sys.executable, "-m", "http.server", str(port), "--bind", "127.0.0.1"]


def _group_alive(pgid: int) -> bool:
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    return True


def _pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def _wait_until(predicate: Any, timeout: float = 10.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.05)
    return bool(predicate())


def _port_open(port: int) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.5):
            return True
    except OSError:
        return False


@pytest.fixture
def state_dir(tmp_path: Path) -> Iterator[Path]:
    path = tmp_path / "state"
    yield path
    # Safety net: never leave a process group a record still names running.
    for record_path in path.glob("*.json"):
        try:
            pgid = int(json.loads(record_path.read_text())["pgid"])
            os.killpg(pgid, signal.SIGKILL)
        except (OSError, ValueError, KeyError):
            pass


@pytest.fixture
def plugin(state_dir: Path) -> ServicePlugin:
    return ServicePlugin(state_dir=str(state_dir))


@pytest.fixture
def foreign(tmp_path: Path) -> Iterator[list[subprocess.Popen[bytes]]]:
    """Processes the test starts itself — the plugin must never signal them."""
    procs: list[subprocess.Popen[bytes]] = []
    yield procs
    for proc in procs:
        if proc.poll() is None:
            os.killpg(proc.pid, signal.SIGKILL)
        proc.wait(timeout=10)


def _start_foreign(
    procs: list[subprocess.Popen[bytes]], argv: list[str], cwd: Path
) -> subprocess.Popen[bytes]:
    cwd.mkdir(parents=True, exist_ok=True)
    proc = subprocess.Popen(
        argv,
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,  # its own group leader: a killpg would reach it
    )
    procs.append(proc)
    return proc


def _start(plugin: ServicePlugin, name: str, port: int, **extra: Any) -> Any:
    params = {
        "action": "start",
        "name": name,
        "command": _http_server(port),
        "ports": [port],
        "ready": f"http://127.0.0.1:{port}/",
        "ready_timeout_ms": 20000,
        **extra,
    }
    return plugin.execute(params=params)


def test_start_ready_status_stop_against_a_local_http_server(
    plugin: ServicePlugin, state_dir: Path, tmp_path: Path
) -> None:
    port = _free_port()
    log = tmp_path / "logs" / "web.log"

    started = _start(plugin, "web", port, log=str(log))

    assert started.ok, started.stderr
    assert started.value["ready"] is True
    assert started.value["already_running"] is False
    pgid = started.value["pgid"]
    assert _group_alive(pgid)
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/", timeout=5) as response:
        assert response.status == 200
    record = json.loads((state_dir / "web.json").read_text())
    assert record["pgid"] == pgid
    assert record["start_time"] == process_start_time(pgid)
    assert abs(record["boot_time"] - boot_time()) <= 5
    assert record["ports"] == [port]
    assert record["log"] == str(log.resolve())
    assert log.exists()

    status = plugin.execute(params={"action": "status", "name": "web"})
    assert status.ok
    assert status.value["running"] is True
    assert status.value["ready"] is True
    assert status.value["port_holders"], "the server's own port must show as held"
    if HAS_LSOF:
        assert all(h["ours"] for h in status.value["port_holders"])

    stopped = plugin.execute(params={"action": "stop", "name": "web"})
    assert stopped.ok, stopped.stderr
    assert stopped.value["stopped"] is True
    assert stopped.value["ports_free"] is True
    assert not _group_alive(pgid)
    assert not _port_open(port)
    assert not (state_dir / "web.json").exists()

    # Idempotent: stopping again, and asking for status, are not errors.
    again = plugin.execute(params={"action": "stop", "name": "web"})
    assert again.ok and again.value["stopped"] is False
    status = plugin.execute(params={"action": "status", "name": "web"})
    assert status.value["running"] is False and status.value["ready"] is False


def test_start_of_a_running_service_leaves_it_running(plugin: ServicePlugin) -> None:
    port = _free_port()
    first = _start(plugin, "web", port)
    assert first.ok, first.stderr

    second = _start(plugin, "web", port)

    assert second.ok, second.stderr
    assert second.value["already_running"] is True
    assert second.value["pgid"] == first.value["pgid"]
    assert plugin.execute(params={"action": "stop", "name": "web"}).value["stopped"]


def test_concurrent_starts_of_one_name_start_one_process(plugin: ServicePlugin) -> None:
    port = _free_port()
    results: list[Any] = []

    def _go() -> None:
        results.append(_start(plugin, "web", port))

    threads = [threading.Thread(target=_go) for _ in range(2)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=60)

    assert all(r.ok for r in results), [r.stderr for r in results]
    assert len({r.value["pgid"] for r in results}) == 1
    assert sorted(r.value["already_running"] for r in results) == [False, True]
    plugin.execute(params={"action": "stop", "name": "web"})


def test_a_port_held_by_a_foreign_process_fails_start_and_leaves_it_running(
    plugin: ServicePlugin,
    state_dir: Path,
    tmp_path: Path,
    foreign: list[subprocess.Popen[bytes]],
) -> None:
    port = _free_port()
    holder_cwd = tmp_path / "somebody-elses-project"
    holder = _start_foreign(foreign, _http_server(port), holder_cwd)
    assert _wait_until(lambda: _port_open(port))

    result = _start(plugin, "web", port)

    assert result.ok is False
    assert "did not start" in (result.stderr or "")
    holders = result.raw["port_holders"]
    assert [h["port"] for h in holders] == [port]
    if HAS_LSOF:
        assert holders[0]["pid"] == holder.pid
        assert "http.server" in holders[0]["command"]
        assert Path(holders[0]["cwd"]).resolve() == holder_cwd.resolve()
        assert str(holder.pid) in result.stderr
    assert not (state_dir / "web.json").exists()

    # Nothing of ours to stop either: the holder is never signalled.
    assert plugin.execute(params={"action": "stop", "name": "web"}).value["stopped"] is False
    time.sleep(0.3)
    assert holder.poll() is None
    assert _port_open(port)


def test_a_port_claimed_by_another_service_fails_start(plugin: ServicePlugin) -> None:
    port = _free_port()
    assert _start(plugin, "web", port).ok

    result = plugin.execute(
        params={"action": "start", "name": "web2", "command": ["sleep", "60"], "ports": [port]}
    )

    assert result.ok is False
    assert "service 'web'" in (result.stderr or "")
    plugin.execute(params={"action": "stop", "name": "web"})


def _write_record(state_dir: Path, name: str, **fields: Any) -> Path:
    state_dir.mkdir(parents=True, exist_ok=True)
    path = state_dir / f"{name}.json"
    path.write_text(json.dumps({"name": name, "ports": [], **fields}))
    return path


def test_a_stale_record_is_dropped_without_error(
    plugin: ServicePlugin, state_dir: Path
) -> None:
    gone = subprocess.Popen(["true"], start_new_session=True)
    start_time = process_start_time(gone.pid)
    gone.wait(timeout=10)
    path = _write_record(
        state_dir, "web", pgid=gone.pid, start_time=start_time, boot_time=boot_time()
    )

    status = plugin.execute(params={"action": "status", "name": "web"})

    assert status.ok
    assert status.value["running"] is False
    assert "no longer exists" in status.value["stale_record"]
    assert not path.exists()

    _write_record(state_dir, "web", pgid=gone.pid, start_time=start_time, boot_time=boot_time())
    stopped = plugin.execute(params={"action": "stop", "name": "web"})
    assert stopped.ok and stopped.value["stopped"] is False
    assert not path.exists()

    # And a start over a stale record simply starts.
    _write_record(state_dir, "web", pgid=gone.pid, start_time=start_time, boot_time=boot_time())
    port = _free_port()
    started = _start(plugin, "web", port)
    assert started.ok and started.value["already_running"] is False
    plugin.execute(params={"action": "stop", "name": "web"})


def test_a_record_whose_pid_now_belongs_to_another_process_is_never_signalled(
    plugin: ServicePlugin,
    state_dir: Path,
    tmp_path: Path,
    foreign: list[subprocess.Popen[bytes]],
) -> None:
    unrelated = _start_foreign(foreign, ["sleep", "60"], tmp_path / "unrelated")
    path = _write_record(
        state_dir,
        "web",
        pgid=unrelated.pid,
        start_time="Thu Jan  1 00:00:00 1970",
        boot_time=boot_time(),
    )

    stopped = plugin.execute(params={"action": "stop", "name": "web"})

    assert stopped.ok
    assert stopped.value["stopped"] is False
    assert "different process" in stopped.value["stale_record"]
    assert not path.exists()
    time.sleep(0.3)
    assert unrelated.poll() is None, "a process the plugin did not start was signalled"

    # status and start treat it the same way.
    _write_record(
        state_dir, "web", pgid=unrelated.pid, start_time="x", boot_time=boot_time()
    )
    assert plugin.execute(params={"action": "status", "name": "web"}).value["running"] is False
    assert unrelated.poll() is None


def test_a_record_from_before_a_reboot_is_never_signalled(
    plugin: ServicePlugin,
    state_dir: Path,
    tmp_path: Path,
    foreign: list[subprocess.Popen[bytes]],
) -> None:
    proc = _start_foreign(foreign, ["sleep", "60"], tmp_path / "proc")
    path = _write_record(
        state_dir,
        "web",
        pgid=proc.pid,
        start_time=process_start_time(proc.pid),
        boot_time=boot_time() - 86_400,
    )

    stopped = plugin.execute(params={"action": "stop", "name": "web"})

    assert stopped.value["stopped"] is False
    assert "rebooted" in stopped.value["stale_record"]
    assert not path.exists()
    time.sleep(0.3)
    assert proc.poll() is None


_STUBBORN_CHILD = textwrap.dedent(
    """
    import signal, subprocess, sys, time
    child = subprocess.Popen([sys.executable, "-c",
        "import signal, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(600)"])
    with open(sys.argv[1], "w") as fh:
        fh.write(str(child.pid))
    import http.server, socketserver
    http.server.test(HandlerClass=http.server.SimpleHTTPRequestHandler,
                     port=int(sys.argv[2]), bind="127.0.0.1")
    """
)


def test_stop_takes_down_the_whole_group_including_a_child_that_ignores_sigterm(
    plugin: ServicePlugin, tmp_path: Path
) -> None:
    port = _free_port()
    script = tmp_path / "stubborn.py"
    script.write_text(_STUBBORN_CHILD)
    child_pid_file = tmp_path / "child.pid"

    started = plugin.execute(
        params={
            "action": "start",
            "name": "stubborn",
            "command": [sys.executable, str(script), str(child_pid_file), str(port)],
            "ports": [port],
            "ready": port,
            "ready_timeout_ms": 20000,
        }
    )
    assert started.ok, started.stderr
    assert _wait_until(child_pid_file.exists)
    child_pid = int(child_pid_file.read_text())
    assert os.getpgid(child_pid) == started.value["pgid"]

    stopped = plugin.execute(params={"action": "stop", "name": "stubborn", "grace_ms": 500})

    assert stopped.ok, stopped.stderr
    assert stopped.value["signal"] == "SIGKILL"
    assert not _group_alive(started.value["pgid"])
    assert _wait_until(lambda: not _pid_alive(child_pid))
    assert stopped.value["ports_free"] is True


def test_start_fails_with_the_log_tail_when_the_process_exits_before_ready(
    plugin: ServicePlugin, state_dir: Path
) -> None:
    port = _free_port()

    result = plugin.execute(
        params={
            "action": "start",
            "name": "crashy",
            "shell": True,
            "command": "echo starting up; echo 'bind: address in use' >&2; exit 3",
            "ready": port,
        }
    )

    assert result.ok is False
    assert "exited with code 3 before it was ready" in result.stderr
    assert "bind: address in use" in result.stderr
    assert "starting up" in result.raw["log_tail"]
    assert not (state_dir / "crashy.json").exists()


def test_start_that_never_gets_ready_is_stopped_again(
    plugin: ServicePlugin, state_dir: Path
) -> None:
    result = plugin.execute(
        params={
            "action": "start",
            "name": "slow",
            "command": ["sleep", "60"],
            "ready": _free_port(),
            "ready_timeout_ms": 500,
        }
    )

    assert result.ok is False
    assert "not ready" in result.stderr
    assert not _group_alive(result.raw["pgid"])
    assert not (state_dir / "slow.json").exists()


def test_cancelling_while_waiting_for_readiness_stops_the_started_group(
    plugin: ServicePlugin, state_dir: Path
) -> None:
    token = get_token()
    timer = threading.Timer(0.5, token.request)
    started_pgids: list[int] = []
    real_save = ServicePlugin._save

    def _spy_save(self: ServicePlugin, directory: Path, record: dict[str, Any]) -> None:
        started_pgids.append(int(record["pgid"]))
        real_save(self, directory, record)

    try:
        with pytest.MonkeyPatch.context() as mp:
            mp.setattr(ServicePlugin, "_save", _spy_save)
            timer.start()
            with pytest.raises(RunCancelledBySignal):
                plugin.execute(
                    params={
                        "action": "start",
                        "name": "never",
                        "command": ["sleep", "60"],
                        "ready": _free_port(),
                        "ready_timeout_ms": 30000,
                    }
                )
    finally:
        timer.cancel()
        token.reset()

    assert started_pgids
    assert not _group_alive(started_pgids[0])
    assert not (state_dir / "never.json").exists()


@pytest.mark.skipif(shutil.which("openssl") is None, reason="requires openssl")
def test_https_readiness_accepts_a_self_signed_local_certificate(
    plugin: ServicePlugin, tmp_path: Path
) -> None:
    cert, key = tmp_path / "cert.pem", tmp_path / "key.pem"
    subprocess.run(
        ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
         "-subj", "/CN=localhost", "-keyout", str(key), "-out", str(cert)],
        check=True,
        capture_output=True,
    )
    script = tmp_path / "tls_server.py"
    script.write_text(
        textwrap.dedent(
            """
            import http.server, ssl, sys
            server = http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])),
                                            http.server.SimpleHTTPRequestHandler)
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.load_cert_chain(sys.argv[2], sys.argv[3])
            server.socket = context.wrap_socket(server.socket, server_side=True)
            server.serve_forever()
            """
        )
    )
    port = _free_port()

    result = plugin.execute(
        params={
            "action": "start",
            "name": "tls",
            "command": [sys.executable, str(script), str(port), str(cert), str(key)],
            "ready": f"https://localhost:{port}/",
            "ready_timeout_ms": 20000,
        }
    )

    assert result.ok, result.stderr
    assert result.value["ports"] == [port]  # a localhost ready URL claims its port
    with pytest.raises(OSError):  # the certificate really is untrusted
        urllib.request.urlopen(
            f"https://localhost:{port}/", timeout=5, context=ssl.create_default_context()
        )
    plugin.execute(params={"action": "stop", "name": "tls"})


@pytest.mark.parametrize(
    ("params", "match"),
    [
        ({"action": "restart", "name": "web"}, "action"),
        ({"action": "status", "name": "../etc"}, "name"),
        ({"action": "start", "name": "web", "command": "python -m http.server"}, "argv list"),
        ({"action": "start", "name": "web", "shell": True, "command": ["ls"]}, "string"),
        ({"action": "start", "name": "web", "command": ["true"], "ports": [70000]}, "range"),
        ({"action": "start", "name": "web", "command": ["true"], "ready": "ftp://x/"}, "URL"),
        ({"action": "start", "name": "web", "command": ["true"], "env": ["A=1"]}, "env"),
    ],
)
def test_invalid_params_are_rejected_before_anything_runs(
    plugin: ServicePlugin, state_dir: Path, params: dict[str, Any], match: str
) -> None:
    with pytest.raises(ValueError, match=match):
        plugin.execute(params=params)
    assert not list(state_dir.glob("*.json"))


def test_state_dir_defaults_beside_the_global_config_and_is_configurable(
    tmp_path: Path,
) -> None:
    from circuitry.cli import config as config_module

    default = build_plugin(plugin_name="service", runtime={})
    assert isinstance(default, ServicePlugin)
    assert default._dir() == Path(config_module.GLOBAL_CONFIG_DIR) / "services"
    assert service_mod.default_state_dir() == default._dir()

    configured = build_plugin(
        plugin_name="service",
        runtime={"plugins": {"service": {"state_dir": str(tmp_path / "svc")}}},
    )
    assert isinstance(configured, ServicePlugin)
    assert configured._dir() == tmp_path / "svc"


def test_service_needs_shell_and_network() -> None:
    assert capabilities_of("service") == {SHELL, NETWORK}


# -- stopping at the end of a run: `finally:` -------------------------------


def _finally_orchestration(tmp_path: Path, port: int, middle: str) -> Path:
    middle_steps = {
        "succeed": "      - {type: tool, name: middle, provider: uuid}\n",
        "fail": (
            "      - type: tool\n"
            "        name: middle\n"
            "        provider: shell\n"
            "        params: {command: 'false', allowed_commands: ['false']}\n"
        ),
        "block": (
            "      - type: tool\n"
            "        name: middle\n"
            "        provider: shell\n"
            "        params: {command: sleep, args: ['30'], allowed_commands: [sleep]}\n"
        ),
    }[middle]
    orch = tmp_path / f"with_web_{middle}.yml"
    orch.write_text(
        "effects:\n"
        "  - type: dynamic\n"
        "    name: with_web\n"
        "    effects:\n"
        "      - type: tool\n"
        "        name: start_web\n"
        "        provider: service\n"
        "        params:\n"
        "          action: start\n"
        "          name: web\n"
        f"          command: {json.dumps(_http_server(port))}\n"
        f"          ports: [{port}]\n"
        f"          ready: 'http://127.0.0.1:{port}/'\n"
        + middle_steps
        + "    finally:\n"
        "      - type: tool\n"
        "        name: stop_web\n"
        "        provider: service\n"
        "        params: {action: stop, name: web}\n",
        encoding="utf-8",
    )
    return orch


@pytest.fixture
def started_groups() -> Iterator[list[int]]:
    """Process groups a whole-run test started — killed if a test left one."""
    pgids: list[int] = []
    yield pgids
    for pgid in pgids:
        try:
            os.killpg(pgid, signal.SIGKILL)
        except OSError:
            pass


@pytest.mark.parametrize("middle", ["succeed", "fail"])
def test_finally_stop_runs_on_success_and_on_failure(
    tmp_path: Path, middle: str, started_groups: list[int]
) -> None:
    from circuitry.cli.runtime_shim import RunRequest, run

    port = _free_port()
    orch = _finally_orchestration(tmp_path, port, middle)

    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
        )
    )

    node = result.state["prime"]["with_web"]
    pgid = node["start_web"]["value"]["pgid"]
    started_groups.append(pgid)
    assert result.ok is (middle == "succeed"), result.error
    assert node["stop_web"]["value"]["stopped"] is True
    assert not _group_alive(pgid)
    assert not _port_open(port)


@pytest.mark.skipif(shutil.which("sleep") is None, reason="requires the 'sleep' binary")
def test_finally_stop_runs_on_ctrl_c(tmp_path: Path, started_groups: list[int]) -> None:
    port = _free_port()
    orch = _finally_orchestration(tmp_path, port, "block")
    home = tmp_path / "home"
    home.mkdir()
    out_path, live_path = tmp_path / "out.json", tmp_path / "live.json"
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in ("OPENAI_API_KEY", "CYBERDINER_TOKEN", "CYBERDINER_EXPO_URL")
    }
    proc = subprocess.Popen(
        [
            sys.executable, "-m", "circuitry.cli.app", "run", str(orch),
            "--out", str(out_path), "--live-state", str(live_path), "--quiet",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env={**env, "HOME": str(home)},
        cwd=tmp_path,
    )

    def _service_started() -> bool:
        try:
            state = json.loads(live_path.read_text(encoding="utf-8"))
            value = state["prime"]["with_web"]["start_web"]["value"]
        except (OSError, ValueError, KeyError, TypeError):
            return False
        started_groups.append(value["pgid"])
        return True

    try:
        assert _wait_until(_service_started, timeout=30), "the service never started"
        time.sleep(0.3)  # let the blocking step start
        proc.send_signal(signal.SIGINT)
        stdout, stderr = proc.communicate(timeout=30)
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.communicate(timeout=10)

    assert proc.returncode == 130, (stdout, stderr)
    state = json.loads(out_path.read_text(encoding="utf-8"))
    assert state["prime"]["with_web"]["stop_web"]["value"]["stopped"] is True
    assert not _group_alive(started_groups[-1])
    assert not _port_open(port)
    # The default state directory is per-user, under the (temporary) HOME.
    assert (home / ".config" / "circuitry" / "services").is_dir()
    assert not (home / ".config" / "circuitry" / "services" / "web.json").exists()
