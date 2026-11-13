"""`cof run` and `electricity run` dispatch the `service` tool the same way
(issue #721): start/status/stop of a local HTTP server, a ready timeout, a
port another process holds (never signalled), a stale record naming a
reused pid (never signalled), and a cross-engine stop (a service one
engine starts, the other stops).

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
        assert start_value["running"] is True
        assert start_value["ready"] is True
        assert start_value["already_running"] is False
        pgid = start_value["pgid"]
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
        assert stop_value["stopped"] is True
        assert stop_value["ports_free"] is True
        assert not _pgid_exists(pgid), f"{engine}: pgid {pgid} still running after stop"


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
        assert _pgid_exists(pgid)

        _write_orchestration(doc_path, {"action": "stop", "name": "web"})
        stop_result = stopper(doc_path, config_path)
        assert stop_result.returncode == 0, f"{stopper_name} stop: {stop_result.stderr}"
        stop_value = _svc_value(stop_result)
        assert stop_value["stopped"] is True
        assert not _pgid_exists(pgid), (
            f"{starter_name}-started pgid {pgid} still running after {stopper_name} stop"
        )
