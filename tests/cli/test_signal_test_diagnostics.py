"""`_diagnose_and_fail`'s own SIGUSR1-then-SIGKILL timeout diagnostics
(#558): behavior of this test suite's own tooling, not `cof` itself, so
it lives here rather than in one of the modules (`test_run_sighup.py`,
`test_run_cancel_parallel.py`, `test_run_cancel_signal_delivery.py`,
`test_events_stream.py`) that call it against a real hang.
"""

from __future__ import annotations

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


def _hung_child(tmp_path: Path) -> subprocess.Popen[str]:
    return subprocess.Popen(
        [sys.executable, "-c", "import time; time.sleep(60)"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=_sandboxed_env(tmp_path),
    )


def test_diagnose_and_fail_dumps_every_thread_stack_and_sigkills(
    tmp_path: Path,
) -> None:
    proc = _hung_child(tmp_path)
    # The real callers only ever reach `_diagnose_and_fail` after a much
    # longer wait (`_wait_for_paths`'s own 20s) for a `cof run` child to
    # get well past interpreter startup; this gives this deliberately
    # hung child's `sitecustomize.py` time to register the handler
    # before the signal below, too.
    time.sleep(0.5)
    try:
        with pytest.raises(pytest.fail.Exception) as excinfo:
            _diagnose_and_fail(proc, timeout=0.0, label="deliberately hung child")
        message = str(excinfo.value)
        assert "Current thread" in message
        assert "<module>" in message
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=5.0)
    assert proc.returncode == -signal.SIGKILL


@pytest.mark.skipif(sys.platform != "darwin", reason="macOS-only crash-report check")
def test_diagnose_and_fail_writes_no_macos_crash_report(tmp_path: Path) -> None:
    reports_dir = Path.home() / "Library" / "Logs" / "DiagnosticReports"
    before = set(reports_dir.glob("Python-*.ips")) if reports_dir.exists() else set()
    proc = _hung_child(tmp_path)
    time.sleep(0.5)
    try:
        with pytest.raises(pytest.fail.Exception):
            _diagnose_and_fail(proc, timeout=0.0, label="deliberately hung child")
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=5.0)
    # ReportCrash writes a report well after the process that crashed has
    # already exited -- observed up to ~20s on this suite's own machine.
    time.sleep(20.0)
    after = set(reports_dir.glob("Python-*.ips")) if reports_dir.exists() else set()
    assert after - before == set()
