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
from circuitry.core.cancellation import get_token

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
    with sigterm_as_interrupt():
        token = get_token()
        seen_promptly = threading.Event()

        def _poll_for_cancellation() -> None:
            deadline = time.monotonic() + 1.0
            while time.monotonic() < deadline:
                if token.is_set():
                    seen_promptly.set()
                    return
                time.sleep(0.005)

        def _signal_self_then_keep_the_main_thread_blocked() -> None:
            poller = threading.Thread(target=_poll_for_cancellation)
            poller.start()
            signal.pthread_kill(threading.get_ident(), signal.SIGINT)
            # Outlasts the poller's own 1.0s window -- the main thread's
            # `join()` below cannot return, and so cannot run its own
            # handler, until this thread does.
            time.sleep(1.5)
            poller.join(timeout=5.0)

        worker = threading.Thread(target=_signal_self_then_keep_the_main_thread_blocked)
        worker.start()
        try:
            worker.join(timeout=10.0)
        except KeyboardInterrupt:
            # Expected: the main thread's own Python-level handler also
            # raises here, the instant it next gets to check for a
            # pending call once `join()` itself returns -- this test is
            # only interested in `seen_promptly`, set well before that by
            # the watcher thread, not in this handler's own behaviour
            # (covered by the rest of this module).
            pass

        assert seen_promptly.is_set(), (
            "the watcher never set the cancellation flag while the main "
            "thread was still blocked in worker.join()"
        )
        assert token.signum == signal.SIGINT


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

        async def _trivial() -> None:
            await asyncio.sleep(0.01)

        asyncio.run(_trivial())

        after_fd = signal.set_wakeup_fd(-1)
        signal.set_wakeup_fd(after_fd)
        assert after_fd == registered_fd, (
            "asyncio.run() replaced the watcher's own wakeup fd -- "
            "_SignalWatcher would stop seeing real signals during an "
            "MCP call"
        )
