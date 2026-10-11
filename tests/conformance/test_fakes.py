"""Direct subprocess tests for `fake`, the reusable PATH-fake binary (issue
#450 item 4): every mode it supports, independent of any conformance case
that happens to use one. `c-shell-path-fake` is the only case that uses it
today (fixed stdout/exit code, via `shell`); `--sleep`/`--crlf`/
`--invalid-utf8` aren't exercised by a committed case yet, so this is their
only coverage until a future timeout/cancellation/encoding case lands.
"""

from __future__ import annotations

import os
import signal
import subprocess
import sys
import time
from pathlib import Path

FAKE = (
    Path(__file__).resolve().parent
    / "cases"
    / "c-shell-path-fake"
    / "fakes"
    / "fake"
)


def test_fixed_stdout_and_exit_code() -> None:
    proc = subprocess.run(
        [sys.executable, str(FAKE), "--stdout", "hi", "--exit", "3"],
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.stdout == "hi\n"
    assert proc.returncode == 3


def test_fixed_stderr() -> None:
    proc = subprocess.run(
        [sys.executable, str(FAKE), "--stderr", "oops"],
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.stderr == "oops\n"
    assert proc.returncode == 0


def test_crlf_output() -> None:
    proc = subprocess.run(
        [sys.executable, str(FAKE), "--crlf"],
        capture_output=True,
        check=False,
    )
    assert proc.stdout == b"line one\r\nline two\r\n"


def test_invalid_utf8_output() -> None:
    proc = subprocess.run(
        [sys.executable, str(FAKE), "--invalid-utf8"],
        capture_output=True,
        check=False,
    )
    with_pytest_raises = False
    try:
        proc.stdout.decode("utf-8")
    except UnicodeDecodeError:
        with_pytest_raises = True
    assert with_pytest_raises, "the fixture's own stdout should not be valid UTF-8"


def test_sleep_ignores_sigterm_until_killed() -> None:
    proc = subprocess.Popen(
        [sys.executable, str(FAKE), "--sleep"],
        start_new_session=True,
    )
    try:
        time.sleep(0.2)
        proc.terminate()  # SIGTERM -- ignored by --sleep
        with_timeout = False
        try:
            proc.wait(timeout=0.5)
        except subprocess.TimeoutExpired:
            with_timeout = True
        assert with_timeout, "--sleep must ignore SIGTERM"
    finally:
        os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        proc.wait(timeout=5)
