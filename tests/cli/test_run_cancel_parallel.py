"""Ctrl-C/SIGTERM stop a parallel step promptly (#356).

A tree-flow `dynamic`/`loop` submits its branches to a `ThreadPoolExecutor`.
Before this fix, leaving the `with` block waited for every submitted
branch — a queued (not-yet-started) branch ran to completion anyway, and a
running branch's subprocess kept going until it finished on its own, so
stopping took as long as finishing would have. These tests deliver a real
SIGINT/SIGTERM to a `cof run` subprocess running several slow shell
branches and assert: a branch already running when the signal lands is
killed (its process group, not just its own pid — nothing survives it); a
branch still queued under a low `max_concurrency` is never started at all
(no marker file); `finally:` still runs; the run ends within a small,
CI-safe bound rather than waiting out each branch's own sleep; the exit
code is 130 (SIGINT) or 143 (SIGTERM); and a second signal during cleanup
ends the process at once, with the same exit code and no traceback.
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

requires_bash = pytest.mark.skipif(
    shutil.which("bash") is None, reason="requires the 'bash' binary"
)

#: Each branch sleeps this long if never interrupted — long enough that a
#: run only finishes early because it was actually cancelled, never because
#: the branch happened to complete on its own first.
_BRANCH_SLEEP_SECONDS = 60

#: How long a cancelled run may take to actually exit. Loose on purpose
#: (#356 says "loosely bound... under 10s" for CI machines under load) —
#: what matters is "nowhere near _BRANCH_SLEEP_SECONDS", not a tight bound.
_STOP_BOUND_SECONDS = 10.0

#: Credential env vars that must never reach a spawned `cof run` here — this
#: suite never configures an adapter, so none of them are needed, and their
#: presence must not change anything it asserts (#356's own test plan).
_CREDENTIAL_ENV_VARS = (
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "CYBERDINER_TOKEN",
    "CYBERDINER_EXPO_URL",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "NPM_TOKEN",
)


def _sandboxed_env(tmp_path: Path) -> dict[str, str]:
    """A child-process env with a fake $HOME (CLAUDE.md hermeticity) and no
    credentials — this suite has no adapter to use them, real or fake."""
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env = {
        k: v for k, v in os.environ.items() if k not in _CREDENTIAL_ENV_VARS
    }
    env["HOME"] = str(home)
    return env


def _branch_tool(name: str, *, pidfile: Path, started: Path) -> str:
    """A `tool: shell` effect: records its own pid and a 'started' marker
    before sleeping, so a test can wait for (and later assert on) exactly
    those two things without a fixed sleep of its own."""
    script = f"echo $$ > {pidfile}; touch {started}; sleep {_BRANCH_SLEEP_SECONDS}"
    return f"""
      - type: tool
        name: {name}
        provider: shell
        params:
          command: bash
          args: ["-c", "{script}"]
          allowed_commands: ["bash"]
""".rstrip("\n")


def _tree_dynamic_orchestration(
    tmp_path: Path, *, n_branches: int, max_concurrency: int
) -> tuple[Path, list[Path], list[Path]]:
    """A tree-flow `dynamic` with *n_branches* slow shell branches, bounded
    to *max_concurrency* at once, `on_error: continue` (#356's own test
    plan asks for this inside the parallel step), and a `finally:` that
    must still run. Returns (orch_path, pidfiles, started_markers)."""
    pidfiles = [tmp_path / f"pid_{i}" for i in range(n_branches)]
    started = [tmp_path / f"started_{i}" for i in range(n_branches)]
    branches = "\n".join(
        _branch_tool(f"b{i}", pidfile=pidfiles[i], started=started[i])
        for i in range(n_branches)
    )
    body = f"""
effects:
  - type: dynamic
    name: d1
    flow: tree
    max_concurrency: {max_concurrency}
    on_error: continue
    effects:
{branches}
    finally:
      - type: tool
        name: cleanup
        provider: uuid
