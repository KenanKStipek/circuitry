"""Unit tests for `core.cancellation` (#356 review F1/F6): the `cleanup()`
suppression scope, its propagation into a worker thread via
`submit_with_context`, and the `armed` gate on starting a tracked child in
its own process group.

These are fast, in-process checks of the mechanism `tests/cli/
test_run_cancel_parallel.py`'s real-subprocess tests exercise end to end —
covering the retry-backoff/slot-wait-style "worker thread polls the token"
shape without a real sleep or a real signal.
"""

from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor

import pytest

from circuitry.core.cancellation import (
    CancellationToken,
    RunCancelledBySignal,
    submit_with_context,
)


@pytest.fixture
def token() -> CancellationToken:
    """A fresh, already-cancelled token — never the module singleton
    (`get_token()`), so these tests can't leak state into another test
    that happens to run in the same process."""
    t = CancellationToken()
    t.request()
    return t


def test_check_raises_once_cancelled(token: CancellationToken) -> None:
    with pytest.raises(RunCancelledBySignal):
        token.check()


def test_check_does_not_raise_inside_cleanup(token: CancellationToken) -> None:
    with token.cleanup():
        token.check()  # must not raise


def test_check_raises_again_after_cleanup_exits(token: CancellationToken) -> None:
    with token.cleanup():
        token.check()
    with pytest.raises(RunCancelledBySignal):
        token.check()


def test_sleep_or_raise_raises_once_cancelled(token: CancellationToken) -> None:
    with pytest.raises(RunCancelledBySignal):
        token.sleep_or_raise(0)


def test_sleep_or_raise_does_not_raise_inside_cleanup(token: CancellationToken) -> None:
    with token.cleanup():
        token.sleep_or_raise(0)  # must not raise, just a plain (zero) sleep


def test_cleanup_nests(token: CancellationToken) -> None:
    """A `finally:` inside a `finally:` (dynamic-in-dynamic) must not have
    the inner scope's exit re-enable cancellation for the outer one."""
    with token.cleanup():
        with token.cleanup():
            token.check()
        token.check()  # still suppressed — the outer scope is still open
    with pytest.raises(RunCancelledBySignal):
        token.check()


def test_cleanup_suppression_propagates_into_a_worker_thread_via_submit_with_context(
    token: CancellationToken,
) -> None:
    """A nested tree-flow dynamic inside `finally:` submits its branches
    with `submit_with_context` (#356) specifically so this holds \u2014 a bare
    `executor.submit` would not carry the suppression into the new thread
    at all, reintroducing F1 one level down."""

    def poll() -> str:
        token.check()  # would raise on a bare executor.submit
        return "ran"

    with token.cleanup():
        with ThreadPoolExecutor(max_workers=1) as executor:
            future = submit_with_context(executor, poll)
            assert future.result() == "ran"


def test_bare_executor_submit_does_not_carry_the_suppression(
    token: CancellationToken,
) -> None:
    """Documents why `submit_with_context` exists at all: contextvars are
    not inherited by a bare `ThreadPoolExecutor.submit` the way they are
    across an ordinary function call on the same thread."""

    def poll() -> None:
        token.check()

    with token.cleanup():
        with ThreadPoolExecutor(max_workers=1) as executor:
            future = executor.submit(poll)
            with pytest.raises(RunCancelledBySignal):
                future.result()


def test_armed_defaults_false_and_toggles() -> None:
    t = CancellationToken()
    assert t.armed is False
    t.arm()
    assert t.armed is True
    t.disarm()
    assert t.armed is False
