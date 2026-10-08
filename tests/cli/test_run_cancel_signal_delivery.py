"""#385 review: two scenarios the original fix pass's own tests never drove
(both exercise `core.dynamic`/`core.loop` themselves, not just the
`as_completed_promptly`/`wait_for_cancelled_branches` helpers in isolation).

1. A step a cancellation genuinely cannot reach (no `run_tracked`, no
   `get_token().check()` of its own — the orchestration reference names
   an in-flight MCP call and the `service` tool's own subprocess as real
   examples; `_fixtures/run_with_uncancellable_step.py`'s `test_block`
   plugin stands in for one with a plain Python-level sleep). Ctrl-C
   during that step must not make the run exit before it returns, and
   must still stop the *next* step (a marker write) from ever starting —
   for both a tree-flow `dynamic` branch and a parallel `loop` pass.

2. A *second* SIGINT delivered to a worker thread rather than the main
   thread — forced via `signal.pthread_kill`, the same deterministic
   shape `tests/core/test_cancellation.py`'s own pthread_kill tests use,
   since POSIX may route a real second Ctrl-C to any thread that doesn't
   block it and CPython only ever runs the registered handler on the
   main thread regardless. The process must still end at once, with the
   second signal's own exit code, while the main thread is itself
   parked in `wait_for_cancelled_branches`'s own wait for that same
   still-running branch.
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

from _signal_test_support import _diagnose_and_fail, _wait_for_paths

_FIXTURE = Path(__file__).parent / "_fixtures" / "run_with_uncancellable_step.py"

#: The uncancellable step's own sleep, for the "waits for it" tests —
#: short, so the test stays fast, but long enough that "the run ends only
#: after this step returns" is clearly distinguishable from "the run
#: ended as soon as the signal landed".
_BLOCK_SECONDS = 3.0

#: How far past the step's own end this waits before asserting its marker
#: never appeared (the orchestrator's own "wait a few seconds past the
#: step's end" — a marker that was merely slow to write, not one that was
#: genuinely never started, must not pass this test by accident).
_PAST_END_SECONDS = 2.0

_CREDENTIAL_ENV_VARS = (
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "CYBERDINER_TOKEN",
    "CYBERDINER_EXPO_URL",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "NPM_TOKEN",
)


def _run_fixture(orch: Path, *, state_path: Path | None = None) -> subprocess.Popen[str]:
    tmp_path = orch.parent
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env = {k: v for k, v in os.environ.items() if k not in _CREDENTIAL_ENV_VARS}
    env["HOME"] = str(home)
    env["PYTHONFAULTHANDLER"] = "1"
    args = [sys.executable, str(_FIXTURE), str(orch)]
    if state_path is not None:
        args.append(str(state_path))
    return subprocess.Popen(
        args,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
        cwd=tmp_path,
    )


def _tree_dynamic_with_uncancellable_first_step(
    tmp_path: Path,
) -> tuple[Path, Path, Path, Path]:
    """A `flow: tree` `dynamic` with one branch that is itself a chain of
    two steps: an uncancellable sleep, then a marker write. Returns
    (orch_path, started, done, marker)."""
    started = tmp_path / "started"
    done = tmp_path / "done"
    marker = tmp_path / "marker"
    body = f"""
effects:
  - type: dynamic
    name: d1
    flow: tree
    effects:
      - type: dynamic
        name: b0
        effects:
          - type: tool
            name: step1
            provider: test_block
            params:
              started: {started}
              done: {done}
              seconds: {_BLOCK_SECONDS}
          - type: tool
            name: step2
            provider: fs
            params:
              mode: write
              path: {marker}
              content: "x"
""".lstrip("\n")
    orch = tmp_path / "tree_uncancellable.yml"
    orch.write_text(body, encoding="utf-8")
    return orch, started, done, marker


def _parallel_loop_with_uncancellable_first_step(
    tmp_path: Path,
) -> tuple[Path, Path, Path, Path]:
    """A `flow: tree` `loop` (a single pass is enough: the point is one
    worker thread running a chain of two body steps) with the same
    uncancellable-sleep-then-marker shape. Returns
    (orch_path, started, done, marker)."""
    started = tmp_path / "started"
    done = tmp_path / "done"
    marker = tmp_path / "marker"
    body = f"""
effects:
  - type: loop
    name: lp
    flow: tree
    each: {{in: input.items, as: x}}
    body:
      - type: tool
        name: step1
        provider: test_block
        params:
          started: {started}
          done: {done}
          seconds: {_BLOCK_SECONDS}
      - type: tool
        name: step2
        provider: fs
        params:
          mode: write
          path: {marker}
          content: "x"
