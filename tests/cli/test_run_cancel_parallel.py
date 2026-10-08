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
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest
from _signal_test_support import (
    _diagnose_and_fail,
    _pid_alive,
    _sandboxed_env,
    _wait_for_paths,
)

requires_bash = pytest.mark.skipif(
    shutil.which("bash") is None, reason="requires the 'bash' binary"
)

#: Each branch sleeps this long if never interrupted — long enough that a
#: run only finishes early because it was actually cancelled, never because
#: the branch happened to complete on its own first.
_BRANCH_SLEEP_SECONDS = 60

#: How long a cancelled run may take to actually exit. Loose on purpose —
#: what matters is "nowhere near _BRANCH_SLEEP_SECONDS", not a tight bound
#: (#356's own rationale), derived from it rather than a literal so the
#: two can't quietly drift apart (#385 review). #385 first widened this to
#: 30s to tolerate what looked like scheduling starvation under heavy
#: machine load; investigating the one real CI failure found a genuine
#: bug instead — a signal landing on a tree-flow/parallel-loop *worker*
#: thread (not this process's main thread) was never noticed until the
#: branch finished on its own, because `as_completed()` sat in one
#: unbounded wait the whole time (now fixed: `core.cancellation.
#: as_completed_promptly`, used by `core.dynamic`/`core.loop`). With that
#: fixed, this goes back to a tight-ish bound — `test_run_sighup_pty.py`'s
#: own 20.0 has never been reported flaky even under load.
_STOP_BOUND_SECONDS = _BRANCH_SLEEP_SECONDS / 3

#: How long `communicate()`/`wait()` are given to actually observe the
#: child exit before a timeout here is treated as a real failure — wider
#: than `_STOP_BOUND_SECONDS` itself, so a genuinely slow (not hung)
#: machine gets a little headroom to actually read the exit before the
#: test gives up; `elapsed < _STOP_BOUND_SECONDS` below is what actually
#: proves promptness once the child does exit. Still well under
#: `_BRANCH_SLEEP_SECONDS` (asserted below) so a run that was genuinely
#: never cancelled still fails here rather than quietly waiting it out.
#: `core.cancellation.wait_for_cancelled_branches` itself now waits with
#: no total time limit of its own (#385 follow-up — an earlier revision's
#: fixed grace period there let a branch outlive `token.reset()` and
#: start a further effect uncancelled), so the only thing the headroom
#: above `_STOP_BOUND_SECONDS` still covers is a killed branch's worker
#: thread actually being scheduled and noticed, ordinarily near-instant.
_COMMUNICATE_TIMEOUT_SECONDS = _STOP_BOUND_SECONDS + 10.0
assert _COMMUNICATE_TIMEOUT_SECONDS < _BRANCH_SLEEP_SECONDS


