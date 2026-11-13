"""`cof run` and `electricity run` dispatch the `service` tool the same way
(issue #721): start/status/stop of a local HTTP server, a ready timeout, a
port another process holds (never signalled), a stale record naming a
reused pid or a different boot (never signalled), a dead leader whose group
still has members (still ours), a start cancelled while waiting for ready
(torn down gracefully), parallel start/stop branches in one run (no
deadlock), and a cross-engine stop (a service one engine starts, the other
stops).

Not a `tests/conformance/cases/` case: a service's own record carries a
pid, a start time and a log path, none of which a generated expected-state
fixture can pin -- same rationale `test_run_cli_parity.py`'s own module
doc gives for the same shape of test. Follows that module's own harness
pattern directly (`_sandboxed_env`, `electricity_binary`) rather than
`tests/conformance/harness.py`'s case-oriented `run_case`.

Every subprocess this module starts for a service's own `command` is
bounded (#661): it exits once reparented or after a short, fixed deadline,
and every test confirms nothing it owns outlives the test itself.
"""

from __future__ import annotations

import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import pytest

ELECTRICITY_DIR = Path(__file__).resolve().parents[2] / "electricity"

_CREDENTIAL_ENV_VARS = (
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "CYBERDINER_TOKEN",
    "CYBERDINER_EXPO_URL",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "NPM_TOKEN",
)


def _real_state_dir_snapshot() -> frozenset[str]:
    """The real `~/.config/circuitry/services`, never the fake one
    `tests/conftest.py`'s own autouse fixture points `GLOBAL_CONFIG_DIR`
    at for this pytest process -- `Path.home()` itself is never
    monkeypatched, only that already-computed module attribute is."""
    real_dir = Path.home() / ".config" / "circuitry" / "services"
    if not real_dir.is_dir():
        return frozenset()
    return frozenset(p.name for p in real_dir.iterdir())


@pytest.fixture(autouse=True)
def _guard_against_the_real_state_directory() -> Any:
    """Every test below passes its own `state_dir` (`_write_config`) and
    a sandboxed `$HOME` (`_sandboxed_env`) to every `cof`/`electricity`
    subprocess it starts -- the real default state directory may hold
    records for orchestrations actually running on this machine right
    now, and must never be read, written or signalled from here. This
    fails the test outright, rather than silently touching it, the
    moment anything there changes while it runs."""
    before = _real_state_dir_snapshot()
    yield
    after = _real_state_dir_snapshot()
    assert after == before, (
        "a test in this module touched the real state directory "
        f"~/.config/circuitry/services: before={sorted(before)} after={sorted(after)}"
    )


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def _sandboxed_env(home_dir: Path, extra: dict[str, str] | None = None) -> dict[str, str]:
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in _CREDENTIAL_ENV_VARS and not k.startswith("CIRCUITRY_")
    }
    env["HOME"] = str(home_dir)
    env.update(extra or {})
    return env


def _write_config(config_path: Path, state_dir: Path) -> None:
    config_path.write_text(
        json.dumps({"runtime": {"plugins": {"service": {"state_dir": str(state_dir)}}}}),
        encoding="utf-8",
    )


def _write_orchestration(doc_path: Path, params: dict, *, on_error: str | None = None) -> None:
    effect = {
        "name": "svc",
        "type": "tool",
        "provider": "service",
        "params": params,
    }
    if on_error is not None:
        effect["on_error"] = on_error
    doc_path.write_text(json.dumps({"effects": [effect]}), encoding="utf-8")


def _run_cof(
    doc_path: Path, config_path: Path, home_dir: Path, *, env: dict[str, str] | None = None,
    timeout: float = 30,
) -> subprocess.CompletedProcess[str]:
    cmd = [sys.executable, "-m", "circuitry.cli.app", "run", str(doc_path), "--config", str(config_path)]
    return subprocess.run(
        cmd, cwd=doc_path.parent, env=_sandboxed_env(home_dir, env), capture_output=True, text=True,
        timeout=timeout, check=False,
    )


