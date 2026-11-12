"""#741 follow-up: with a live observer (``--live-state``, which osi always
passes), a step whose background process keeps its stdout/stderr pipe open
past the tracked process's own exit must end exactly as it would with no
live wiring at all -- the step's own timeout/cancellation covers process
exit *and* EOF on both pipes (not just process exit, leaving the readers
joined separately with no deadline), neither a timeout nor a cancellation
ever waits for a reader to reach real EOF, and no reader thread ever
blocks inside ``BufferedReader.read1`` (which can abort the interpreter at
shutdown with a blocked daemon thread holding its lock).

Each test here fails against ``f73231f`` (the regression commit): the
timeout/SIGINT cases either hang far past their own deadline waiting for a
background process's pipe (the first two), crash with a shutdown-time
``Fatal Python error`` (the third), or both.
"""

from __future__ import annotations

import json
import signal
import subprocess
import sys
import time
import uuid
from pathlib import Path

import pytest
from _signal_test_support import _sandboxed_env

pytestmark = pytest.mark.skipif(
    not Path("/bin/sh").exists(), reason="requires a POSIX /bin/sh"
)


def _unique_sleep_arg(base_seconds: int) -> str:
    """A fractional ``sleep`` argument unique to this call -- never a
    plain round number another process on the same machine could also
    happen to be sleeping for, so a cleanup ``pkill -f`` naming it can
    never reach an unrelated process (#661)."""
    digits = "".join(c for c in uuid.uuid4().hex if c.isdigit())[:6] or "314159"
    return f"{base_seconds}.{digits}"


def _run_cof(
    orch: Path,
    tmp_path: Path,
    *,
    extra_args: list[str] | None = None,
) -> tuple[subprocess.CompletedProcess[str], Path, Path]:
    out_path = tmp_path / "out.json"
    live_path = tmp_path / "live.json"
    args = [
        sys.executable, "-m", "circuitry.cli.app", "run", str(orch),
        "--out", str(out_path), "--live-state", str(live_path), "--quiet",
        *(extra_args or []),
    ]
    result = subprocess.run(
        args,
        capture_output=True,
        text=True,
        env=_sandboxed_env(tmp_path),
        cwd=tmp_path,
        timeout=60,
        check=False,
    )
    return result, out_path, live_path


def _step_error(out_path: Path, step: str) -> str:
    state = json.loads(out_path.read_text(encoding="utf-8"))
    return state["prime"][step]["meta"]["error"] or ""


def _step_value(out_path: Path, step: str) -> str:
    state = json.loads(out_path.read_text(encoding="utf-8"))
    return state["prime"][step]["value"]


def _pkill(pattern: str) -> None:
    subprocess.run(["pkill", "-f", pattern], check=False)


def test_timeout_with_a_same_group_background_child_matches_main(
    tmp_path: Path,
) -> None:
    """A timed-out step whose own background child (started with a bare
    shell ``&``, same process group) is still holding its stdout open must
    fail at the step's own timeout, exactly as it does with no live
    observer at all -- not succeed minutes later once that background
    child finally exits on its own."""
    sleep_arg = _unique_sleep_arg(9)
    orch = tmp_path / "orch.yml"
    orch.write_text(
        "effects:\n"
        "  - type: tool\n"
        "    name: bg\n"
        "    provider: shell\n"
        "    timeout_ms: 3000\n"
        "    params:\n"
        "      command: sh\n"
        "      allowed_commands: [sh]\n"
        f'      args: ["-c", "sleep {sleep_arg} & echo started"]\n',
        encoding="utf-8",
    )
    try:
        t0 = time.monotonic()
        result, out_path, _live_path = _run_cof(orch, tmp_path)
        elapsed = time.monotonic() - t0

        assert result.returncode == 1, (result.stdout, result.stderr)
        assert 2.5 <= elapsed <= 5.0, (
            f"took {elapsed:.1f}s -- the step's own 3s timeout must cover "
            "waiting for the background child's pipe too, not hang until "
            "it exits on its own"
        )
        assert "exceeded timeout of 3s" in _step_error(out_path, "bg")
        assert "Fatal Python error" not in result.stderr
    finally:
        _pkill(f"sleep {sleep_arg}")


