"""Unit tests for `cli.interrupts.sigterm_as_interrupt`'s own SIGHUP
bookkeeping (#357 follow-up): a repeated SIGHUP is a no-op while the
handlers are installed, and a cancelled run leaves SIGHUP ignored past
this scope's own exit rather than reverting to whatever handler it found
on entry. Also covers `_SignalWatcher` itself (#404): it sets the
cancellation flag off the raw signal, independent of the main thread's
own Python-level handler, and survives a plain `asyncio.run()`.

Fast, in-process checks of the signal-handler mechanism itself — the real
end-to-end behavior (an actual hangup killing a running branch,
`finally:` still running, `--out`/the `--last` stash written) is covered
against a real subprocess by `tests/cli/test_run_sighup.py` and
`test_run_sighup_pty.py`.
"""

from __future__ import annotations

import asyncio
import os
import signal
import threading
import time
from types import FrameType

import pytest

from circuitry.cli.interrupts import (
    SigHupInterrupt,
    SigTermInterrupt,
    sigterm_as_interrupt,
)
from circuitry.core.cancellation import deferred_signal_region, get_token

requires_sighup = pytest.mark.skipif(
    not hasattr(signal, "SIGHUP"), reason="SIGHUP does not exist on this platform"
)


@pytest.fixture(autouse=True)
def _restore_process_signal_state():
    """`sigterm_as_interrupt` mutates process-wide signal handlers and the
    module-level `CancellationToken` singleton — undo both after every
    test here, regardless of what the test itself left behind, so this
    file can never leak state into a test that runs after it."""
    sigs = [signal.SIGINT, signal.SIGTERM]
    if hasattr(signal, "SIGHUP"):
        sigs.append(signal.SIGHUP)
    previous = {sig: signal.getsignal(sig) for sig in sigs}
    yield
    for sig, handler in previous.items():
        signal.signal(sig, handler)
    get_token().reset()


@requires_sighup
def test_sighup_left_sig_ign_after_a_cancelled_run_exits() -> None:
    """Whatever cancelled this run — here, a simulated SIGTERM, the same
    way a real one would by calling `request()` from the main thread
    before raising — SIGHUP must be `SIG_IGN` once this scope exits, not
    reverted to its own previous handler: a SIGHUP arriving after this
    point (while `cli.app` is still writing `--out`/the `--last` stash)
    must not be able to kill the process.
    """
    with sigterm_as_interrupt():
        get_token().request(signum=signal.SIGTERM)
    assert signal.getsignal(signal.SIGHUP) is signal.SIG_IGN


@requires_sighup
def test_sighup_restored_to_previous_handler_when_run_not_cancelled() -> None:
    """An uncancelled run restores exactly what it found on entry, as
    before this change — the `SIG_IGN` left behind by a cancelled run
    (above) is specific to a run that was actually interrupted."""

    def _sentinel_handler(signum: int, frame: FrameType | None) -> None:
        del signum, frame

    signal.signal(signal.SIGHUP, _sentinel_handler)
    with sigterm_as_interrupt():
        pass
    assert signal.getsignal(signal.SIGHUP) is _sentinel_handler


@requires_sighup
def test_repeated_sighup_while_installed_is_a_no_op_not_a_second_signal() -> None:
    """Calling the installed SIGHUP handler itself a second time — the
    in-process equivalent of a closed terminal's own double hangup — must
    neither exit the process nor raise a second time, unlike a second
    SIGINT/SIGTERM's own handler (#357 follow-up)."""
    with sigterm_as_interrupt():
        handler = signal.getsignal(signal.SIGHUP)
        with pytest.raises(SigHupInterrupt):
            handler(signal.SIGHUP, None)
        # The second call must return quietly: no exception, no os._exit
        # (which would have ended this very test process).
        assert handler(signal.SIGHUP, None) is None
        assert get_token().signum == signal.SIGHUP


