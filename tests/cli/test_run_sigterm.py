"""`cof run` and a SIGTERM mid-run (#338).

Mirrors `test_run_interrupt.py`'s Ctrl-C/SIGINT coverage, but for real:
SIGTERM has no default `KeyboardInterrupt`-raising behavior the way SIGINT
does, so it can only be exercised by actually delivering the signal to a
real `cof run` subprocess, not by faking an adapter in-process.
"""

from __future__ import annotations

import json
import os
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


def _sandboxed_env(tmp_path: Path) -> dict[str, str]:
    """A child-process env whose fake $HOME keeps it off the real
    ~/.config/circuitry/last-run.json and global config (CLAUDE.md hermeticity)."""
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    return {**os.environ, "HOME": str(home)}


def _find_node(state: object, name: str) -> dict[str, object] | None:
    """Recursively find the ``{name: {..., "meta": {...}}}`` node anywhere in
    *state*'s tree, regardless of nesting (e.g. under a ``dynamic`` effect)."""
    stack = [state]
    while stack:
        node = stack.pop()
        if not isinstance(node, dict):
            continue
        child = node.get(name)
        if isinstance(child, dict) and "meta" in child:
            return child
        stack.extend(v for v in node.values() if isinstance(v, dict))
    return None


def _wait_for_step1(live_state_path: Path, *, timeout: float = 10.0) -> None:
    """Poll *live_state_path* until step1 has finished, instead of a fixed
    sleep that can land too early (handler not installed yet) or too late
    (under load from other concurrent test runs) (#338)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if live_state_path.exists():
            try:
                state = json.loads(live_state_path.read_text(encoding="utf-8"))
            except json.JSONDecodeError:
                state = None
            if state is not None:
                node = _find_node(state.get("prime"), "step1")
                if node is not None and node.get("meta", {}).get("completed_at"):
                    return
        time.sleep(0.05)
    raise TimeoutError(f"step1 did not complete within {timeout}s ({live_state_path})")


def _run_and_sigterm(orch: Path, out_path: Path) -> subprocess.CompletedProcess[str]:
    tmp_path = orch.parent
    live_state_path = tmp_path / "live.json"
    proc = subprocess.Popen(
        [
            sys.executable, "-m", "circuitry.cli.app", "run", str(orch),
            "--out", str(out_path), "--live-state", str(live_state_path), "--quiet",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=_sandboxed_env(tmp_path),
        cwd=tmp_path,
    )
    try:
        _wait_for_step1(live_state_path)
    except TimeoutError:
        proc.kill()
        proc.communicate(timeout=15)
        raise
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

    before = json.loads(out_path.read_text(encoding="utf-8"))
    step1_before = before["prime"]["step1"]

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
        env=_sandboxed_env(tmp_path),
        cwd=tmp_path,
    )

    assert resume.returncode == 0, (resume.stdout, resume.stderr)
    state = json.loads(resumed_out.read_text(encoding="utf-8"))
    for name in ("step1", "slow", "step3"):
        assert state["prime"][name]["meta"]["completed_at"]
    # step1 (and its uuid value) was skipped on resume, not rerun: the value
    # and completion timestamp carried over unchanged from before the resume.
    step1_after = state["prime"]["step1"]
    assert step1_after["value"] == step1_before["value"]
    assert step1_after["meta"]["completed_at"] == step1_before["meta"]["completed_at"]
    # step3 never ran before the signal landed, so its completion is new.
    assert "step3" not in before["prime"]
