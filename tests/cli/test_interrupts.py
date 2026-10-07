"""Unit tests for `cli.interrupts.sigterm_as_interrupt`'s own SIGHUP
bookkeeping (#357 follow-up): a repeated SIGHUP is a no-op while the
handlers are installed, and a cancelled run leaves SIGHUP ignored past
this scope's own exit rather than reverting to whatever handler it found
on entry.

Fast, in-process checks of the signal-handler mechanism itself — the real
end-to-end behavior (an actual hangup killing a running branch,
`finally:` still running, `--out`/the `--last` stash written) is covered
against a real subprocess by `tests/cli/test_run_sighup.py` and
`test_run_sighup_pty.py`.
"""

from __future__ import annotations

import os
import signal
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