@requires_sighup
def test_sighup_after_a_first_sigterm_is_also_a_no_op() -> None:
    """A SIGHUP arriving after a *different* signal already cancelled the
    run is just as much a no-op as a repeated SIGHUP would be (#357
    follow-up) — SIGHUP is never the signal a second, different one needs
    to have been for the no-op rule to apply."""
    with sigterm_as_interrupt():
        sigterm_handler = signal.getsignal(signal.SIGTERM)
        with pytest.raises(SigTermInterrupt):
            sigterm_handler(signal.SIGTERM, None)
        sighup_handler = signal.getsignal(signal.SIGHUP)
        assert sighup_handler(signal.SIGHUP, None) is None


class _FakeExit(BaseException):
    """Stands in for `os._exit`, which never returns -- the mock must
    raise, not just record, or the handler falls through to its own
    unconditional `raise exc` below the `os._exit` call, which only ever
    happens in a real process because that call never comes back."""


def test_second_sigint_exits_on_a_platform_without_sighup(monkeypatch) -> None:
    """On a platform with no `signal.SIGHUP` at all (Windows), a second
    SIGINT/SIGTERM during cleanup must still reach `os._exit` -- the
    handler's own SIGHUP check used to compare against `signal.SIGHUP`
    unconditionally, raising `AttributeError` here instead (#357 review
    finding 1)."""
    exit_codes: list[int] = []

    def _fake_exit(code: int) -> None:
        exit_codes.append(code)
        raise _FakeExit

    monkeypatch.setattr(os, "_exit", _fake_exit)

    had_sighup = hasattr(signal, "SIGHUP")
    previous_sighup = signal.SIGHUP if had_sighup else None
    if had_sighup:
        del signal.SIGHUP
    try:
        with sigterm_as_interrupt():
            handler = signal.getsignal(signal.SIGINT)
            with pytest.raises(KeyboardInterrupt):
                handler(signal.SIGINT, None)
            with pytest.raises(_FakeExit):
                handler(signal.SIGINT, None)
    finally:
        if had_sighup:
            signal.SIGHUP = previous_sighup

    assert exit_codes == [130]


