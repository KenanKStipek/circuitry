"""A cancelled run (SIGTERM) stops an `agent` effect's whole session group,
on both engines (issue #720, #661): the fake `pi` ignores SIGTERM itself,
the same shape `tests/cli/test_run_sigterm.py` already exercises for a
`shell` effect, so only the run's own process-group kill -- not the
child's own cooperative exit -- can end it.
"""

from __future__ import annotations

import json
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

from . import harness

ELECTRICITY_DIR = Path(__file__).resolve().parents[2] / "electricity"

_FAKE_PI = '''#!/usr/bin/env python3
"""A fake `pi` that announces a tool call, then ignores SIGTERM and
sleeps -- a cancelled run has to kill its whole process group to stop it.
Bounded (#661): gives up on its own once orphaned or after 120 s."""

from __future__ import annotations

import json
import os
import signal
import sys
import time

attached = [arg[1:] for arg in sys.argv[1:] if arg.startswith("@")]
with open(attached[0], encoding="utf-8") as handle:
    handle.read()

sys.stdout.write(json.dumps({"type": "session", "id": "sess-cancel"}) + "\\n")
sys.stdout.write(
    json.dumps(
        {
            "type": "tool_execution_start",
            "toolName": "write",
            "toolCallId": "t1",
            "args": {"path": "result.json", "content": "x\\n"},
        }
    )
    + "\\n"
)
sys.stdout.flush()

signal.signal(signal.SIGTERM, signal.SIG_IGN)
parent = os.getppid()
deadline = time.monotonic() + 120
while os.getppid() == parent and time.monotonic() < deadline:
    time.sleep(0.2)
'''

_ORCHESTRATION = """name: agent-cancel-sample
effects:
  - type: tool
    name: implement
    provider: agent
    timeout_ms: 60000
    params:
      engine: pi
      prompt: "Write the result file."
      cwd: {from: input.work_dir}
"""


def _write_fixture(tmp_path: Path) -> tuple[Path, Path, Path]:
    """Returns `(case_dir, work_dir, fake_path)` -- *fake_path* is this
    test's own unique path, specific enough to `pgrep -f` for safely
    (#661's own hygiene: never matching an unrelated `pi` elsewhere on a
    shared machine)."""
    case_dir = tmp_path / "doc"
    work_dir = case_dir / "work"
    fakes_dir = case_dir / "fakes"
    work_dir.mkdir(parents=True)
    fakes_dir.mkdir(parents=True)
    (case_dir / "orchestration.yml").write_text(_ORCHESTRATION, encoding="utf-8")
    (case_dir / "config.json").write_text("{}", encoding="utf-8")
    fake_path = fakes_dir / "pi"
    fake_path.write_text(_FAKE_PI, encoding="utf-8")
    fake_path.chmod(0o700)
    return case_dir, work_dir, fake_path


def _wait_for_tool_start(live_state_path: Path, *, timeout: float = 20.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if live_state_path.exists():
            try:
                data = json.loads(live_state_path.read_text(encoding="utf-8"))
            except (json.JSONDecodeError, OSError):
                data = None
            if data is not None:
                live = data.get("prime", {}).get("implement", {}).get("meta", {}).get("live")
                if isinstance(live, dict) and live.get("current_tool") is not None:
                    return
        time.sleep(0.05)
    raise TimeoutError(f"agent effect never started a tool call within {timeout}s")


def _assert_no_fake_pi_survives(fake_path: Path) -> None:
    """`pgrep -f <fake_path>` -- the absolute path is unique to this test's
    own `tmp_path`, so this can never match an unrelated `pi` process
    elsewhere on a shared machine. `pgrep` exits `1` when nothing matches."""
    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline:
        result = subprocess.run(
            ["pgrep", "-f", str(fake_path)], capture_output=True, text=True, check=False
        )
        if result.returncode != 0:
            return
        time.sleep(0.1)
    pytest.fail(f"a fake pi process survived cancellation: {result.stdout}")


def _run_and_sigterm(cmd: list[str], cwd: Path, env: dict[str, str], live_state_path: Path) -> int:
    proc = subprocess.Popen(
        cmd, cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
    )
    try:
        _wait_for_tool_start(live_state_path)
    except TimeoutError:
        proc.kill()
        proc.communicate(timeout=15)
        raise
    proc.send_signal(signal.SIGTERM)
    try:
        proc.communicate(timeout=15)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.communicate(timeout=15)
        raise
    return proc.returncode


def test_python_sigterm_mid_agent_session_kills_the_whole_group(tmp_path: Path) -> None:
    case_dir, work_dir, fake_path = _write_fixture(tmp_path)
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    live_state_path = tmp_path / "live-state.json"
    out_path = tmp_path / "out.json"
    cmd = [
        sys.executable, "-m", "circuitry.cli.app", "run", "orchestration.yml",
        "--out", str(out_path), "--quiet", "--config", "config.json",
        "--live-state", str(live_state_path), "-e", f"work_dir={work_dir}",
    ]
    returncode = _run_and_sigterm(
        cmd, case_dir, harness._sandboxed_env(case_dir, home_dir), live_state_path
    )
    assert returncode == 143
    _assert_no_fake_pi_survives(fake_path)


@pytest.fixture(scope="module")
def electricity_binary() -> Path | None:
    if shutil.which("cargo") is None:
        return None
    proc = subprocess.run(
        ["cargo", "build", "-p", "electricity-cli", "--bin", "electricity"],
        cwd=ELECTRICITY_DIR,
        env=harness.cargo_env(),
        capture_output=True,
        text=True,
        timeout=600,
        check=False,
    )
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p electricity-cli failed:\n{proc.stdout}\n{proc.stderr}")
    binary = ELECTRICITY_DIR / "target" / "debug" / "electricity"
    assert binary.exists(), f"cargo build succeeded but {binary} is missing"
    return binary


def test_electricity_sigterm_mid_agent_session_kills_the_whole_group(
    tmp_path: Path, electricity_binary: Path | None
) -> None:
    if electricity_binary is None:
        pytest.skip("cargo not available; electricity binary cannot be built")
    case_dir, work_dir, fake_path = _write_fixture(tmp_path)
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    live_state_path = tmp_path / "live-state.json"
    out_path = tmp_path / "out.json"
    cmd = [
        str(electricity_binary), str(case_dir / "config.json"), "orchestration.yml",
        "--out", str(out_path), "--live-state", str(live_state_path),
        "-e", f"work_dir={work_dir}",
    ]
    returncode = _run_and_sigterm(
        cmd, case_dir, harness._sandboxed_env(case_dir, home_dir), live_state_path
    )
    assert returncode == 143
    _assert_no_fake_pi_survives(fake_path)