""".lstrip("\n")
    orch = tmp_path / "parallel_dynamic.yml"
    orch.write_text(body, encoding="utf-8")
    return orch, pidfiles, started


def _parallel_loop_orchestration(
    tmp_path: Path, *, n_iterations: int, max_concurrency: int
) -> tuple[Path, Path, list[Path], list[Path]]:
    """A `loop: flow: tree` with *n_iterations* slow shell passes, same
    bound/continue/finally shape as the dynamic above. Returns
    (orch_path, state_path, pidfiles, started_markers)."""
    pidfiles = [tmp_path / f"pid_{i}" for i in range(n_iterations)]
    started = [tmp_path / f"started_{i}" for i in range(n_iterations)]
    scripts = [
        f"echo $$ > {pidfiles[i]}; touch {started[i]}; sleep {_BRANCH_SLEEP_SECONDS}"
        for i in range(n_iterations)
    ]
    body = f"""
effects:
  - type: loop
    name: lp
    flow: tree
    max_concurrency: {max_concurrency}
    on_error: continue
    each: {{in: input.scripts, as: cmd}}
    body:
      - type: tool
        name: step
        provider: shell
        params:
          command: bash
          # Triple-brace: Mustache's default double-brace HTML-escapes
          # output (turning our script's own ">"/"&" into "&gt;"/"&amp;"
          # and silently breaking the shell redirect), so this must stay
          # unescaped.
          args: ["-c", "{{{{{{cmd}}}}}}"]
          allowed_commands: ["bash"]
  - type: tool
    name: cleanup
    provider: uuid
""".lstrip("\n")
    orch = tmp_path / "parallel_loop.yml"
    orch.write_text(body, encoding="utf-8")
    state_path = tmp_path / "initial_state.json"
    state_path.write_text(
        json.dumps({"input": {"scripts": scripts}}), encoding="utf-8"
    )
    return orch, state_path, pidfiles, started


def _wait_for_paths(paths: list[Path], *, timeout: float = 15.0) -> None:
    """Poll until every path in *paths* exists — no fixed sleep (#356's own
    test plan): a branch's marker lands the instant it actually starts, and
    nothing else tells us that reliably under load."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if all(p.exists() for p in paths):
            return
        time.sleep(0.02)
    missing = [str(p) for p in paths if not p.exists()]
    raise TimeoutError(f"never started within {timeout}s: {missing}")


def _pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _run_cof(orch: Path, *, out_path: Path, state_path: Path | None = None) -> subprocess.Popen[str]:
    tmp_path = orch.parent
    args = [sys.executable, "-m", "circuitry.cli.app", "run", str(orch)]
    if state_path is not None:
        args += ["--state", str(state_path)]
    args += ["--out", str(out_path), "--quiet"]
    return subprocess.Popen(
        args,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=_sandboxed_env(tmp_path),
        cwd=tmp_path,
    )


@requires_bash
@pytest.mark.parametrize("sig,expected_code", [(signal.SIGINT, 130), (signal.SIGTERM, 143)])
def test_cancel_tree_dynamic_stops_promptly(
    tmp_path: Path, sig: int, expected_code: int
) -> None:
    orch, pidfiles, started = _tree_dynamic_orchestration(
        tmp_path, n_branches=4, max_concurrency=2
    )
    out_path = tmp_path / "out.json"
    proc = _run_cof(orch, out_path=out_path)

    # max_concurrency=2: exactly the first two branches ever start.
    try:
        _wait_for_paths(started[:2])
    except TimeoutError:
        proc.kill()
        proc.communicate(timeout=15)
        raise

    t0 = time.monotonic()
    proc.send_signal(sig)
    try:
        stdout, stderr = proc.communicate(timeout=_STOP_BOUND_SECONDS + 5)
    except subprocess.TimeoutExpired:
        proc.kill()
        raise
    elapsed = time.monotonic() - t0

    assert proc.returncode == expected_code, (stdout, stderr)
    assert elapsed < _STOP_BOUND_SECONDS, (
        f"took {elapsed:.1f}s to stop; branches sleep {_BRANCH_SLEEP_SECONDS}s"
    )
    assert "Traceback" not in stderr, stderr

    # Queued branches (b2, b3) were never dequeued at all — no marker.
    assert not started[2].exists()
    assert not started[3].exists()
    assert not pidfiles[2].exists()
    assert not pidfiles[3].exists()

    # Running branches (b0, b1) were started, then killed outright.
    for pidfile in pidfiles[:2]:
        pid = int(pidfile.read_text(encoding="utf-8").strip())
        assert not _pid_alive(pid), f"pid {pid} survived cancellation"

    state = json.loads(out_path.read_text(encoding="utf-8"))
    d1 = state["prime"]["d1"]
    # A cancelled tree-flow step is not merged (#356's own acceptance
    # criteria): tree branches write into their own isolated Store, folded
    # into the parent only *after* every branch finishes — skipped
    # entirely once cancellation raises past that merge. A branch already
    # running when the signal landed (b0, b1) is exactly as absent from
    # the committed state as one that was never even dequeued (b2, b3).
    assert "b0" not in d1
    assert "b1" not in d1
    assert "b2" not in d1
    assert "b3" not in d1
    # The dynamic's own node still finalizes (unlike its isolated
    # children): meta.completed_at/error are set and `finally:` still ran.
    assert d1["meta"]["completed_at"]
    # Never empty (#356 follow-up) — an empty meta.error would make this
    # dynamic look finished-ok to `cof run --resume`'s own
    # effect_completed_ok check, skipping its unfinished children instead
    # of rerunning them.
    assert d1["meta"]["error"]
    assert d1["cleanup"]["meta"]["completed_at"]
    assert state["runtime"]["last_run"]["completed_at"]