def _run_electricity(
    binary: Path, doc_path: Path, config_path: Path, home_dir: Path, *,
    env: dict[str, str] | None = None, timeout: float = 30,
) -> subprocess.CompletedProcess[str]:
    cmd = [str(binary), "run", str(doc_path), "--config", str(config_path)]
    return subprocess.run(
        cmd, cwd=doc_path.parent, env=_sandboxed_env(home_dir, env), capture_output=True, text=True,
        timeout=timeout, check=False,
    )


def _svc_value(result: subprocess.CompletedProcess[str]) -> dict:
    return json.loads(result.stdout)["prime"]["svc"]["value"]


def _svc_node(result: subprocess.CompletedProcess[str]) -> dict:
    return json.loads(result.stdout)["prime"]["svc"]


#: Keys a service's own record (and so its tool result) carries that a
#: pid, a start time or a temp path make inherently different between
#: two separate runs -- scrubbed before comparing one engine's output
#: against the other's, so what's left is everything that must match.
_VOLATILE_KEYS = frozenset(
    {"pgid", "pid", "log", "started_at", "log_tail", "created_at", "completed_at", "cwd"}
)


def _scrub(node: Any) -> Any:
    if isinstance(node, dict):
        return {k: ("<scrubbed>" if k in _VOLATILE_KEYS else _scrub(v)) for k, v in node.items()}
    if isinstance(node, list):
        return [_scrub(v) for v in node]
    return node


def _assert_parity(cof_node: dict, electricity_node: dict) -> None:
    """Both engines' own `value` and `meta.raw`, scrubbed of whatever a
    pid/timestamp/temp path makes inherently different between two
    separate runs, must otherwise be identical."""
    assert _scrub(cof_node["value"]) == _scrub(electricity_node["value"]), (
        cof_node["value"],
        electricity_node["value"],
    )
    cof_raw = cof_node["meta"].get("raw")
    electricity_raw = electricity_node["meta"].get("raw")
    assert _scrub(cof_raw) == _scrub(electricity_raw), (cof_raw, electricity_raw)


@pytest.fixture(scope="module")
def electricity_binary() -> Path:
    if shutil.which("cargo") is None:
        pytest.skip("cargo not available; electricity binary cannot be built")
    from . import harness

    proc = subprocess.run(
        ["cargo", "build", "-p", "electricity-cli", "--bin", "electricity"],
        cwd=ELECTRICITY_DIR, env=harness.cargo_env(), capture_output=True, text=True,
        timeout=600, check=False,
    )
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p electricity-cli failed:\n{proc.stdout}\n{proc.stderr}")
    binary = ELECTRICITY_DIR / "target" / "debug" / "electricity"
    assert binary.exists(), f"cargo build succeeded but {binary} is missing"
    return binary


def _pgid_exists(pgid: int) -> bool:
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _http_server_command(port: int) -> list:
    script = Path(__file__).resolve().parent / "fakes" / "service_http_server.py"
    return [sys.executable, str(script), str(port)]


