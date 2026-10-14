"""Unit tests for `process_ownership_support.owned` (#561)."""

from __future__ import annotations

import os
import subprocess
import sys
import uuid

import pytest
from process_ownership_support import owned

pytestmark = pytest.mark.skipif(os.name != "posix", reason="ps -o pgid is POSIX only")


def test_owned_is_false_for_a_dead_group() -> None:
    proc = subprocess.Popen(["true"], start_new_session=True)
    proc.wait(timeout=10)

    assert owned(proc.pid, ("anything",), by="pgid") is False


def test_owned_is_false_for_a_live_group_without_the_token() -> None:
    proc = subprocess.Popen(["sleep", "5"], start_new_session=True)
    try:
        assert owned(proc.pid, (f"no-such-token-{uuid.uuid4().hex}",), by="pgid") is False
    finally:
        proc.kill()
        proc.wait(timeout=10)


def test_owned_is_true_for_a_live_group_with_the_token() -> None:
    token = uuid.uuid4().hex
    proc = subprocess.Popen(
        [sys.executable, "-c", "import time; time.sleep(5)", token],
        start_new_session=True,
    )
    try:
        assert owned(proc.pid, (token,), by="pgid") is True
    finally:
        proc.kill()
        proc.wait(timeout=10)


def test_owned_checks_the_pid_column_when_by_is_pid() -> None:
    """A single pid, not its whole group -- the mode a teardown that only
    holds a pid (never a pgid) must use (#561)."""
    token = uuid.uuid4().hex
    proc = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(5)", token])
    try:
        assert owned(proc.pid, (token,), by="pid") is True
        assert owned(proc.pid, (f"not-{token}",), by="pid") is False
    finally:
        proc.kill()
        proc.wait(timeout=10)
