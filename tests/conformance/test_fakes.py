"""Direct subprocess tests for `fake`, the reusable PATH-fake binary (issue
#450 item 4): every mode the base fake supports, independent of any
conformance case that happens to use one. `c-shell-path-fake` uses
`--stdout`/`--exit`; `c-shell-crlf` and `c-shell-invalid-utf8` use
`--crlf`/`--invalid-utf8`; `c-shell-timeout` uses `--sleep` -- this module
stays their direct, case-independent coverage either way. The pidfile
flags (`--spawn-background`/`--check-pidfile`/`--kill-pidfile`) live only
on the extended copy `c-shell-background-survives-normal-exit` keeps
(issue #519); this module doesn't test them.
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


def test_cat_copies_stdin_to_stdout_verbatim() -> None:
    proc = subprocess.run(
        [sys.executable, str(FAKE), "--cat"],
        input=b"hello from stdin\n",
        capture_output=True,
        check=False,
    )
    assert proc.stdout == b"hello from stdin\n"
    assert proc.returncode == 0


def test_print_env_reports_a_set_variable() -> None:
    env = {**os.environ, "FAKE_TEST_VAR": "the-value"}
    proc = subprocess.run(
        [sys.executable, str(FAKE), "--print-env", "FAKE_TEST_VAR"],
        capture_output=True,
        text=True,
        check=False,
        env=env,
    )
    assert proc.stdout == "the-value\n"


def test_print_env_reports_unset_for_a_missing_variable() -> None:
    env = {k: v for k, v in os.environ.items() if k != "FAKE_TEST_VAR_MISSING"}
    proc = subprocess.run(
        [sys.executable, str(FAKE), "--print-env", "FAKE_TEST_VAR_MISSING"],
        capture_output=True,
        text=True,
        check=False,
        env=env,
    )
    assert proc.stdout == "<unset>\n"


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
