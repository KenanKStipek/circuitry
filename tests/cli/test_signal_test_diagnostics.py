"""`_diagnose_and_fail`'s own SIGUSR1-then-SIGKILL timeout diagnostics
(#558): behavior of this test suite's own tooling, not `cof` itself, so
it lives here rather than in one of the modules (`test_run_sighup.py`,
`test_run_cancel_parallel.py`, `test_run_cancel_signal_delivery.py`,
`test_events_stream.py`) that call it against a real hang.
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest
from _signal_test_support import _diagnose_and_fail, _sandboxed_env

pytestmark = pytest.mark.skipif(
    not hasattr(signal, "SIGUSR1"), reason="SIGUSR1 does not exist on this platform"
)

#: Named distinctly from anything the main thread's own frames could be
#: (`<module>`, the final `time.sleep`), so a dump that's missing this
#: thread's frames -- the "every thread" half of #558's claim, not just
#: "a" thread's -- is actually caught.
_WORKER_FRAME_NAME = "_signal_test_worker_frame"

_HUNG_CHILD_SCRIPT = f"""
import threading
import time

def {_WORKER_FRAME_NAME}():
    while True:
        time.sleep(0.1)

threading.Thread(target={_WORKER_FRAME_NAME}, daemon=True).start()
print("ready", flush=True)
time.sleep(60)
"""


def _hung_child(tmp_path: Path) -> subprocess.Popen[str]:
    proc = subprocess.Popen(
        [sys.executable, "-c", _HUNG_CHILD_SCRIPT],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=_sandboxed_env(tmp_path),
    )
    # `sitecustomize.py` registers the SIGUSR1 handler before any of this
    # script's own code runs, so this line reaching stdout means the
    # handler is already in place -- unlike a fixed sleep, this can't be
    # too short on a loaded/CI runner.
    assert proc.stdout is not None
    ready_line = proc.stdout.readline()
    assert ready_line == "ready\n", f"child did not start as expected: {ready_line!r}"
    return proc


def test_diagnose_and_fail_dumps_every_thread_stack_and_sigkills(
    tmp_path: Path,
) -> None:
    proc = _hung_child(tmp_path)
    try:
        with pytest.raises(pytest.fail.Exception) as excinfo:
            _diagnose_and_fail(proc, timeout=0.0, label="deliberately hung child")
        message = str(excinfo.value)
        assert "Current thread" in message
        assert "<module>" in message
        assert _WORKER_FRAME_NAME in message
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=5.0)
    assert proc.returncode == -signal.SIGKILL


def _ips_pid(path: Path) -> object:
    """The `pid` field of a macOS `.ips` crash report (its body is a JSON
    object on the second and later lines, after a one-line JSON header),
    or None if it can't be read -- ReportCrash may still be writing it."""
    try:
        _header, body = path.read_text(encoding="utf-8").split("\n", 1)
        return json.loads(body).get("pid")
    except (OSError, ValueError):
        return None


@pytest.mark.skipif(sys.platform != "darwin", reason="macOS-only crash-report check")
@pytest.mark.skipif(
    os.environ.get("CIRCUITRY_TEST_CRASH_REPORTS") != "1",
    reason="opt-in: set CIRCUITRY_TEST_CRASH_REPORTS=1 to scan "
    "~/Library/Logs/DiagnosticReports, shared with every other process on "
    "this machine, for up to 20s. "
    "test_diagnose_and_fail_dumps_every_thread_stack_and_sigkills's own "
    "returncode == -SIGKILL assertion already proves no crash report is "
    "possible for that run: ReportCrash only ever reacts to SIGABRT and "
    "its kin, never SIGKILL.",
)
def test_diagnose_and_fail_writes_no_macos_crash_report(tmp_path: Path) -> None:
    reports_dir = Path.home() / "Library" / "Logs" / "DiagnosticReports"
    before = set(reports_dir.glob("Python-*.ips")) if reports_dir.exists() else set()
    proc = _hung_child(tmp_path)
    pid = proc.pid
    try:
        with pytest.raises(pytest.fail.Exception):
            _diagnose_and_fail(proc, timeout=0.0, label="deliberately hung child")
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=5.0)
    # ReportCrash writes a report well after the process that crashed has
    # already exited -- observed up to ~20s on this suite's own machine.
    # Polled, not a flat sleep: most of the time there is nothing to find
    # at all (nothing crashed), so this returns in well under 20s.
    deadline = time.monotonic() + 20.0
    crashed_pids: set[object] = set()
    while time.monotonic() < deadline:
        after = set(reports_dir.glob("Python-*.ips")) if reports_dir.exists() else set()
        # Matched by pid, not just "no new report at all": another process
        # on this shared machine can send itself a real SIGABRT at any
        # moment, and that is not this test's concern.
        crashed_pids = {_ips_pid(report) for report in after - before}
        if pid in crashed_pids:
            break
        time.sleep(1.0)
    assert pid not in crashed_pids