def test_watcher_sets_the_cancellation_flag_from_a_signal_delivered_to_a_worker_thread(
) -> None:
    """#404: the core of the fix. `_SignalWatcher` must set the token's
    flag off the raw signal itself, not wait for the main thread's own
    Python-level handler -- which only ever runs on the main thread's
    own bytecode eval loop, and can be arbitrarily delayed if that
    thread is blocked elsewhere (#399).

    Proven here with the main thread genuinely parked: it blocks in
    `Thread.join()` -- a C-level wait that a signal delivered to a
    *different* thread's tid does not interrupt -- while a worker
    thread self-delivers SIGINT via `signal.pthread_kill` and then keeps
    running a while longer, so the main thread's own join() cannot
    return (and so cannot reach bytecode to run its handler) until well
    after a third, independent poller thread already observed the
    token cancelled.
    """
    seen_promptly = threading.Event()
    observed_signum: list[int | None] = [None]

    def _poll_for_cancellation(token) -> None:
        # Generous even under heavy CPU contention (this suite's own
        # bounded-load gate uses three CPU-bound processes): the watcher
        # thread's `os.read()` wakes the instant the signal is delivered,
        # independent of the GIL, but it still has to acquire the GIL to
        # run `CancellationToken.request()`, which this process's other
        # threads are also contending for.
        deadline = time.monotonic() + 6.0
        while time.monotonic() < deadline:
            if token.is_set():
                observed_signum[0] = token.signum
                seen_promptly.set()
                return
            time.sleep(0.005)

    def _signal_self_then_keep_the_main_thread_blocked(token) -> None:
        # Gives the main thread time to actually reach and enter its own
        # blocking `join()` call below before the signal is sent --
        # without this, starting this thread and reaching the
        # `pthread_kill` below can race the main thread merely getting as
        # far as calling `join()` at all, and if this thread wins that
        # race, the main thread's own pending-call check fires on some
        # earlier bytecode instead, well before it is actually blocked --
        # defeating the scenario this test exists to drive.
        time.sleep(0.2)
        poller = threading.Thread(target=_poll_for_cancellation, args=(token,))
        poller.start()
        signal.pthread_kill(threading.get_ident(), signal.SIGINT)
        # Outlasts the poller's own window -- the main thread's `join()`
        # below cannot return, and so cannot run its own handler, until
        # this thread does.
        time.sleep(7.0)
        poller.join(timeout=10.0)

    try:
        with sigterm_as_interrupt():
            token = get_token()
            worker = threading.Thread(
                target=_signal_self_then_keep_the_main_thread_blocked,
                args=(token,),
            )
            worker.start()
            worker.join(timeout=20.0)
            # Drains a real SIGINT this main thread is still delayed in
            # noticing under heavy CPU contention: CPython only ever
            # services a signal's pending Python-level handler the next
            # time *this* thread's own bytecode eval loop checks for one,
            # and a thread merely being unblocked by `join()` doesn't by
            # itself guarantee the OS schedules it again instantly under
            # load. Actively running bytecode here (rather than leaving
            # the `with` scope, which would restore the previous SIGINT
            # handler and could then misattribute an unserviced pending
            # signal to some later, unrelated scope's own handler) gives
            # the eval breaker many chances to catch it while this test's
            # own `except KeyboardInterrupt:` below is still the thing
            # watching for it.
            pump_deadline = time.monotonic() + 5.0
            while time.monotonic() < pump_deadline:
                time.sleep(0.01)
    except KeyboardInterrupt:
        # Expected: the main thread's own Python-level handler also
        # raises eventually -- this test is only interested in
        # `seen_promptly`, set well before that by the watcher thread,
        # not in this handler's own behaviour (covered by the rest of
        # this module).
        pass

    assert seen_promptly.is_set(), (
        "the watcher never set the cancellation flag while the main "
        "thread was still blocked in worker.join()"
    )
    assert observed_signum[0] == signal.SIGINT


def test_a_signal_the_watcher_already_flagged_is_still_the_handler_s_first_signal(
) -> None:
    """#404 regression: once `_SignalWatcher` exists, its own call to
    `CancellationToken.request()` ordinarily beats the main thread's
    Python-level handler to it, so `request()`'s return value can no
    longer tell that handler whether *it* is seeing the first signal --
    it would already read `False` even for a genuinely first SIGINT,
    which (unfixed) would make the handler take the second-signal
    `os._exit` path immediately, with no cleanup, on every run's very
    first Ctrl-C. `_SignalCount` exists to keep this decision independent
    of the token's own flag -- simulated directly here by setting the
    flag (as the watcher would) before the handler itself ever runs.
    """
    with sigterm_as_interrupt():
        token = get_token()
        token.request(signum=signal.SIGINT)  # simulates the watcher winning the race

        handler = signal.getsignal(signal.SIGINT)
        with pytest.raises(KeyboardInterrupt):
            handler(signal.SIGINT, None)


def test_first_signal_inside_a_deferred_region_is_parked_not_raised(
) -> None:
    """#555: while the main thread is inside a `deferred_signal_region`
    (`core.dynamic`/`core.loop`'s own `Future`/`executor` call sites),
    the handler must not raise a *first* signal's own exception straight
    into whatever lock-touching bytecode is running -- it parks it
    (`defer_first_signal_exception`) and returns instead, leaving the
    region to raise it once every lock it could have been holding is
    released.
    """
    with sigterm_as_interrupt():
        handler = signal.getsignal(signal.SIGINT)
        with pytest.raises(KeyboardInterrupt):
            with deferred_signal_region():
                assert handler(signal.SIGINT, None) is None, (
                    "a first signal inside a deferred region must return "
                    "quietly, not raise"
                )