@requires_bash
@pytest.mark.parametrize("sig,expected_code", [(signal.SIGINT, 130), (signal.SIGTERM, 143)])
def test_cancel_parallel_loop_stops_promptly(
    tmp_path: Path, sig: int, expected_code: int
) -> None:
    orch, state_path, pidfiles, started = _parallel_loop_orchestration(
        tmp_path, n_iterations=4, max_concurrency=2
    )
    out_path = tmp_path / "out.json"
    proc = _run_cof(orch, out_path=out_path, state_path=state_path)

    try:
        _wait_for_paths(started[:2])
    except TimeoutError:
        proc.kill()
        proc.communicate(timeout=15)
        raise

    t0 = time.monotonic()
    proc.send_signal(sig)
    try:
        stdout, stderr = proc.communicate(timeout=_STOP_BOUND_SECONDS + 5)
    except subprocess.TimeoutExpired:
        proc.kill()
        raise
    elapsed = time.monotonic() - t0

    assert proc.returncode == expected_code, (stdout, stderr)
    assert elapsed < _STOP_BOUND_SECONDS, (
        f"took {elapsed:.1f}s to stop; passes sleep {_BRANCH_SLEEP_SECONDS}s"
    )
    assert "Traceback" not in stderr, stderr

    assert not started[2].exists()
    assert not started[3].exists()

    for pidfile in pidfiles[:2]:
        pid = int(pidfile.read_text(encoding="utf-8").strip())
        assert not _pid_alive(pid), f"pid {pid} survived cancellation"

    state = json.loads(out_path.read_text(encoding="utf-8"))
    # The loop itself never reached a completed node; the cleanup step
    # declared after it (chain flow, main dynamic) never ran either —
    # exactly the usual "an interrupted step's own parent never advances"
    # shape (#270 F6).
    assert state["prime"]["lp"]["meta"]["completed_at"] is None
    assert "cleanup" not in state["prime"]
    assert state["runtime"]["last_run"]["completed_at"]


@requires_bash
def test_second_signal_during_cleanup_exits_immediately(tmp_path: Path) -> None:
    """A second SIGINT while `finally:` is still running ends the process
    at once, same exit code, no traceback (#356) — not an uncaught
    exception escaping the first signal's own cleanup."""
    pidfile = tmp_path / "pid_0"
    started = tmp_path / "started_0"
    cleanup_started = tmp_path / "cleanup_started"
    body = f"""
effects:
  - type: dynamic
    name: d1
    flow: tree
    max_concurrency: 1
    effects:
      - type: tool
        name: b0
        provider: shell
        params:
          command: bash
          args: ["-c", "echo $$ > {pidfile}; touch {started}; sleep {_BRANCH_SLEEP_SECONDS}"]
          allowed_commands: ["bash"]
    finally:
      - type: tool
        name: cleanup
        provider: shell
        params:
          command: bash
          args: ["-c", "touch {cleanup_started}; sleep 5"]
          allowed_commands: ["bash"]
""".lstrip("\n")
    orch = tmp_path / "second_signal.yml"
    orch.write_text(body, encoding="utf-8")
    out_path = tmp_path / "out.json"
    proc = _run_cof(orch, out_path=out_path)

    try:
        _wait_for_paths([started])
    except TimeoutError:
        proc.kill()
        proc.communicate(timeout=15)
        raise

    t0 = time.monotonic()
    proc.send_signal(signal.SIGINT)
    try:
        _wait_for_paths([cleanup_started], timeout=10.0)
    except TimeoutError:
        proc.kill()
        proc.communicate(timeout=15)
        raise
    # cleanup's own `sleep 5` is still running — exactly the "cleanup still
    # in progress" window the second signal must cut through at once.
    proc.send_signal(signal.SIGINT)
    try:
        stdout, stderr = proc.communicate(timeout=_STOP_BOUND_SECONDS + 5)
    except subprocess.TimeoutExpired:
        proc.kill()
        raise
    elapsed = time.monotonic() - t0

    assert proc.returncode == 130, (stdout, stderr)
    assert elapsed < _STOP_BOUND_SECONDS
    assert "Traceback" not in stderr, stderr


