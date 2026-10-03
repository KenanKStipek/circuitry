"""`cof run` and a SIGTERM mid-run (#338).

Mirrors `test_run_interrupt.py`'s Ctrl-C/SIGINT coverage, but for real:
SIGTERM has no default `KeyboardInterrupt`-raising behavior the way SIGINT
does, so it can only be exercised by actually delivering the signal to a
real `cof run` subprocess, not by faking an adapter in-process.
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

requires_sleep = pytest.mark.skipif(
    shutil.which("sleep") is None, reason="requires the 'sleep' binary"
)


def _slow_orchestration(tmp_path: Path, *, with_finally: bool) -> Path:
    orch = tmp_path / "slow.yml"
    if with_finally:
        body = """
effects:
  - type: dynamic
    name: d1
    effects:
      - type: tool
        name: step1
        provider: uuid
      - type: tool
        name: slow
        provider: shell
        params:
          command: sleep
          args: ["5"]
          allowed_commands: ["sleep"]
      - type: tool
        name: step3
        provider: uuid
    finally:
      - type: tool
        name: cleanup
        provider: uuid
""".lstrip()
    else:
        body = """
effects:
  - type: tool
    name: step1
    provider: uuid
  - type: tool
    name: slow
    provider: shell
    params:
      command: sleep
      args: ["5"]
      allowed_commands: ["sleep"]
  - type: tool
    name: step3
    provider: uuid
""".lstrip()
    orch.write_text(body, encoding="utf-8")
    return orch


def _run_and_sigterm(orch: Path, out_path: Path) -> subprocess.CompletedProcess[str]:
    proc = subprocess.Popen(
        [
            sys.executable, "-m", "circuitry.cli.app", "run", str(orch),
            "--out", str(out_path), "--quiet",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    # Give the "slow" step time to start (step1 is instant) before killing.
    time.sleep(1.5)
    proc.send_signal(signal.SIGTERM)
    try:
        stdout, stderr = proc.communicate(timeout=15)
    except subprocess.TimeoutExpired:
        proc.kill()
        raise
    return subprocess.CompletedProcess(proc.args, proc.returncode, stdout, stderr)


@requires_sleep
def test_sigterm_mid_run_exits_143_and_writes_resumable_out(tmp_path: Path) -> None:
    orch = _slow_orchestration(tmp_path, with_finally=False)
    out_path = tmp_path / "out.json"

    result = _run_and_sigterm(orch, out_path)

    assert result.returncode == 143, (result.stdout, result.stderr)
    assert out_path.exists()
    state = json.loads(out_path.read_text(encoding="utf-8"))
    # step1 finished before the signal; "slow" never completed; step3 never
    # started — exactly the shape an ordinary crash mid-run would leave.
    assert state["prime"]["step1"]["meta"]["completed_at"]
    assert state["prime"]["slow"]["meta"]["completed_at"] is None
    assert "step3" not in state["prime"]
    assert state["runtime"]["last_run"]["completed_at"]


@requires_sleep
def test_sigterm_mid_run_runs_finally(tmp_path: Path) -> None:
    orch = _slow_orchestration(tmp_path, with_finally=True)
    out_path = tmp_path / "out.json"

    result = _run_and_sigterm(orch, out_path)

    assert result.returncode == 143, (result.stdout, result.stderr)
    state = json.loads(out_path.read_text(encoding="utf-8"))
    assert state["prime"]["d1"]["cleanup"]["meta"]["completed_at"]


@requires_sleep
def test_resume_after_sigterm_skips_finished_steps(tmp_path: Path) -> None:
    orch = _slow_orchestration(tmp_path, with_finally=False)
    out_path = tmp_path / "out.json"
    _run_and_sigterm(orch, out_path)

    resumed_out = tmp_path / "resumed.json"
    resume = subprocess.run(
        [
            sys.executable, "-m", "circuitry.cli.app", "run", str(orch),
            "--state", str(out_path), "--resume", "x",
            "--out", str(resumed_out), "--quiet",
        ],
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )

    assert resume.returncode == 0, (resume.stdout, resume.stderr)
    state = json.loads(resumed_out.read_text(encoding="utf-8"))
    for name in ("step1", "slow", "step3"):
        assert state["prime"][name]["meta"]["completed_at"]