def _stop_and_wait_free(pgid: int, *, timeout: float = 5.0) -> None:
    if not _pgid_exists(pgid):
        return
    try:
        os.killpg(pgid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass
    deadline = time.monotonic() + timeout
    while _pgid_exists(pgid) and time.monotonic() < deadline:
        time.sleep(0.1)


def test_start_status_and_stop_of_a_local_http_server_match_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    port = _free_port()
    params = {
        "action": "start",
        "name": "web",
        "command": _http_server_command(port),
        "ports": [port],
        "ready": f"http://127.0.0.1:{port}/",
        "ready_timeout_ms": 5000,
    }

    nodes: dict[str, dict] = {}
    for engine, runner in (
        ("cof", lambda doc, cfg: _run_cof(doc, cfg, home_dir)),
        (
            "electricity",
            lambda doc, cfg: _run_electricity(electricity_binary, doc, cfg, home_dir),
        ),
    ):
        case_dir = tmp_path / engine
        case_dir.mkdir()
        state_dir = case_dir / "state"
        doc_path = case_dir / "orchestration.json"
        config_path = case_dir / "config.json"
        _write_config(config_path, state_dir)

        _write_orchestration(doc_path, params)
        start_result = runner(doc_path, config_path)
        assert start_result.returncode == 0, start_result.stderr
        start_value = _svc_value(start_result)
        pgid = start_value["pgid"]
        try:
            assert start_value["running"] is True
            assert start_value["ready"] is True
            assert start_value["already_running"] is False
            assert _pgid_exists(pgid), f"{engine}: pgid {pgid} is not running after start"

            _write_orchestration(doc_path, {"action": "status", "name": "web"})
            status_result = runner(doc_path, config_path)
            assert status_result.returncode == 0, status_result.stderr
            status_value = _svc_value(status_result)
            assert status_value["running"] is True
            assert status_value["ready"] is True

            _write_orchestration(doc_path, {"action": "stop", "name": "web"})
            stop_result = runner(doc_path, config_path)
            assert stop_result.returncode == 0, stop_result.stderr
            stop_value = _svc_value(stop_result)
            nodes[engine] = _svc_node(stop_result)
            assert stop_value["stopped"] is True
            assert stop_value["ports_free"] is True
            assert not _pgid_exists(pgid), f"{engine}: pgid {pgid} still running after stop"
        finally:
            # So an assertion failure mid-loop never leaves the server
            # running for the rest of the suite (#661).
            _stop_and_wait_free(pgid)

    _assert_parity(nodes["cof"], nodes["electricity"])


def test_a_ready_timeout_matches_on_both_engines(tmp_path: Path, electricity_binary: Path) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    dead_port = _free_port()  # nothing ever listens here
    params = {
        "action": "start",
        "name": "never_ready",
        "command": [sys.executable, "-c", "import time; time.sleep(30)"],
        "ready": dead_port,
        "ready_timeout_ms": 500,
    }

    errors: dict[str, str] = {}
    for engine, runner in (
        ("cof", lambda doc, cfg: _run_cof(doc, cfg, home_dir)),
        (
            "electricity",
            lambda doc, cfg: _run_electricity(electricity_binary, doc, cfg, home_dir),
        ),
    ):
        case_dir = tmp_path / f"{engine}-timeout"
        case_dir.mkdir()
        state_dir = case_dir / "state"
        doc_path = case_dir / "orchestration.json"
        config_path = case_dir / "config.json"
        _write_config(config_path, state_dir)
        _write_orchestration(doc_path, params)

        result = runner(doc_path, config_path)
        assert result.returncode != 0, result.stdout
        payload = json.loads(result.stdout)
        assert payload["ok"] is False, f"{engine}: {payload}"
        assert "not ready" in payload["error"], f"{engine}: {payload}"
        errors[engine] = payload["error"]

    # Deterministic (name, target, timeout) -- compared in full between
    # the two engines, not just "contains 'not ready'" on each alone.
    assert errors["cof"] == errors["electricity"]


def test_a_port_held_by_an_unrelated_process_is_never_signalled_on_either_engine(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()

    for engine, runner in (
        ("cof", lambda doc, cfg: _run_cof(doc, cfg, home_dir)),
        (
            "electricity",
            lambda doc, cfg: _run_electricity(electricity_binary, doc, cfg, home_dir),
        ),
    ):
        holder = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        holder.bind(("127.0.0.1", 0))
        holder.listen(1)
        held_port = holder.getsockname()[1]
        try:
            case_dir = tmp_path / f"{engine}-held"
            case_dir.mkdir()
            state_dir = case_dir / "state"
            doc_path = case_dir / "orchestration.json"
            config_path = case_dir / "config.json"
            _write_config(config_path, state_dir)
            _write_orchestration(
                doc_path,
                {
                    "action": "start",
                    "name": "blocked",
                    "command": [sys.executable, "-c", "pass"],
                    "ports": [held_port],
                },
                on_error="continue",
            )

            result = runner(doc_path, config_path)
            assert result.returncode == 0, result.stderr
            svc = json.loads(result.stdout)["prime"]["svc"]
            assert svc["value"] is None, f"{engine}: {svc}"
            assert "not started" in (svc["meta"].get("stderr") or ""), f"{engine}: {svc}"
            holders = svc["meta"]["raw"]["port_holders"]
            assert holders, f"{engine}: {svc}"
            assert holders[0]["port"] == held_port, f"{engine}: {svc}"
        finally:
            # Still accepting connections -- the holder was never signalled.
            probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            probe.settimeout(1)
            probe.connect(("127.0.0.1", held_port))
            probe.close()
            holder.close()


def test_a_stale_record_naming_a_reused_pid_is_never_signalled_on_either_engine(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """A record whose `start_time` no longer matches the live process at
    that pid is stale -- dropped, never signalled -- on both engines
    (the live process started here keeps running throughout)."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    port = _free_port()

    for engine, runner in (
        ("cof", lambda doc, cfg: _run_cof(doc, cfg, home_dir)),
        (
            "electricity",
            lambda doc, cfg: _run_electricity(electricity_binary, doc, cfg, home_dir),
        ),
    ):
        case_dir = tmp_path / f"{engine}-stale"
        case_dir.mkdir()
        state_dir = case_dir / "state"
        doc_path = case_dir / "orchestration.json"
        config_path = case_dir / "config.json"
        _write_config(config_path, state_dir)
        _write_orchestration(
            doc_path,
            {
                "action": "start",
                "name": "web",
                "command": _http_server_command(port),
                "ports": [port],
            },
        )
        start_result = runner(doc_path, config_path)
        assert start_result.returncode == 0, start_result.stderr
        pgid = _svc_value(start_result)["pgid"]
        assert _pgid_exists(pgid)
        try:
            record_path = state_dir / "web.json"
            record = json.loads(record_path.read_text(encoding="utf-8"))
            record["start_time"] = "not a real start time"
            record_path.write_text(json.dumps(record), encoding="utf-8")

            _write_orchestration(doc_path, {"action": "status", "name": "web"})
            status_result = runner(doc_path, config_path)
            assert status_result.returncode == 0, status_result.stderr
            status_value = _svc_value(status_result)
            assert status_value["running"] is False, f"{engine}: {status_value}"
            assert status_value["stale_record"], f"{engine}: {status_value}"
            # Never signalled: the real process is still running.
            assert _pgid_exists(pgid), f"{engine}: pgid {pgid} was signalled"
        finally:
            _stop_and_wait_free(pgid)


def test_a_service_started_under_one_engine_is_stopped_under_the_other(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()

    for starter_name, starter, stopper_name, stopper in (
        (
            "cof",
            lambda doc, cfg: _run_cof(doc, cfg, home_dir),
            "electricity",
            lambda doc, cfg: _run_electricity(electricity_binary, doc, cfg, home_dir),
        ),
        (
            "electricity",
            lambda doc, cfg: _run_electricity(electricity_binary, doc, cfg, home_dir),
            "cof",
            lambda doc, cfg: _run_cof(doc, cfg, home_dir),
        ),
    ):
        case_dir = tmp_path / f"{starter_name}-to-{stopper_name}"
        case_dir.mkdir()
        state_dir = case_dir / "state"
        doc_path = case_dir / "orchestration.json"
        config_path = case_dir / "config.json"
        _write_config(config_path, state_dir)
        port = _free_port()
        _write_orchestration(
            doc_path,
            {
                "action": "start",
                "name": "web",
                "command": _http_server_command(port),
                "ports": [port],
                "ready": f"http://127.0.0.1:{port}/",
                "ready_timeout_ms": 5000,
            },
        )

        start_result = starter(doc_path, config_path)
        assert start_result.returncode == 0, (
            f"{starter_name} start: {start_result.stderr}"
        )
        pgid = _svc_value(start_result)["pgid"]
        try:
            assert _pgid_exists(pgid)

            _write_orchestration(doc_path, {"action": "stop", "name": "web"})
            stop_result = stopper(doc_path, config_path)
            assert stop_result.returncode == 0, f"{stopper_name} stop: {stop_result.stderr}"
            stop_value = _svc_value(stop_result)
            assert stop_value["stopped"] is True
            assert not _pgid_exists(pgid), (
                f"{starter_name}-started pgid {pgid} still running after {stopper_name} stop"
            )
        finally:
            _stop_and_wait_free(pgid)


def test_a_dead_leader_whose_group_still_has_members_is_still_ours_on_either_engine(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """The leader (the `/bin/sh -c` process) exits almost at once; only
    its backgrounded `sleep` -- still a member of the same process
    group, since a plain, non-interactive `&` never starts a job in a
    group of its own -- keeps the group alive. Ownership survives that:
    a dead leader whose group still has members is still ours."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()

    for engine, runner in (
        ("cof", lambda doc, cfg: _run_cof(doc, cfg, home_dir)),
        (
            "electricity",
            lambda doc, cfg: _run_electricity(electricity_binary, doc, cfg, home_dir),
        ),
    ):
        case_dir = tmp_path / f"{engine}-dead-leader"
        case_dir.mkdir()
        state_dir = case_dir / "state"
        doc_path = case_dir / "orchestration.json"
        config_path = case_dir / "config.json"
        _write_config(config_path, state_dir)
        _write_orchestration(
            doc_path,
            {
                # No `ready`: the leader is still alive when `start`
                # checks, so it succeeds at once, before the leader
                # itself exits a little over a second later.
                "action": "start",
                "name": "leaderdies",
                "command": "sleep 20 & sleep 1",
                "shell": True,
            },
        )
        start_result = runner(doc_path, config_path)
        assert start_result.returncode == 0, start_result.stderr
        pgid = _svc_value(start_result)["pgid"]
        try:
            time.sleep(2)  # the shell leader exits on its own by now
            assert _pgid_exists(pgid), f"{engine}: the group disappeared entirely"

            _write_orchestration(doc_path, {"action": "status", "name": "leaderdies"})
            status_result = runner(doc_path, config_path)
            assert status_result.returncode == 0, status_result.stderr
            status_value = _svc_value(status_result)
            assert status_value["running"] is True, f"{engine}: {status_value}"

            _write_orchestration(doc_path, {"action": "stop", "name": "leaderdies"})
            stop_result = runner(doc_path, config_path)
            assert stop_result.returncode == 0, stop_result.stderr
            stop_value = _svc_value(stop_result)
            assert stop_value["stopped"] is True, f"{engine}: {stop_value}"
            assert not _pgid_exists(pgid), f"{engine}: pgid {pgid} still running after stop"
        finally:
            _stop_and_wait_free(pgid)


def test_a_record_naming_a_different_boot_time_is_never_signalled_on_either_engine(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """A record whose `boot_time` no longer matches the machine's real
    one is stale (another boot) -- dropped, never signalled -- on both
    engines, exactly like a mismatched `start_time` (a reused pid)."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    port = _free_port()

    for engine, runner in (
        ("cof", lambda doc, cfg: _run_cof(doc, cfg, home_dir)),
        (
            "electricity",
            lambda doc, cfg: _run_electricity(electricity_binary, doc, cfg, home_dir),
        ),
    ):
        case_dir = tmp_path / f"{engine}-other-boot"
        case_dir.mkdir()
        state_dir = case_dir / "state"
        doc_path = case_dir / "orchestration.json"
        config_path = case_dir / "config.json"
        _write_config(config_path, state_dir)
        _write_orchestration(
            doc_path,
            {
                "action": "start",
                "name": "web",
                "command": _http_server_command(port),
                "ports": [port],
            },
        )
        start_result = runner(doc_path, config_path)
        assert start_result.returncode == 0, start_result.stderr
        pgid = _svc_value(start_result)["pgid"]
        assert _pgid_exists(pgid)
        try:
            record_path = state_dir / "web.json"
            record = json.loads(record_path.read_text(encoding="utf-8"))
            record["boot_time"] = 1  # the Unix epoch -- never the real boot time
            record_path.write_text(json.dumps(record), encoding="utf-8")

            _write_orchestration(doc_path, {"action": "status", "name": "web"})
            status_result = runner(doc_path, config_path)
            assert status_result.returncode == 0, status_result.stderr
            status_value = _svc_value(status_result)
            assert status_value["running"] is False, f"{engine}: {status_value}"
            assert status_value["stale_record"], f"{engine}: {status_value}"
            # Never signalled: the real process is still running.
            assert _pgid_exists(pgid), f"{engine}: pgid {pgid} was signalled"
        finally:
            _stop_and_wait_free(pgid)


def test_a_start_cancelled_while_waiting_for_ready_tears_the_group_down_gracefully(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """A SIGTERM to the `cof run`/`electricity run` process itself while
    a `start` is still waiting for `ready` cancels the run; both engines
    then stop exactly the group they started -- `SIGTERM` reaching the
    child first (observed through a trap it installs itself), not an
    unconditional `SIGKILL` -- and drop its record."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()

    for engine, build_cmd in (
        (
            "cof",
            lambda doc, cfg: [
                sys.executable, "-m", "circuitry.cli.app", "run", str(doc), "--config", str(cfg),
            ],
        ),
        (
            "electricity",
            lambda doc, cfg: [str(electricity_binary), "run", str(doc), "--config", str(cfg)],
        ),
    ):
        case_dir = tmp_path / f"{engine}-cancel"
        case_dir.mkdir()
        state_dir = case_dir / "state"
        doc_path = case_dir / "orchestration.json"
        config_path = case_dir / "config.json"
        _write_config(config_path, state_dir)
        marker = case_dir / "sigterm-seen"
        script = f"trap 'touch {marker} ; exit 0' TERM; sleep 20"
        _write_orchestration(
            doc_path,
            {
                "action": "start",
                "name": "cancelme",
                "command": script,
                "shell": True,
                # Nothing ever listens here, so the run is still waiting
                # for `ready` when the signal below arrives.
                "ready": 1,
                "ready_timeout_ms": 30_000,
                "grace_ms": 2_000,
            },
        )

        proc = subprocess.Popen(
            build_cmd(doc_path, config_path), cwd=doc_path.parent,
            env=_sandboxed_env(home_dir), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        pgid: int | None = None
        try:
            record_path = state_dir / "cancelme.json"
            deadline = time.monotonic() + 10
            while pgid is None and time.monotonic() < deadline:
                if record_path.exists():
                    try:
                        pgid = json.loads(record_path.read_text(encoding="utf-8"))["pgid"]
                    except (json.JSONDecodeError, KeyError):
                        pass
                if pgid is None:
                    time.sleep(0.05)
            assert pgid is not None, f"{engine}: the record never appeared"

            proc.send_signal(signal.SIGTERM)
            proc.communicate(timeout=15)

            deadline = time.monotonic() + 10
            while _pgid_exists(pgid) and time.monotonic() < deadline:
                time.sleep(0.05)
            assert not _pgid_exists(pgid), f"{engine}: pgid {pgid} still running after cancellation"
            assert marker.exists(), f"{engine}: SIGTERM should have reached the child before it died"
            assert not record_path.exists(), f"{engine}: the record should be dropped once torn down"
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)
            if pgid is not None:
                _stop_and_wait_free(pgid)


def test_parallel_start_and_stop_branches_in_one_run_do_not_deadlock(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """Two `start`s, then two `stop`s, run in parallel (a tree-flow
    `dynamic`) in one single run. electricity's own dispatch runs every
    branch of a run as futures polled from one `FuturesUnordered` on a
    single thread -- a blocking `flock` acquire deadlocked this
    outright: branch A takes the lock and yields; branch B's own
    blocking acquire attempt then parks the only thread, so A can never
    resume to release it. Bounded (`subprocess.run`'s own `timeout`): a
    regression here fails loudly rather than hanging the suite."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()

    for engine, run in (
        ("cof", lambda doc, cfg: _run_cof(doc, cfg, home_dir, timeout=30)),
        (
            "electricity",
            lambda doc, cfg: _run_electricity(electricity_binary, doc, cfg, home_dir, timeout=30),
        ),
    ):
        case_dir = tmp_path / f"{engine}-parallel"
        case_dir.mkdir()
        state_dir = case_dir / "state"
        doc_path = case_dir / "orchestration.json"
        config_path = case_dir / "config.json"
        _write_config(config_path, state_dir)
        port_a = _free_port()
        port_b = _free_port()
        doc_path.write_text(
            json.dumps(
                {
                    "effects": [
                        {
                            "type": "dynamic",
                            "name": "starts",
                            "flow": "tree",
                            "effects": [
                                {
                                    "type": "tool",
                                    "name": "a",
                                    "provider": "service",
                                    "params": {
                                        "action": "start",
                                        "name": "par_a",
                                        "command": _http_server_command(port_a),
                                        "ports": [port_a],
                                        "ready": f"http://127.0.0.1:{port_a}/",
                                        "ready_timeout_ms": 5000,
                                    },
                                },
                                {
                                    "type": "tool",
                                    "name": "b",
                                    "provider": "service",
                                    "params": {
                                        "action": "start",
                                        "name": "par_b",
                                        "command": _http_server_command(port_b),
                                        "ports": [port_b],
                                        "ready": f"http://127.0.0.1:{port_b}/",
                                        "ready_timeout_ms": 5000,
                                    },
                                },
                            ],
                        },
                        {
                            "type": "dynamic",
                            "name": "stops",
                            "flow": "tree",
                            "effects": [
                                {
                                    "type": "tool",
                                    "name": "a",
                                    "provider": "service",
                                    "params": {"action": "stop", "name": "par_a"},
                                },
                                {
                                    "type": "tool",
                                    "name": "b",
                                    "provider": "service",
                                    "params": {"action": "stop", "name": "par_b"},
                                },
                            ],
                        },
                    ]
                }
            ),
            encoding="utf-8",
        )

        try:
            result = run(doc_path, config_path)
        except subprocess.TimeoutExpired:
            pytest.fail(f"{engine}: the run deadlocked instead of finishing")
        assert result.returncode == 0, result.stderr
        payload = json.loads(result.stdout)
        starts = payload["prime"]["starts"]
        stops = payload["prime"]["stops"]
        assert starts["a"]["value"]["running"] is True, payload
        assert starts["b"]["value"]["running"] is True, payload
        assert stops["a"]["value"]["stopped"] is True, payload
        assert stops["b"]["value"]["stopped"] is True, payload


def _ignores_sigterm_command() -> list:
    script = Path(__file__).resolve().parent / "fakes" / "service_ignores_sigterm.py"
    return [sys.executable, str(script)]


def test_sigint_during_a_waiting_start_and_a_locked_stop_does_not_hang(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """issue #721's own follow-up: one branch (A) starts a service still
    waiting for `ready` (armed, never satisfied); a sibling branch (B)
    waits briefly, then stops a service whose process ignores
    `SIGTERM` -- holding the state directory's lock through its own
    `grace_ms`, exactly as `ServicePlugin._stop`'s own `with
    self._locked():` block does. `SIGINT` to the run itself, sent only
    once A's record exists and B's target has logged that it ignored
    `SIGTERM`, cancels the run on both engines -- it must never hang:
    electricity used to tear A's group down with a synchronous,
    blocking lock acquire right there in `Drop`, which parked its own
    single runtime thread for good while B's `stop` still held that
    same lock, so neither branch could ever finish and the process sat
    past its own grace period until something force-killed it. Both
    engines must end the same way: exit 130, both groups gone, both
    records gone."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()

    for engine, build_cmd, setup_run in (
        (
            "cof",
            lambda doc, cfg: [
                sys.executable, "-m", "circuitry.cli.app", "run", str(doc), "--config", str(cfg),
            ],
            lambda doc, cfg: _run_cof(doc, cfg, home_dir),
        ),
        (
            "electricity",
            lambda doc, cfg: [str(electricity_binary), "run", str(doc), "--config", str(cfg)],
            lambda doc, cfg: _run_electricity(electricity_binary, doc, cfg, home_dir),
        ),
    ):
        case_dir = tmp_path / f"{engine}-cancel-parallel"
        case_dir.mkdir()
        state_dir = case_dir / "state"
        config_path = case_dir / "config.json"
        _write_config(config_path, state_dir)

        # B's own target: started ahead of the run under test (same as
        # the engines sharing one state directory elsewhere in this
        # module), so its record already exists when that run starts.
        setup_doc = case_dir / "setup.json"
        _write_orchestration(
            setup_doc,
            {"action": "start", "name": "stub", "command": _ignores_sigterm_command()},
        )
        setup_result = setup_run(setup_doc, config_path)
        assert setup_result.returncode == 0, setup_result.stderr
        stub_record_path = state_dir / "stub.json"
        stub_pgid = json.loads(stub_record_path.read_text(encoding="utf-8"))["pgid"]
        stub_log_path = state_dir / "stub.log"

        doc_path = case_dir / "orchestration.json"
        doc_path.write_text(
            json.dumps(
                {
                    "effects": [
                        {
                            "type": "dynamic",
                            "name": "fan",
                            "flow": "tree",
                            "effects": [
                                {
                                    "type": "tool",
                                    "name": "a",
                                    "provider": "service",
                                    "params": {
                                        "action": "start",
                                        "name": "cx",
                                        "command": "sleep 30",
                                        "shell": True,
                                        # Nothing ever listens here, so A
                                        # is still waiting for `ready`
                                        # when the signal below arrives.
                                        "ready": 1,
                                        "ready_timeout_ms": 30_000,
                                        "grace_ms": 2_000,
                                    },
                                },
                                {
                                    "type": "dynamic",
                                    "name": "b",
                                    "effects": [
                                        {
                                            "type": "tool",
                                            "name": "wait",
                                            "provider": "shell",
                                            "params": {
                                                "command": "sleep",
                                                "args": ["2"],
                                                "allowed_commands": ["sleep"],
                                            },
                                        },
                                        {
                                            "type": "tool",
                                            "name": "stop",
                                            "provider": "service",
                                            "params": {
                                                "action": "stop",
                                                "name": "stub",
                                                "grace_ms": 12_000,
                                            },
                                        },
                                    ],
                                },
                            ],
                        },
                    ]
                }
            ),
            encoding="utf-8",
        )

        proc = subprocess.Popen(
            build_cmd(doc_path, config_path), cwd=doc_path.parent,
            env=_sandboxed_env(home_dir), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        cx_record_path = state_dir / "cx.json"
        cx_pgid: int | None = None
        try:
            deadline = time.monotonic() + 20
            armed = False
            while time.monotonic() < deadline and not armed:
                armed = (
                    cx_record_path.exists()
                    and stub_log_path.exists()
                    and "ignored SIGTERM" in stub_log_path.read_text(encoding="utf-8")
                )
                if not armed:
                    time.sleep(0.05)
            assert armed, f"{engine}: never saw the start armed while the stop held the lock"
            cx_pgid = json.loads(cx_record_path.read_text(encoding="utf-8"))["pgid"]

            proc.send_signal(signal.SIGINT)
            try:
                proc.communicate(timeout=25)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.communicate(timeout=5)
                pytest.fail(f"{engine}: the run hung instead of exiting after SIGINT")
            assert proc.returncode == 130, (engine, proc.returncode)

            deadline = time.monotonic() + 10
            while (_pgid_exists(cx_pgid) or _pgid_exists(stub_pgid)) and time.monotonic() < deadline:
                time.sleep(0.05)
            assert not _pgid_exists(cx_pgid), f"{engine}: the waiting start's group outlived the cancel"
            assert not _pgid_exists(stub_pgid), f"{engine}: the stub's group outlived the cancel"
            assert not cx_record_path.exists(), f"{engine}: the waiting start's record should be gone"
            assert not stub_record_path.exists(), f"{engine}: the stub's record should be gone"
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)
            if cx_pgid is not None:
                _stop_and_wait_free(cx_pgid)
            _stop_and_wait_free(stub_pgid)