def _interrupted_dynamic_orchestration(tmp_path: Path) -> tuple[Path, Path]:
    """A named chain-flow `dynamic` with a quick step, a slow (killable)
    step, and a step after it that must never run before the interrupted
    step does. Returns (orch_path, started_marker) for the slow step.

    Chain flow (not tree) on purpose: this reproduces the orchestrator's
    own follow-up finding on #356, not the parallel-stopping bug above —
    an interrupted *named dynamic*'s own ``meta.error`` must never be
    empty, regardless of its ``flow``, or `cof run --resume` reads it as
    finished and skips every child inside it, including the ones that
    never ran at all. step_b's own script only sleeps the *first* time (it
    checks its own 'started' marker): long enough for the interrupt window
    below, but a resumed rerun of this same unfinished step must finish
    quickly rather than sleeping the full duration all over again.
    """
    started = tmp_path / "slow_started"
    script = (
        f"if [ -f {started} ]; then exit 0; fi; "
        f"touch {started}; sleep {_BRANCH_SLEEP_SECONDS}"
    )
    body = f"""
effects:
  - type: dynamic
    name: d1
    effects:
      - type: tool
        name: step_a
        provider: uuid
      - type: tool
        name: step_b
        provider: shell
        params:
          command: bash
          args: ["-c", "{script}"]
          allowed_commands: ["bash"]
      - type: tool
        name: step_c
        provider: uuid
""".lstrip("\n")
    orch = tmp_path / "interrupted_dynamic.yml"
    orch.write_text(body, encoding="utf-8")
    return orch, started


@requires_bash
def test_resume_after_cancel_reruns_an_interrupted_dynamics_children(
    tmp_path: Path,
) -> None:
    """An interrupted named `dynamic`'s own meta.error must never be empty
    (str(KeyboardInterrupt()) == '') — otherwise `core.resume.
    effect_completed_ok` (completed_at set, no error) reads it as finished
    cleanly, and `cof run --resume` skips the whole dynamic, including
    step_b/step_c, which never ran at all.
    """
    orch, started = _interrupted_dynamic_orchestration(tmp_path)
    out_path = tmp_path / "out.json"
    proc = _run_cof(orch, out_path=out_path)

    try:
        _wait_for_paths([started])
    except TimeoutError:
        proc.kill()
        proc.communicate(timeout=15)
        raise

    proc.send_signal(signal.SIGINT)
    try:
        stdout, stderr = proc.communicate(timeout=_STOP_BOUND_SECONDS + 5)
    except subprocess.TimeoutExpired:
        proc.kill()
        raise
    assert proc.returncode == 130, (stdout, stderr)

    before = json.loads(out_path.read_text(encoding="utf-8"))
    d1_before = before["prime"]["d1"]
    assert d1_before["meta"]["error"], "an interrupted dynamic's meta.error must not be empty"
    assert "step_c" not in d1_before
    step_a_before = d1_before["step_a"]

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
    d1 = state["prime"]["d1"]
    # step_a was already finished before the interrupt — skipped, not rerun.
    assert d1["step_a"]["value"] == step_a_before["value"]
    assert d1["step_a"]["meta"]["completed_at"] == step_a_before["meta"]["completed_at"]
    # step_b/step_c never ran before the interrupt — the whole point of this
    # test is that resume must still reach them, not skip the whole dynamic.
    assert d1["step_b"]["meta"]["completed_at"]
    assert d1["step_c"]["meta"]["completed_at"]
