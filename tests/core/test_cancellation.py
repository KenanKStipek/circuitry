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

import os
import signal
from concurrent.futures import ThreadPoolExecutor
from typing import Any

import pytest

from circuitry.core import cancellation
from circuitry.core.cancellation import (
    CancellationToken,
    RunCancelledBySignal,
    kill_process_group,
    kill_tracked_process,
    submit_with_context,
)


class _FakeTrackedProc:
    """Stands in for a `subprocess.Popen` tracked by `CancellationToken` —
    just enough of the interface `kill_process_group`/`track` touch."""

    def __init__(self, pid: int = 4242) -> None:
        self.pid = pid
        self._exited = False
        self.killed = False

    def poll(self) -> int | None:
        return 0 if self._exited else None

    def kill(self) -> None:
        self.killed = True
        self._exited = True


class _FakeMpProcess:
    """Stands in for a `multiprocessing.Process` tracked by
    `CancellationToken.track_process` (#357 follow-up) — just enough of
    the interface `kill_tracked_process`/`track_process` touch."""

    def __init__(self) -> None:
        self._alive = True
        self.killed = False

    def is_alive(self) -> bool:
        return self._alive

    def kill(self) -> None:
        self.killed = True
        self._alive = False


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


def test_kill_process_group_kills_only_the_child_when_it_shares_our_own_group(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """#357 review N1: a child never started with `start_new_session`
    (an unarmed token — the SDK, the MCP server, the REST host, any
    embedder) shares this interpreter's own process group. `killpg` on it
    would SIGKILL this process and its whole group along with the child;
    `kill_process_group` must fall back to killing just the child."""
    proc = _FakeTrackedProc()
    monkeypatch.setattr(os, "getpgid", lambda pid: 777)
    monkeypatch.setattr(os, "getpgrp", lambda: 777)

    def _fail_killpg(pgid: int, sig: int) -> None:
        raise AssertionError("must not killpg our own process group")

    monkeypatch.setattr(os, "killpg", _fail_killpg)

    kill_process_group(proc)  # type: ignore[arg-type]
    assert proc.killed is True


def test_kill_process_group_kills_the_whole_group_when_isolated(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A child started with `start_new_session=True` (armed) is its own
    process-group leader — `kill_process_group` must still killpg it, the
    pre-#357-review behaviour, so a `shell` step's whole subtree dies."""
    proc = _FakeTrackedProc()
    monkeypatch.setattr(os, "getpgid", lambda pid: 999)
    monkeypatch.setattr(os, "getpgrp", lambda: 777)
    killed_groups: list[tuple[int, int]] = []
    monkeypatch.setattr(
        os, "killpg", lambda pgid, sig: killed_groups.append((pgid, sig))
    )

    kill_process_group(proc)  # type: ignore[arg-type]
    assert killed_groups == [(999, signal.SIGKILL)]
    assert proc.killed is False


def test_track_kills_a_process_added_after_cancellation_was_already_requested(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """#357 review N2: `request()` only kills the processes it can already
    see at the moment it's called. A process registered via `track()`
    right after that snapshot must not run unseen to completion —
    `track()` itself must kill it immediately if cancellation is already
    set."""
    token = CancellationToken()
    token.request()
    killed: list[Any] = []
    monkeypatch.setattr(cancellation, "kill_process_group", killed.append)

    proc = _FakeTrackedProc()
    with token.track(proc):
        pass

    assert killed == [proc]


def test_track_does_not_kill_inside_cleanup_even_if_already_cancelled(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A `finally:` block's own subprocess steps (`run_tracked` already
    lets these start via its own `cleanup()` check) must not be killed on
    registration just because the run they're cleaning up from was
    already cancelled."""
    token = CancellationToken()
    token.request()
    killed: list[Any] = []
    monkeypatch.setattr(cancellation, "kill_process_group", killed.append)

    proc = _FakeTrackedProc()
    with token.cleanup():
        with token.track(proc):
            pass

    assert killed == []


def test_kill_tracked_process_kills_by_pid_only() -> None:
    """#357 follow-up: `python_eval`'s sandboxed child shares cof's own
    process group (it is never started with `start_new_session`), so
    unlike `kill_process_group` there is no group-vs-pid branch at all —
    `kill_tracked_process` must always just call `proc.kill()`."""
    proc = _FakeMpProcess()
    kill_tracked_process(proc)  # type: ignore[arg-type]
    assert proc.killed is True


def test_kill_tracked_process_is_a_no_op_once_already_exited() -> None:
    proc = _FakeMpProcess()
    proc.kill()
    assert proc.killed is True
    proc.killed = False  # reset the flag to prove a second kill() isn't called
    kill_tracked_process(proc)  # type: ignore[arg-type]
    assert proc.killed is False


def test_track_process_kills_a_process_added_after_cancellation_was_already_requested() -> None:
    """Mirrors `test_track_kills_a_process_added_after_cancellation_was_
    already_requested` for `track_process`/multiprocessing children."""
    token = CancellationToken()
    token.request()
    proc = _FakeMpProcess()

    with token.track_process(proc):  # type: ignore[arg-type]
        pass

    assert proc.killed is True


def test_track_process_does_not_kill_inside_cleanup_even_if_already_cancelled() -> None:
    token = CancellationToken()
    token.request()
    proc = _FakeMpProcess()

    with token.cleanup():
        with token.track_process(proc):  # type: ignore[arg-type]
            pass

    assert proc.killed is False


def test_request_kills_every_tracked_mp_process() -> None:
    token = CancellationToken()
    proc = _FakeMpProcess()
    with token.track_process(proc):  # type: ignore[arg-type]
        token.request()
        assert proc.killed is True
