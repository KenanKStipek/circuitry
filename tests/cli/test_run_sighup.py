"""`cof run` and a SIGHUP mid-run (#356 follow-up).

A terminal hangup (closing the terminal window, an SSH disconnect)
delivers SIGHUP to `cof`'s own process group only — every tracked child
(`core.cancellation.run_tracked`) is started in its own process group
(`start_new_session=True`), so a bare, unhandled SIGHUP would end `cof`
with no cleanup at all while those children kept running as orphans.
These tests mirror `test_run_sigterm.py`/`test_run_cancel_parallel.py`,
but for SIGHUP: the first SIGHUP must behave exactly like the first
SIGTERM (tracked children killed, `finally:` runs, exit code 129, the run
persisted for `--resume`), whether or not stdout/stderr are still
readable, and SIGHUP already ignored on entry (as `nohup` leaves it) must
be left alone entirely.
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
requires_sighup = pytest.mark.skipif(
    not hasattr(signal, "SIGHUP"), reason="SIGHUP does not exist on this platform"
)

#: Long enough that a run only finishes early because it was actually
#: cancelled, never because the branch happened to complete on its own.
_BRANCH_SLEEP_SECONDS = 60

#: How long a cancelled run may take to actually exit — loose on purpose,
#: what matters is "nowhere near _BRANCH_SLEEP_SECONDS" (half of it, at
#: most), not a few seconds of wall time (#385): under heavy machine load
#: (several full test runs in parallel, or `pytest -n` with many workers),
#: the main thread may sit starved of CPU for well over 10s before it
#: ever gets to run the signal handler that kills the tracked child —
#: observed in CI itself once, and reproduced locally well over half the
#: time under enough concurrent load, with the stuck child's own stack (a
#: stdlib `subprocess.communicate` `select.poll()` and a lock wait,
#: nothing resembling a real deadlock) confirming scheduling starvation
#: rather than a stuck signal/cleanup path.
_STOP_BOUND_SECONDS = 30.0

#: How long `communicate()`/`wait()` are given to actually observe the
#: child exit before a timeout here is treated as a real failure — wider
#: than `_STOP_BOUND_SECONDS` itself (#385), but still well under
#: `_BRANCH_SLEEP_SECONDS` so a run that was never actually cancelled
#: (the real bug this would catch) still fails here rather than quietly
#: passing once the branch finishes on its own; `elapsed <
#: _STOP_BOUND_SECONDS` below is what actually proves promptness once the
#: child does exit.
_COMMUNICATE_TIMEOUT_SECONDS = _STOP_BOUND_SECONDS + 20.0

_CREDENTIAL_ENV_VARS = (
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "CYBERDINER_TOKEN",
    "CYBERDINER_EXPO_URL",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_AUTH_TOKEN",
    "NPM_TOKEN",
    "NPM_TOKEN_GITHUB",
    "NODE_AUTH_TOKEN",
)


def _sandboxed_env(tmp_path: Path) -> dict[str, str]:
    """A child-process env with a fake $HOME (CLAUDE.md hermeticity) and no
    credentials — this suite has no adapter to use them, real or fake."""
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env = {k: v for k, v in os.environ.items() if k not in _CREDENTIAL_ENV_VARS}
    env["HOME"] = str(home)
    return env


def _wait_for_paths(paths: list[Path], *, timeout: float = 30.0) -> None:
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


def _interrupted_tree_dynamic_orchestration(tmp_path: Path) -> tuple[Path, Path, Path]:
    """A chain-flow `step_a`, then a tree-flow `dynamic` with one slow
    shell branch and its own `finally:`, then `step_c`. The branch's own
    script is idempotent (exits at once if its own 'started' marker is
    already there) so a resumed rerun of this same unfinished branch
    finishes quickly instead of sleeping the full duration all over again.
    Returns (orch_path, pidfile, started_marker).
    """
    pidfile = tmp_path / "branch_pid"
    started = tmp_path / "branch_started"
    script = (
        f"if [ -f {started} ]; then exit 0; fi; "
        f"echo $$ > {pidfile}; touch {started}; sleep {_BRANCH_SLEEP_SECONDS}"
    )
    body = f"""
