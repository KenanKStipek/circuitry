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
import re
import signal
import subprocess
import sys
import time
from pathlib import Path

CASES = Path(__file__).resolve().parent / "cases"
FAKE = (
    CASES
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


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # Another account's process now holds the pid: ours is gone.
        return False
    return True


def test_sleep_exits_by_itself_once_orphaned() -> None:
    """An engine normally stops a `--sleep` fake with SIGKILL. If the engine
    dies first, nothing ever would, so the fake stops by itself once its
    parent is gone (#661)."""
    parent_code = (
        "import subprocess, sys, time\n"
        f"child = subprocess.Popen([sys.executable, {str(FAKE)!r}, '--sleep'], start_new_session=True)\n"
        "print(child.pid, flush=True)\n"
        "time.sleep(60)\n"
    )
    parent = subprocess.Popen(
        [sys.executable, "-c", parent_code], stdout=subprocess.PIPE, text=True
    )
    assert parent.stdout is not None
    fake_pid = int(parent.stdout.readline())
    exited = False
    try:
        parent.kill()  # SIGKILL: the parent never gets to stop its child
        parent.wait(timeout=5)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if not _alive(fake_pid):
                exited = True
                break
            time.sleep(0.1)
        assert exited, "the --sleep fake outlived its parent by more than 5 s"
    finally:
        if not exited and _alive(fake_pid):
            os.kill(fake_pid, signal.SIGKILL)


def test_no_fake_sleeps_without_a_bound() -> None:
    """A fake that sleeps in an unbounded loop outlives an engine that dies
    mid-case (#661): every such loop must also watch for its parent going
    away and stop at a deadline."""
    unbounded = re.compile(r"while True:\s*\n\s*time\.sleep\(")
    offenders = [
        str(path.relative_to(CASES))
        for path in sorted(CASES.glob("*/fakes/*"))
        if path.is_file() and unbounded.search(path.read_text(errors="replace"))
    ]
    assert offenders == []


def test_every_copy_of_the_generic_fake_is_identical() -> None:
    """Each case keeps its own copy of the generic fake, so a change to one
    (such as the bounded `--sleep`) must reach them all. The pidfile-extended
    copy in `c-shell-background-survives-normal-exit` is the one exception."""
    header = "A fake process-backed binary for the conformance suite"
    reference = FAKE.read_bytes()
    differing = [
        str(path.relative_to(CASES))
        for path in sorted(CASES.glob("*/fakes/*"))
        if path.is_file()
        and path.parent.parent.name != "c-shell-background-survives-normal-exit"
        and header in path.read_text(errors="replace")
        and path.read_bytes() != reference
    ]
    assert differing == []