def test_second_signal_inside_a_deferred_region_still_exits_at_once(
    monkeypatch,
) -> None:
    """#555: deferring a *first* signal's exception must never delay a
    *second* one's `os._exit` -- that decision (`_SignalCount`) is
    independent of whether a `deferred_signal_region` happens to be
    active, exactly as it is independent of everything else about where
    the main thread happens to be.
    """
    exit_codes: list[int] = []

    def _fake_exit(code: int) -> None:
        exit_codes.append(code)
        raise _FakeExit

    monkeypatch.setattr(os, "_exit", _fake_exit)

    with sigterm_as_interrupt():
        handler = signal.getsignal(signal.SIGINT)
        with pytest.raises(KeyboardInterrupt):
            with deferred_signal_region():
                assert handler(signal.SIGINT, None) is None
                with pytest.raises(_FakeExit):
                    handler(signal.SIGINT, None)
            # The region's own exit raises the first signal's parked
            # exception -- caught by the outer `pytest.raises` above.

    assert exit_codes == [130]


def test_second_signal_reaches_os_exit_without_calling_request_again(
    monkeypatch,
) -> None:
    """A second SIGINT/SIGTERM must reach `os._exit` without calling
    `CancellationToken.request()` a second time -- that call touches
    the token's own `Event`, a `Condition`-backed lock a first signal's
    own handler raising mid-release of could leave stuck (the
    #555-style exposure this closes). A repeated SIGHUP is the no-op
    case: it must return quietly too, without calling `request()` or
    `os._exit` either.
    """
    exit_codes: list[int] = []

    def _fake_exit(code: int) -> None:
        exit_codes.append(code)
        raise _FakeExit

    monkeypatch.setattr(os, "_exit", _fake_exit)

    with sigterm_as_interrupt():
        token = get_token()
        request_calls: list[int | None] = []
        real_request = token.request

        def _spy_request(signum: int | None = None) -> bool:
            request_calls.append(signum)
            return real_request(signum=signum)

        monkeypatch.setattr(token, "request", _spy_request)

        handler = signal.getsignal(signal.SIGINT)
        with pytest.raises(KeyboardInterrupt):
            handler(signal.SIGINT, None)
        assert request_calls == [signal.SIGINT]

        with pytest.raises(_FakeExit):
            handler(signal.SIGINT, None)
        assert request_calls == [signal.SIGINT], (
            "a second SIGINT must not call request() again"
        )
        assert exit_codes == [130]

        if hasattr(signal, "SIGHUP"):
            sighup_handler = signal.getsignal(signal.SIGHUP)
            assert sighup_handler(signal.SIGHUP, None) is None
            assert request_calls == [signal.SIGINT], (
                "a repeated SIGHUP must not call request() either"
            )
            assert exit_codes == [130]


def test_wakeup_fd_unaffected_by_asyncio_run_with_no_signal_handlers_registered() -> (
    None
):
    """#404: before relying on `signal.set_wakeup_fd` for `_SignalWatcher`,
    confirm with a real test -- not just asyncio's own docs -- that a
    plain `asyncio.run()` never replaces it. `plugins.mcp_client`'s own
    `_run_coroutine` calls exactly this (`asyncio.run(coro)` once per MCP
    call, with no signal handler of its own registered), so the watcher
    must keep working across one. ``asyncio.run()`` only ever touches the
    wakeup fd via ``loop.add_signal_handler`` -- asyncio's own opt-in
    mechanism, which nothing in this package or its `mcp`/`anyio`
    dependencies calls.
    """
    with sigterm_as_interrupt():
        registered_fd = signal.set_wakeup_fd(-1)
        signal.set_wakeup_fd(registered_fd)
        assert registered_fd != -1, "the watcher's own wakeup fd must be set"

        async def _run_a_subprocess() -> None:
            # Like `plugins.mcp_client`'s own stdio transport, which
            # launches the MCP server itself as a subprocess from inside
            # `asyncio.run()` -- a plain `asyncio.sleep` doesn't touch
            # the event loop's subprocess machinery at all, so it can't
            # tell this test apart from a coroutine that never exercises
            # that path.
            proc = await asyncio.create_subprocess_exec("true")
            await proc.wait()

        asyncio.run(_run_a_subprocess())

        after_fd = signal.set_wakeup_fd(-1)
        signal.set_wakeup_fd(after_fd)
        assert after_fd == registered_fd, (
            "asyncio.run() replaced the watcher's own wakeup fd -- "
            "_SignalWatcher would stop seeing real signals during an "
            "MCP call"
        )


