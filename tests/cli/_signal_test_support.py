"""Shared helpers for the signal-cancellation CLI test suite (#385 review
P2): a sandboxed `cof run` child env, a poll-until-exists wait, and the
SIGABRT-plus-stack-dump timeout diagnostics every one of those test
modules otherwise defined (word for word) on its own.
"""

from __future__ import annotations

import os
import signal
import subprocess
import time
from pathlib import Path

import pytest

#: Credential env vars that must never reach a spawned `cof run` here —
#: none of this suite configures a real adapter, so none of them are
#: needed, and their presence must not change anything it asserts (#356's
#: own test plan).
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
    credentials — this suite has no adapter to use them, real or fake.

    ``PYTHONFAULTHANDLER=1`` costs nothing on a run that exits normally —
    it only matters the moment something here times out (see
    ``_diagnose_and_fail``): it lets a SIGABRT sent to a stuck child dump
    every thread's Python stack to stderr before it dies, instead of a
    bare `TimeoutExpired`/`TimeoutError` with nothing to debug a future
    CI failure from (#385 review).
    """
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env = {k: v for k, v in os.environ.items() if k not in _CREDENTIAL_ENV_VARS}
    env["HOME"] = str(home)
    env["PYTHONFAULTHANDLER"] = "1"
    return env


def _wait_for_paths(paths: list[Path], *, timeout: float = 20.0) -> None:
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


def _diagnosis_header(
    *, label: str, timeout: float, elapsed: float | None, pid: int | None
) -> str:
    """The part of a timeout failure that's the same whether or not
    stdout/stderr could be read back (#385 review: every timeout failure
    here states how long it actually waited and whether a branch's own
    subprocess is still alive, not just that something eventually gave
    up)."""
    bits = [f"after {timeout:.0f}s"]
    if elapsed is not None:
        bits.append(f"elapsed={elapsed:.1f}s")
    if pid is not None:
        bits.append(f"branch pid {pid} alive={_pid_alive(pid)}")
    return f"{label}: child still running ({', '.join(bits)})"


def _diagnose_and_fail(
    proc: subprocess.Popen[str],
    *,
    timeout: float,
    label: str,
    elapsed: float | None = None,
    pid: int | None = None,
) -> None:
    """Fail the test with everything needed to debug a stuck `cof run`
    child: its own stdout/stderr, plus (via SIGABRT + this module's own
    `PYTHONFAULTHANDLER=1`) a dump of every thread's Python stack — never
    a bare `TimeoutExpired`/`TimeoutError` with none of that (#385
    review).

    SIGABRT, not SIGKILL: faulthandler installs itself for exactly the
    signals a fatal crash would send (SIGABRT included), dumps first,
    then lets the signal's own default disposition finish the job — so
    the child still exits, just not silently. Falls back to SIGKILL only
    if SIGABRT itself doesn't finish the job in time.
    """
    if proc.poll() is None:
        proc.send_signal(signal.SIGABRT)
        try:
            stdout, stderr = proc.communicate(timeout=10.0)
        except subprocess.TimeoutExpired:
            proc.kill()
            stdout, stderr = proc.communicate(timeout=10.0)
    else:
        stdout, stderr = proc.communicate(timeout=10.0)
    header = _diagnosis_header(label=label, timeout=timeout, elapsed=elapsed, pid=pid)
    pytest.fail(
        f"{header}\n"
        f"--- stdout ---\n{stdout}\n"
        "--- stderr (includes a PYTHONFAULTHANDLER stack dump of every "
        "thread, from SIGABRT, if the child was still alive) ---\n"
        f"{stderr}"
    )


def _diagnose_and_fail_no_pipes(
    proc: subprocess.Popen[str],
    *,
    timeout: float,
    label: str,
    elapsed: float | None = None,
    pid: int | None = None,
) -> None:
    """Like `_diagnose_and_fail`, for the one scenario where stdout/stderr
    can't be read here at all: the caller closed its own read end of both
    pipes already, to simulate a closed terminal."""
    if proc.poll() is None:
        proc.send_signal(signal.SIGABRT)
        try:
            returncode = proc.wait(timeout=10.0)
        except subprocess.TimeoutExpired:
            proc.kill()
            returncode = proc.wait(timeout=10.0)
    else:
        returncode = proc.returncode
    header = _diagnosis_header(label=label, timeout=timeout, elapsed=elapsed, pid=pid)
    pytest.fail(
        f"{header} "
        f"(stdout/stderr already closed by this test; returncode after "
        f"SIGABRT+kill: {returncode})"
    )
