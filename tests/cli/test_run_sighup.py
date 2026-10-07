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
#: what matters is "nowhere near _BRANCH_SLEEP_SECONDS".
_STOP_BOUND_SECONDS = 10.0

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


def _wait_for_paths(paths: list[Path], *, timeout: float = 15.0) -> None:
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
        stdout, stderr = proc.communicate(timeout=_STOP_BOUND_SECONDS + 5)
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
        returncode = proc.wait(timeout=_STOP_BOUND_SECONDS + 5)
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