def test_timeout_with_a_detached_child_matches_main(tmp_path: Path) -> None:
    """A timed-out step whose own child forked a *further* detached
    (``start_new_session``) grandchild that still holds stdout open --
    the step's own ``killpg`` cannot reach it, so this must fail at the
    step's own timeout exactly like a plain run with no live wiring,
    leaving the detached grandchild running rather than waiting for it."""
    sleep_arg = _unique_sleep_arg(9)
    orch = tmp_path / "orch.yml"
    orch.write_text(
        "effects:\n"
        "  - type: tool\n"
        "    name: bg\n"
        "    provider: shell\n"
        "    timeout_ms: 3000\n"
        "    params:\n"
        "      command: python3\n"
        "      allowed_commands: [python3]\n"
        "      args: [\"-c\", \"import subprocess; "
        f"subprocess.Popen(['sleep', '{sleep_arg}'], start_new_session=True); "
        "print('started', flush=True)\"]\n",
        encoding="utf-8",
    )
    try:
        t0 = time.monotonic()
        result, out_path, _live_path = _run_cof(orch, tmp_path)
        elapsed = time.monotonic() - t0

        assert result.returncode == 1, (result.stdout, result.stderr)
        assert 2.5 <= elapsed <= 5.0, (
            f"took {elapsed:.1f}s -- a detached grandchild's own pipe must "
            "not be waited past the step's own 3s timeout"
        )
        assert "exceeded timeout of 3s" in _step_error(out_path, "bg")
        assert "Fatal Python error" not in result.stderr
    finally:
        _pkill(f"sleep {sleep_arg}")


def _wait_for_live_stdout(live_path: Path, marker: str, *, timeout: float = 20.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            state = json.loads(live_path.read_text(encoding="utf-8"))
            tail = state["prime"]["bg"]["meta"]["live"]["stdout_tail"]
        except (OSError, ValueError, KeyError, TypeError):
            tail = ""
        if marker in tail:
            return
        time.sleep(0.05)
    raise TimeoutError(f"{marker!r} never showed up in the live stdout tail")


def test_sigint_with_a_detached_child_exits_130_without_crashing(
    tmp_path: Path,
) -> None:
    """SIGINT, no timeout, the same detached-grandchild shape as above --
    the run must still exit 130 at once, the same as it always has with no
    live wiring, not crash with a shutdown-time ``Fatal Python error`` from
    a reader thread still blocked in ``BufferedReader.read1`` on the
    grandchild's own still-open pipe."""
    sleep_arg = _unique_sleep_arg(9)
    orch = tmp_path / "orch.yml"
    orch.write_text(
        "effects:\n"
        "  - type: tool\n"
        "    name: bg\n"
        "    provider: shell\n"
        "    params:\n"
        "      command: python3\n"
        "      allowed_commands: [python3]\n"
        "      args: [\"-c\", \"import subprocess; "
        f"subprocess.Popen(['sleep', '{sleep_arg}'], start_new_session=True); "
        "print('started', flush=True)\"]\n",
        encoding="utf-8",
    )
    out_path = tmp_path / "out.json"
    live_path = tmp_path / "live.json"
    proc = subprocess.Popen(
        [
            sys.executable, "-m", "circuitry.cli.app", "run", str(orch),
            "--out", str(out_path), "--live-state", str(live_path), "--quiet",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=_sandboxed_env(tmp_path),
        cwd=tmp_path,
    )
    try:
        _wait_for_live_stdout(live_path, "started")
        t0 = time.monotonic()
        proc.send_signal(signal.SIGINT)
        try:
            stdout, stderr = proc.communicate(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()
            stdout, stderr = proc.communicate(timeout=10)
            raise AssertionError(
                f"did not exit within 20s of SIGINT\nstdout={stdout}\nstderr={stderr}"
            ) from None
        elapsed = time.monotonic() - t0

        assert proc.returncode == 130, (stdout, stderr)
        assert "Fatal Python error" not in stderr, stderr
        assert elapsed < 10.0, f"took {elapsed:.1f}s to exit after SIGINT"
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.communicate(timeout=10)
        _pkill(f"sleep {sleep_arg}")


def test_background_child_writing_after_the_parent_exits_is_fully_collected(
    tmp_path: Path,
) -> None:
    """No timeout, no cancellation: a same-group background child that
    outlives its own parent and keeps writing to the inherited stdout
    pipe must have every byte of that collected, exactly as a plain
    ``communicate()`` with no live wiring would -- this must not be cut
    off at some shorter bound."""
    sleep_arg = _unique_sleep_arg(1)
    orch = tmp_path / "orch.yml"
    orch.write_text(
        "effects:\n"
        "  - type: tool\n"
        "    name: bg\n"
        "    provider: shell\n"
        "    params:\n"
        "      command: sh\n"
        "      allowed_commands: [sh]\n"
        "      args: [\"-c\", \"echo started; "
        f"(sleep {sleep_arg}; echo late) &\"]\n",
        encoding="utf-8",
    )
    result, out_path, _live_path = _run_cof(orch, tmp_path)

    assert result.returncode == 0, (result.stdout, result.stderr)
    assert _step_value(out_path, "bg") == "started\nlate\n"