def _branch_tool(name: str, *, pidfile: Path, started: Path) -> str:
    """A `tool: shell` effect: records its own pid and a 'started' marker
    before sleeping, so a test can wait for (and later assert on) exactly
    those two things without a fixed sleep of its own."""
    # `exec` the tail command (#385 round 3): plain `sleep N` as the last
    # of several `;`-separated commands makes macOS's /bin/bash (3.2) fork
    # a *child* process to run it rather than exec into it, so the pid
    # recorded above is bash's own, not the long-lived process -- a
    # cancellation's `killpg` on that pid's process group then races the
    # kernel's own registration of that just-forked child into the group,
    # which can (rarely, confirmed under load) leave it alive, still
    # holding this step's own stdout/stderr pipes open past bash's own
    # death and the run that is waiting for them to close. `exec` removes
    # the extra process entirely: bash becomes `sleep`, same pid, so
    # there is nothing left to race.
    script = f"echo $$ > {pidfile}; touch {started}; exec sleep {_BRANCH_SLEEP_SECONDS}"
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
        f"echo $$ > {pidfiles[i]}; touch {started[i]}; exec sleep {_BRANCH_SLEEP_SECONDS}"
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
        _diagnose_and_fail(proc, timeout=20.0, label="waiting for the branches to start")

    t0 = time.monotonic()
    proc.send_signal(sig)
    try:
        stdout, stderr = proc.communicate(timeout=_COMMUNICATE_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        _diagnose_and_fail(
            proc,
            timeout=_COMMUNICATE_TIMEOUT_SECONDS,
            label="waiting for the signal to stop the run",
            elapsed=time.monotonic() - t0,
            pid=int(pidfiles[0].read_text(encoding="utf-8").strip())
            if pidfiles[0].exists()
            else None,
        )
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
        _diagnose_and_fail(proc, timeout=20.0, label="waiting for the passes to start")

    t0 = time.monotonic()
    proc.send_signal(sig)
    try:
        stdout, stderr = proc.communicate(timeout=_COMMUNICATE_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        _diagnose_and_fail(
            proc,
            timeout=_COMMUNICATE_TIMEOUT_SECONDS,
            label="waiting for the signal to stop the run",
            elapsed=time.monotonic() - t0,
            pid=int(pidfiles[0].read_text(encoding="utf-8").strip())
            if pidfiles[0].exists()
            else None,
        )
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
          args: ["-c", "echo $$ > {pidfile}; touch {started}; exec sleep {_BRANCH_SLEEP_SECONDS}"]
          allowed_commands: ["bash"]
    finally:
      - type: tool
        name: cleanup
        provider: shell
        params:
          command: bash
          args: ["-c", "touch {cleanup_started}; exec sleep 5"]
          allowed_commands: ["bash"]
""".lstrip("\n")
    orch = tmp_path / "second_signal.yml"
    orch.write_text(body, encoding="utf-8")
    out_path = tmp_path / "out.json"
    proc = _run_cof(orch, out_path=out_path)

    try:
        _wait_for_paths([started])
    except TimeoutError:
        _diagnose_and_fail(proc, timeout=20.0, label="waiting for the branch to start")

    t0 = time.monotonic()
    proc.send_signal(signal.SIGINT)
    try:
        _wait_for_paths([cleanup_started])
    except TimeoutError:
        _diagnose_and_fail(
            proc,
            timeout=20.0,
            label="waiting for cleanup to start after the first signal",
            elapsed=time.monotonic() - t0,
            pid=int(pidfile.read_text(encoding="utf-8").strip()),
        )
    # cleanup's own `sleep 5` is still running — exactly the "cleanup still
    # in progress" window the second signal must cut through at once.
    proc.send_signal(signal.SIGINT)
    try:
        stdout, stderr = proc.communicate(timeout=_COMMUNICATE_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        _diagnose_and_fail(
            proc,
            timeout=_COMMUNICATE_TIMEOUT_SECONDS,
            label="waiting for the second signal to end the run",
            elapsed=time.monotonic() - t0,
            pid=int(pidfile.read_text(encoding="utf-8").strip()),
        )
    elapsed = time.monotonic() - t0

    assert proc.returncode == 130, (stdout, stderr)
    assert elapsed < _STOP_BOUND_SECONDS
    assert "Traceback" not in stderr, stderr
    # Isolates this test from the first signal's own (uninterrupted)
    # cleanup path (#356 review F6): `os._exit` skips writing `--out`
    # entirely, so its presence would mean this test actually exercised a
    # plain re-raised KeyboardInterrupt from inside cleanup rather than
    # the second-signal path it's named for.
    assert not out_path.exists()


@requires_bash
def test_cancel_loop_body_on_error_continue_does_not_start_next_effect(
    tmp_path: Path,
) -> None:
    """A branch-level `on_error: continue` must not let a cancelled run's
    worker start the body's next effect (#356 review F3): `a`'s killed
    subprocess is a nonzero exit that `on_error: continue` swallows
    (ToolRuntime.execute returns normally instead of raising), so only an
    explicit cancellation check before `b` — not `a`'s own exception path
    — can stop this loop body from dispatching a second subprocess
    nothing would ever track or kill.
    """
    a_started = tmp_path / "a_started"
    a_pidfile = tmp_path / "a_pid"
    b_started = tmp_path / "b_started"
    body = f"""
effects:
  - type: loop
    name: lp
    flow: tree
    max_concurrency: 1
    each: {{in: input.items, as: x}}
    body:
      - type: tool
        name: a
        provider: shell
        on_error: continue
        params:
          command: bash
          args: ["-c", "echo $$ > {a_pidfile}; touch {a_started}; exec sleep {_BRANCH_SLEEP_SECONDS}"]
          allowed_commands: ["bash"]
      - type: tool
        name: b
        provider: shell
        params:
          command: bash
          args: ["-c", "touch {b_started}; exec sleep {_BRANCH_SLEEP_SECONDS}"]
          allowed_commands: ["bash"]
""".lstrip("\n")
    orch = tmp_path / "loop_on_error_continue.yml"
    orch.write_text(body, encoding="utf-8")
    state_path = tmp_path / "initial_state.json"
    state_path.write_text(json.dumps({"input": {"items": [1]}}), encoding="utf-8")
    out_path = tmp_path / "out.json"
    proc = _run_cof(orch, out_path=out_path, state_path=state_path)

    try:
        _wait_for_paths([a_started])
    except TimeoutError:
        _diagnose_and_fail(proc, timeout=20.0, label="waiting for a to start")

    t0 = time.monotonic()
    proc.send_signal(signal.SIGINT)
    try:
        stdout, stderr = proc.communicate(timeout=_COMMUNICATE_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        _diagnose_and_fail(
            proc,
            timeout=_COMMUNICATE_TIMEOUT_SECONDS,
            label="waiting for the signal to stop the run",
            elapsed=time.monotonic() - t0,
            pid=int(a_pidfile.read_text(encoding="utf-8").strip())
            if a_pidfile.exists()
            else None,
        )
    elapsed = time.monotonic() - t0

    assert proc.returncode == 130, (stdout, stderr)
    assert elapsed < _STOP_BOUND_SECONDS, (
        f"took {elapsed:.1f}s to stop; a sleeps {_BRANCH_SLEEP_SECONDS}s"
    )
    assert "Traceback" not in stderr, stderr

    pid = int(a_pidfile.read_text(encoding="utf-8").strip())
    assert not _pid_alive(pid), f"pid {pid} survived cancellation"
    # The fix under test: on_error: continue swallowing a's failure must
    # not let this cancelled run dispatch b at all.
    assert not b_started.exists()


@requires_bash
def test_cancel_finally_with_nested_container_still_runs(tmp_path: Path) -> None:
    """A `finally:` containing its own container (dynamic/loop/use/
    conditional) must still run in full after a signal (#356 review F1) —
    not abort the moment that nested container's own cancellation check
    sees the already-cancelled token and raises past the rest of the
    `finally:` list.
    """
    started = tmp_path / "started"
    finally_marker = tmp_path / "finally_marker"
    body = f"""
effects:
  - type: dynamic
    name: d1
    flow: tree
    effects:
      - type: tool
        name: b0
        provider: shell
        params:
          command: bash
          args: ["-c", "touch {started}; exec sleep {_BRANCH_SLEEP_SECONDS}"]
          allowed_commands: ["bash"]
    finally:
      - type: dynamic
        name: cleanup_tree
        flow: tree
        effects:
          - type: tool
            name: c0
            provider: shell
            params:
              command: bash
              args: ["-c", "touch {finally_marker}"]
              allowed_commands: ["bash"]
""".lstrip("\n")
    orch = tmp_path / "finally_nested_container.yml"
    orch.write_text(body, encoding="utf-8")
    out_path = tmp_path / "out.json"
    proc = _run_cof(orch, out_path=out_path)

    try:
        _wait_for_paths([started])
    except TimeoutError:
        _diagnose_and_fail(proc, timeout=20.0, label="waiting for the branch to start")

    t0 = time.monotonic()
    proc.send_signal(signal.SIGINT)
    try:
        stdout, stderr = proc.communicate(timeout=_COMMUNICATE_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        _diagnose_and_fail(
            proc,
            timeout=_COMMUNICATE_TIMEOUT_SECONDS,
            label="waiting for the signal to stop the run",
            elapsed=time.monotonic() - t0,
        )
    elapsed = time.monotonic() - t0

    assert proc.returncode == 130, (stdout, stderr)
    assert elapsed < _STOP_BOUND_SECONDS
    assert "Traceback" not in stderr, stderr
    # The fix under test: the nested dynamic inside `finally:` ran to
    # completion instead of raising RunCancelledBySignal on its own first
    # cancellation check.
    assert finally_marker.exists()


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
        f"touch {started}; exec sleep {_BRANCH_SLEEP_SECONDS}"
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
        _diagnose_and_fail(proc, timeout=20.0, label="waiting for step_b to start")

    t0 = time.monotonic()
    proc.send_signal(signal.SIGINT)
    try:
        stdout, stderr = proc.communicate(timeout=_COMMUNICATE_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        _diagnose_and_fail(
            proc,
            timeout=_COMMUNICATE_TIMEOUT_SECONDS,
            label="waiting for the signal to stop the run",
            elapsed=time.monotonic() - t0,
        )
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