effects:
  - type: tool
    name: step_a
    provider: uuid
  - type: dynamic
    name: d1
    flow: tree
    effects:
      - type: tool
        name: b0
        provider: shell
        params:
          command: bash
          args: ["-c", "{script}"]
          allowed_commands: ["bash"]
    finally:
      - type: tool
        name: cleanup
        provider: uuid
  - type: tool
    name: step_c
    provider: uuid
""".lstrip("\n")
    orch = tmp_path / "sighup_tree_dynamic.yml"
    orch.write_text(body, encoding="utf-8")
    return orch, pidfile, started


def _run_cof_popen(
    orch: Path, *, out_path: Path, pipes: bool = True
) -> subprocess.Popen[str]:
    tmp_path = orch.parent
    args = [
        sys.executable, "-m", "circuitry.cli.app", "run", str(orch),
        "--out", str(out_path), "--quiet",
    ]
    return subprocess.Popen(
        args,
        stdout=subprocess.PIPE if pipes else subprocess.DEVNULL,
        stderr=subprocess.PIPE if pipes else subprocess.DEVNULL,
        text=True,
        env=_sandboxed_env(tmp_path),
        cwd=tmp_path,
    )


def _resume_reruns_unfinished_work(orch: Path, out_path: Path) -> None:
    """Shared tail for (a)/(b): `--resume` must still reach step_b/step_c,
    not skip the whole cancelled dynamic (#356 follow-up's own meta.error
    requirement)."""
    before = json.loads(out_path.read_text(encoding="utf-8"))
    d1_before = before["prime"]["d1"]
    assert d1_before["meta"]["error"], "an interrupted dynamic's meta.error must not be empty"
    assert "step_c" not in before["prime"]

    tmp_path = orch.parent
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
    assert state["prime"]["step_c"]["meta"]["completed_at"]


@requires_bash
@requires_sighup
def test_sighup_mid_run_kills_branch_runs_finally_and_is_resumable(
    tmp_path: Path,
) -> None:
    orch, pidfile, started = _interrupted_tree_dynamic_orchestration(tmp_path)
    out_path = tmp_path / "out.json"
    proc = _run_cof_popen(orch, out_path=out_path)

    try:
        _wait_for_paths([started])
    except TimeoutError:
        proc.kill()
        proc.communicate(timeout=15)
        raise

    t0 = time.monotonic()
    proc.send_signal(signal.SIGHUP)
    try:
        stdout, stderr = proc.communicate(timeout=_COMMUNICATE_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        proc.kill()
        raise
    elapsed = time.monotonic() - t0

    assert proc.returncode == 129, (stdout, stderr)
    assert elapsed < _STOP_BOUND_SECONDS, (
        f"took {elapsed:.1f}s to stop; branch sleeps {_BRANCH_SLEEP_SECONDS}s"
    )
    assert "Traceback" not in stderr, stderr

    pid = int(pidfile.read_text(encoding="utf-8").strip())
    assert not _pid_alive(pid), f"pid {pid} survived the hangup"

    assert out_path.exists()
    state = json.loads(out_path.read_text(encoding="utf-8"))
    d1 = state["prime"]["d1"]
    assert d1["meta"]["completed_at"]
    assert d1["meta"]["error"] == "Interrupted (SIGHUP)"
    assert d1["cleanup"]["meta"]["completed_at"]
    assert state["runtime"]["last_run"]["completed_at"]

    _resume_reruns_unfinished_work(orch, out_path)


@requires_bash
@requires_sighup
def test_double_sighup_back_to_back_does_not_abort_cleanup(tmp_path: Path) -> None:
    """A closed terminal can deliver SIGHUP to a foreground `cof run` job
    twice, about a millisecond apart (#357 follow-up), not once — the
    second one must not hit the "second signal" `os._exit` rule a real
    second SIGINT/SIGTERM takes, which would abort the first SIGHUP's own
    cleanup before it finishes. Sending two SIGHUPs back to back from here
    (no real terminal needed) is the direct-subprocess version of that;
    `test_run_sighup_pty.py` drives the same thing through a real pty.

    Not the deterministic regression guard for the no-op rule itself: two
    back-to-back `send_signal` calls this close together usually land
    before the interpreter runs the Python-level handler for the first
    one, so the two deliveries typically collapse into a single handler
    call here. `test_interrupts.py`'s own
    `test_repeated_sighup_while_installed_is_a_no_op_not_a_second_signal`
    (calling the installed handler directly, twice, with no process
    involved) and the real pty test are what actually exercise two
    separate handler invocations; this test stays as an end-to-end check
    that the common case still exits cleanly.
    """
    orch, pidfile, started = _interrupted_tree_dynamic_orchestration(tmp_path)
    out_path = tmp_path / "out.json"
    proc = _run_cof_popen(orch, out_path=out_path)

    try:
        _wait_for_paths([started])
    except TimeoutError:
        proc.kill()
        proc.communicate(timeout=15)
        raise

    t0 = time.monotonic()
    proc.send_signal(signal.SIGHUP)
    proc.send_signal(signal.SIGHUP)
    try:
        stdout, stderr = proc.communicate(timeout=_COMMUNICATE_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        proc.kill()
        raise
    elapsed = time.monotonic() - t0

    assert proc.returncode == 129, (stdout, stderr)
    assert elapsed < _STOP_BOUND_SECONDS, (
        f"took {elapsed:.1f}s to stop; branch sleeps {_BRANCH_SLEEP_SECONDS}s"
    )
    assert "Traceback" not in stderr, stderr

    pid = int(pidfile.read_text(encoding="utf-8").strip())
    assert not _pid_alive(pid), f"pid {pid} survived the double hangup"

    assert out_path.exists()
    state = json.loads(out_path.read_text(encoding="utf-8"))
    d1 = state["prime"]["d1"]
    assert d1["meta"]["completed_at"]
    assert d1["meta"]["error"] == "Interrupted (SIGHUP)"
    assert d1["cleanup"]["meta"]["completed_at"]
    assert state["runtime"]["last_run"]["completed_at"]
    home = orch.parent / "home"
    assert (home / ".config" / "circuitry" / "last-run.json").exists()

    _resume_reruns_unfinished_work(orch, out_path)


@requires_bash
@requires_sighup
@pytest.mark.parametrize("sig,expected_code", [(signal.SIGINT, 130), (signal.SIGTERM, 143)])
def test_second_sigint_or_sigterm_after_first_sighup_exits_immediately(
    tmp_path: Path, sig: int, expected_code: int
) -> None:
    """SIGHUP never counts as the "already cancelled" signal that lets a
    later SIGINT/SIGTERM end the process at once (#357 follow-up) — the
    other direction from `test_double_sighup_back_to_back_does_not_abort_
    cleanup`: a first SIGHUP starts cancellation exactly like a first
    SIGINT/SIGTERM would, so a genuine second signal (SIGINT or SIGTERM,
    not another SIGHUP) arriving while its cleanup is still running must
    still take the immediate `os._exit` path, same as it would after a
    first SIGINT/SIGTERM of its own.
    """
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
    orch = tmp_path / "second_signal_after_sighup.yml"
    orch.write_text(body, encoding="utf-8")
    out_path = tmp_path / "out.json"
    proc = _run_cof_popen(orch, out_path=out_path)

    try:
        _wait_for_paths([started])
    except TimeoutError:
        proc.kill()
        proc.communicate(timeout=15)
        raise

    t0 = time.monotonic()
    proc.send_signal(signal.SIGHUP)
    try:
        _wait_for_paths([cleanup_started])
    except TimeoutError:
        proc.kill()
        proc.communicate(timeout=15)
        raise
    # cleanup's own `sleep 5` is still running — exactly the "cleanup still
    # in progress" window the second signal must cut through at once.
    proc.send_signal(sig)
    try:
        stdout, stderr = proc.communicate(timeout=_COMMUNICATE_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        proc.kill()
        raise
    elapsed = time.monotonic() - t0

    assert proc.returncode == expected_code, (stdout, stderr)
    assert elapsed < _STOP_BOUND_SECONDS
    assert "Traceback" not in stderr, stderr
    # Isolates this from the first SIGHUP's own (uninterrupted) cleanup
    # path: `os._exit` skips writing `--out` entirely, so its presence
    # would mean the second signal was itself silently ignored instead of
    # cutting through cleanup the way a second SIGINT/SIGTERM must.
    assert not out_path.exists()


@requires_bash
@requires_sighup
def test_sighup_with_closed_stdout_stderr_still_cleans_up(tmp_path: Path) -> None:
    """Same scenario, but the terminal is well and truly gone: our own read
    ends of the child's stdout/stderr pipes are closed before the signal
    lands, so any write cof makes from here on raises EPIPE/BrokenPipeError
    — that must not stop `finally:`, the `--out` write, persistence, or the
    exit code (#356 follow-up).
    """
    orch, pidfile, started = _interrupted_tree_dynamic_orchestration(tmp_path)
    out_path = tmp_path / "out.json"
    proc = _run_cof_popen(orch, out_path=out_path)

    try:
        _wait_for_paths([started])
    except TimeoutError:
        proc.kill()
        if proc.stdout is not None:
            proc.stdout.close()
        if proc.stderr is not None:
            proc.stderr.close()
        proc.wait(timeout=15)
        raise

    assert proc.stdout is not None
    assert proc.stderr is not None
    proc.stdout.close()
    proc.stderr.close()

    t0 = time.monotonic()
    proc.send_signal(signal.SIGHUP)
    try:
        returncode = proc.wait(timeout=_COMMUNICATE_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        proc.kill()
        raise
    elapsed = time.monotonic() - t0

    assert returncode == 129
    assert elapsed < _STOP_BOUND_SECONDS, (
        f"took {elapsed:.1f}s to stop; branch sleeps {_BRANCH_SLEEP_SECONDS}s"
    )

    pid = int(pidfile.read_text(encoding="utf-8").strip())
    assert not _pid_alive(pid), f"pid {pid} survived the hangup"

    assert out_path.exists()
    state = json.loads(out_path.read_text(encoding="utf-8"))
    d1 = state["prime"]["d1"]
    assert d1["meta"]["completed_at"]
    assert d1["meta"]["error"] == "Interrupted (SIGHUP)"
    assert d1["cleanup"]["meta"]["completed_at"]
    assert state["runtime"]["last_run"]["completed_at"]

    _resume_reruns_unfinished_work(orch, out_path)


@requires_bash
@requires_sighup
def test_sighup_ignored_on_entry_leaves_run_uninterrupted(tmp_path: Path) -> None:
    """SIGHUP already set to SIG_IGN before `cof run` even starts (what
    `nohup` does to its own child before exec'ing it) must be left exactly
    as found — `sigterm_as_interrupt` never installs a handler for it, so
    the run completes normally, as if the signal was never sent at all.
    """
    started = tmp_path / "slow_started"
    body = f"""
effects:
  - type: tool
    name: step_a
    provider: uuid
  - type: tool
    name: slow
    provider: shell
    params:
      command: bash
      args: ["-c", "touch {started}; sleep 2"]
      allowed_commands: ["bash"]
  - type: tool
    name: step_c
    provider: uuid
""".lstrip("\n")
    orch = tmp_path / "sighup_ignored.yml"
    orch.write_text(body, encoding="utf-8")
    out_path = tmp_path / "out.json"

    def _ignore_sighup() -> None:
        # Mirrors what `nohup` does to its own child before exec'ing it:
        # SIG_IGN (and only SIG_IGN) survives an execve, unlike SIG_DFL or
        # a Python callable, which is exactly why this reproduces it
        # without depending on the real `nohup` binary being installed.
        signal.signal(signal.SIGHUP, signal.SIG_IGN)

    proc = subprocess.Popen(
        [
            sys.executable, "-m", "circuitry.cli.app", "run", str(orch),
            "--out", str(out_path), "--quiet",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=_sandboxed_env(tmp_path),
        cwd=orch.parent,
        # Safe here: this test process is single-threaded at the point of
        # the fork (pytest isn't running this test concurrently with any
        # other on a second thread), which is exactly the condition
        # `preexec_fn` is unsafe without.
        preexec_fn=_ignore_sighup,  # noqa: PLW1509
    )

    try:
        _wait_for_paths([started])
    except TimeoutError:
        proc.kill()
        proc.communicate(timeout=15)
        raise

    proc.send_signal(signal.SIGHUP)
    try:
        stdout, stderr = proc.communicate(timeout=15)
    except subprocess.TimeoutExpired:
        proc.kill()
        raise

    assert proc.returncode == 0, (stdout, stderr)
    assert "Traceback" not in stderr, stderr
    state = json.loads(out_path.read_text(encoding="utf-8"))
    assert state["prime"]["step_a"]["meta"]["completed_at"]
    assert state["prime"]["slow"]["meta"]["completed_at"]
    assert state["prime"]["step_c"]["meta"]["completed_at"]