requires_fork = pytest.mark.skipif(
    os.name != "posix" or not hasattr(os, "fork"),
    reason="fork is POSIX-only",
)


@requires_fork
def test_forked_child_sigterm_does_not_cancel_the_parent() -> None:
    """#404 review (a): a forked child -- `plugins.python_eval`'s own
    sandboxed child is the real-world case -- inherits CPython's
    process-global wakeup-fd arming across the fork (fork, unlike exec,
    never closes or resets anything). A SIGTERM delivered to such a
    child before it resets its own handlers writes into the *parent*'s
    wakeup pipe just as surely as one delivered to the parent itself --
    the write happens inside the signal's own C-level trampoline, which
    does this for every signal with a registered handler, regardless of
    which process receives it, as long as that process's own copy of
    `signal.set_wakeup_fd` is still armed. Without
    `_reset_signal_state_in_fork_child` disarming it in the child, the
    real parent process's own `_SignalWatcher` thread -- still blocked
    reading the *same* pipe, shared with the child via the fd table
    `fork()` duplicated -- would see that write and cancel the parent's
    run over a signal the child sent only to itself.
    """
    with sigterm_as_interrupt():
        token = get_token()
        pid = os.fork()
        if pid == 0:
            try:
                os.kill(os.getpid(), signal.SIGTERM)
                time.sleep(0.2)
            except BaseException:
                pass
            finally:
                os._exit(0)
        os.waitpid(pid, 0)
        # No in-process synchronization point to wait on here: the real
        # bug this guards is the parent's own watcher thread reacting to
        # a write on a pipe shared with the child via fork, not anything
        # this test process's own memory would show directly.
        deadline = time.monotonic() + 1.0
        while time.monotonic() < deadline and not token.is_set():
            time.sleep(0.01)
        assert not token.is_set(), (
            "a SIGTERM the forked child sent itself, inherited wakeup fd "
            "and all, cancelled the parent's own run"
        )


@requires_fork
def test_watcher_close_returns_promptly_while_a_forked_child_sleeps() -> None:
    """#404 review (b): a forked child also inherits both of
    `_SignalWatcher`'s own pipe fds -- a pipe only reports EOF to a
    reader once *every* copy of its write end, across every process, is
    closed. Without `_reset_signal_state_in_fork_child` closing the
    child's own inherited copies right after the fork,
    `_SignalWatcher.close` closing just the parent's own copy would
    leave its watcher thread blocked in `os.read()` for as long as this
    sleeping child lives, and its own `join(timeout=5.0)` would wait out
    the full five seconds on every run cancelled while a step's forked
    child is still alive.
    """
    child_pid: int | None = None
    try:
        before = time.monotonic()
        with sigterm_as_interrupt():
            pid = os.fork()
            if pid == 0:
                try:
                    time.sleep(10.0)
                finally:
                    os._exit(0)
            child_pid = pid
        elapsed = time.monotonic() - before
    finally:
        if child_pid is not None:
            try:
                os.kill(child_pid, signal.SIGKILL)
            except OSError:
                pass
            os.waitpid(child_pid, 0)

    assert elapsed < 1.0, (
        f"sigterm_as_interrupt's own exit (which calls _SignalWatcher.close) "
        f"took {elapsed:.2f}s with a sleeping forked child still alive -- its "
        "own copies of the watcher's pipe fds must be closed in the child, "
        "not just the parent"
    )