""".lstrip("\n")
    orch = tmp_path / "loop_uncancellable.yml"
    orch.write_text(body, encoding="utf-8")
    return orch, started, done, marker


def _assert_waits_for_the_uncancellable_step_then_never_writes_the_marker(
    proc: subprocess.Popen[str], *, started: Path, done: Path, marker: Path
) -> None:
    try:
        _wait_for_paths([started])
    except TimeoutError:
        _diagnose_and_fail(proc, timeout=20.0, label="waiting for step1 to start")

    t0 = time.monotonic()
    proc.send_signal(signal.SIGINT)
    try:
        stdout, stderr = proc.communicate(timeout=_BLOCK_SECONDS + 20.0)
    except subprocess.TimeoutExpired:
        _diagnose_and_fail(
            proc,
            timeout=_BLOCK_SECONDS + 20.0,
            label="waiting for the run to exit after step1 returns",
            elapsed=time.monotonic() - t0,
        )
    elapsed = time.monotonic() - t0

    assert proc.returncode == 130, (stdout, stderr)
    # The run ends only once step1 actually returns -- not as soon as the
    # signal landed, and not instantly: the kill genuinely couldn't reach
    # it, so this is the one place in this whole suite where "promptly"
    # does NOT mean "much less than the step's own sleep".
    assert elapsed >= _BLOCK_SECONDS * 0.8, (
        f"exited after {elapsed:.1f}s, well before step1's own "
        f"{_BLOCK_SECONDS}s sleep -- the cancellation reached a step it "
        "must not be able to"
    )
    assert done.exists(), "step1 itself must have run to completion"
    assert "Traceback" not in stderr, stderr

    # Give a marker that was merely slow to write every chance to show up
    # before concluding it never will.
    time.sleep(_PAST_END_SECONDS)
    assert not marker.exists(), (
        "step2 must never start once cancellation was requested, even "
        "though step1 (which it followed) ignored it entirely"
    )


def test_tree_dynamic_branch_the_kill_cannot_reach_delays_exit_and_blocks_next_step(
    tmp_path: Path,
) -> None:
    orch, started, done, marker = _tree_dynamic_with_uncancellable_first_step(tmp_path)
    proc = _run_fixture(orch)
    _assert_waits_for_the_uncancellable_step_then_never_writes_the_marker(
        proc, started=started, done=done, marker=marker
    )


def test_parallel_loop_pass_the_kill_cannot_reach_delays_exit_and_blocks_next_step(
    tmp_path: Path,
) -> None:
    orch, started, done, marker = _parallel_loop_with_uncancellable_first_step(
        tmp_path
    )
    state_path = tmp_path / "initial_state.json"
    state_path.write_text(json.dumps({"input": {"items": [1]}}), encoding="utf-8")
    proc = _run_fixture(orch, state_path=state_path)
    _assert_waits_for_the_uncancellable_step_then_never_writes_the_marker(
        proc, started=started, done=done, marker=marker
    )


def test_second_sigint_delivered_to_a_worker_thread_ends_process_promptly(
    tmp_path: Path,
) -> None:
    """#385 review required test 2: the second SIGINT is self-delivered by
    the branch's own worker thread via `signal.pthread_kill` (see the
    fixture's `test_block` plugin) -- standing in for POSIX routing a
    user's real second Ctrl-C to that thread instead of the main one,
    which CPython would still only ever run the registered handler for on
    the main thread. The main thread is confirmed to be inside
    `wait_for_cancelled_branches`'s own wait for this same branch at the
    moment the second signal lands, because the branch's own sleep
    (10s) vastly outlasts the delay before it self-delivers (1s).
    """
    started = tmp_path / "started"
    done = tmp_path / "done"
    body = f"""
effects:
  - type: dynamic
    name: d1
    flow: tree
    effects:
      - type: tool
        name: b0
        provider: test_block
        params:
          started: {started}
          done: {done}
          seconds: 10
          second_signal_delay: 1.0
""".lstrip("\n")
    orch = tmp_path / "second_signal_worker_thread.yml"
    orch.write_text(body, encoding="utf-8")
    proc = _run_fixture(orch)

    try:
        _wait_for_paths([started])
    except TimeoutError:
        _diagnose_and_fail(proc, timeout=20.0, label="waiting for the branch to start")

    t0 = time.monotonic()
    proc.send_signal(signal.SIGINT)
    try:
        stdout, stderr = proc.communicate(timeout=15.0)
    except subprocess.TimeoutExpired:
        _diagnose_and_fail(
            proc,
            timeout=15.0,
            label="waiting for the second (worker-thread) signal to end the run",
            elapsed=time.monotonic() - t0,
        )
    elapsed = time.monotonic() - t0

    assert proc.returncode == 130, (stdout, stderr)
    # Ends around the 1s self-delivery delay, nowhere near the branch's
    # own 10s sleep -- proof the second signal was noticed promptly even
    # though it landed on the worker thread while the main thread was
    # itself parked waiting for that same branch.
    assert elapsed < 5.0, (
        f"took {elapsed:.1f}s to exit after the second signal; branch "
        "sleeps 10s -- the worker-thread-delivered second signal wasn't "
        "noticed promptly"
    )
    assert "Traceback" not in stderr, stderr
    assert not done.exists(), "os._exit must end the process before step1 returns"
