"""SIGINT/SIGTERM/SIGHUP-as-cancellation for the CLI's own run commands (#338, #356).

Ctrl-C/SIGINT already raises ``KeyboardInterrupt`` in the main thread via
CPython's default handler, and ``runtime_shim.run`` treats that exactly
like any other failure (#270 F6). SIGTERM (``kill <pid>``, a process
manager, a system shutdown) has no such default — the interpreter just
dies — so a run killed that way wrote neither ``--out`` nor its persistence
snapshot and could never be resumed.

``sigterm_as_interrupt`` installs handlers for *both* signals, for the
duration of one run:

  - SIGTERM raises :class:`SigTermInterrupt` in the main thread — a
    ``KeyboardInterrupt`` subclass, so every place that already treats
    `KeyboardInterrupt` as "an interrupted, still-resumable run" (the
    ``except (Exception, KeyboardInterrupt)`` in ``runtime_shim.run``, the
    ``BaseException`` catch around ``finally:`` in ``core.dynamic``) keeps
    working unchanged, while callers that care which signal it was can
    still ``isinstance`` for this specific subclass to pick exit code 143
    over 130.
  - SIGINT raises plain ``KeyboardInterrupt``, the same as CPython's own
    default handler — installing a handler for it at all is only needed so
    the *first* SIGINT, like the first SIGTERM, also calls
    ``core.cancellation.CancellationToken.request`` (#356): that is what
    lets a tree-flow branch running on a worker thread — which a signal
    never reaches directly — see that the run was cancelled and stop
    promptly instead of running to completion while the main thread waits.
  - SIGHUP (a terminal hangup: the terminal window closing, an SSH
    disconnect) is handled exactly like SIGTERM — a hangup kills the
    terminal, not this process's session, and with every tracked child
    started in its own process group (``core.cancellation.run_tracked``)
    a bare, unhandled SIGHUP would otherwise end `cof` with no cleanup at
    all while those children kept running as orphans. Left alone if it is
    already ignored on entry (``signal.getsignal(signal.SIGHUP) is
    signal.SIG_IGN`` — e.g. launched under ``nohup``, which is exactly the
    mechanism by which a caller opts out of this), and skipped entirely on
    a platform with no SIGHUP (Windows).
  - A *second* SIGINT/SIGTERM, while the first one's cleanup
    (``finally:`` blocks, state persistence) is still running, ends the
    process at once via ``os._exit`` — the same exit code, no traceback —
    rather than escaping as an uncaught exception from inside that cleanup.
  - SIGHUP never counts as that second signal (#357 follow-up): a closed
    terminal can deliver SIGHUP to a foreground job twice, a millisecond
    or so apart (observed from an interactive zsh; a plain `kill -HUP`
    or macOS's own bash only ever send one), and the second one landing
    inside this same signal-handling window must not race the first
    one's own cleanup to ``os._exit``. Once cancellation has already been
    requested by *any* signal, every further SIGHUP here is a pure no-op.
    This scope also leaves SIGHUP set to ``SIG_IGN`` — instead of
    restoring whatever handler it found on entry — when it exits with
    cancellation already requested, so a SIGHUP arriving after `cof`
    stops watching for it (while `--out`/the `--last` stash are still
    being written — ``runtime.persistence`` is already written by this
    point, inside the run itself) cannot kill the process before that
    finishes; it is never set to ``SIG_IGN`` any earlier than that, since
    a step can still start a child in here and a child inherits
    ``SIG_IGN`` across ``exec`` the same way `nohup` relies on.
    A second SIGINT/SIGTERM still ends the process at once as above.

Only `cof run`/`run-library` install this, only while they run, and only on
the main thread: ``signal.signal`` itself raises off the main thread, and
even if it didn't, an embedder (the SDK, the MCP server, the REST host)
calling into this package from a worker thread must keep its own signal
handling — this module must never install anything on its behalf.
"""

from __future__ import annotations

import os
import signal
import threading
from collections.abc import Iterator
from contextlib import contextmanager
from types import FrameType

from ..core.cancellation import get_token

#: Conventional POSIX exit code for each signal (128 + signal number) —
#: what a second SIGINT/SIGTERM during cleanup exits with, matching the
#: code the first signal's own (uninterrupted) cleanup path would have
#: produced. No SIGHUP entry: SIGHUP never takes this path (#357
#: follow-up) — its own first-signal exit code (129) is computed in
#: `cli.app` instead, from `result.sighup`.
_EXIT_CODES: dict[int, int] = {signal.SIGINT: 130, signal.SIGTERM: 143}


class SigTermInterrupt(KeyboardInterrupt):
    """Raised in the main thread when SIGTERM arrives during a run."""


class SigHupInterrupt(KeyboardInterrupt):
    """Raised in the main thread when SIGHUP (a terminal hangup) arrives
    during a run — treated exactly like :class:`SigTermInterrupt`."""


def _make_handler(exc: type[BaseException]):
    def _handler(signum: int, frame: FrameType | None) -> None:
        del frame
        first = get_token().request(signum=signum)
        if not first:
            if signum == getattr(signal, "SIGHUP", None):
                # Every further SIGHUP is a no-op once cancellation has
                # already been requested by any signal (#357 follow-up) —
                # never the `os._exit` a second SIGINT/SIGTERM takes, so a
                # closed terminal's own double-hangup can't race the first
                # one's cleanup.
                return
            # A second SIGINT/SIGTERM while cleanup from the first is still
            # running ends the run at once, same exit code, no traceback
            # (#356) — the first signal's own cleanup gets no further say.
            os._exit(_EXIT_CODES.get(signum, 128 + signum))
        raise exc

    return _handler


@contextmanager
def sigterm_as_interrupt() -> Iterator[None]:
    """Make SIGINT/SIGTERM/SIGHUP cancel the ``with`` body's run promptly (#356).

    A no-op off the main thread, so it is always safe to wrap a CLI command
    body in this regardless of how that command happens to be invoked.
    """
    if threading.current_thread() is not threading.main_thread():
        yield
        return

    token = get_token()
    token.reset()
    token.arm()
    previous_sigint = signal.signal(signal.SIGINT, _make_handler(KeyboardInterrupt))
    previous_sigterm = signal.signal(signal.SIGTERM, _make_handler(SigTermInterrupt))
    # SIGHUP is installed only when the platform has one (not Windows) and
    # it isn't already ignored on entry -- a caller that ran `cof` under
    # `nohup` (or otherwise set SIG_IGN) means it, and a hangup can't reach
    # an ignored handler anyway, so leave it exactly as found and never
    # touch it again on the way out either.
    install_sighup = (
        hasattr(signal, "SIGHUP")
        and signal.getsignal(signal.SIGHUP) is not signal.SIG_IGN
    )
    previous_sighup = (
        signal.signal(signal.SIGHUP, _make_handler(SigHupInterrupt))
        if install_sighup
        else None
    )
    try:
        yield
    finally:
        # Read before `token.reset()` clears it below: whether *this* run
        # was cancelled by any signal, not just SIGHUP, decides whether
        # SIGHUP is left ignored past this point (#357 follow-up).
        cancelled = token.is_set()
        signal.signal(signal.SIGINT, previous_sigint)
        signal.signal(signal.SIGTERM, previous_sigterm)
        if install_sighup:
            # A cancelled run still has `--out` and the `--last` stash to
            # write after this scope exits (`cli.app`, outside this
            # `with`) — `runtime.persistence` is already written by this
            # point, inside `run()` while these handlers were still
            # installed. A SIGHUP landing in that window must not revert
            # to whatever handler was here before (SIG_DFL would kill the
            # process outright) and undo that write. Set SIG_IGN only now,
            # once no step can start a new child to inherit it — never
            # inside the handler itself, see the module docstring. An
            # uncancelled run restores exactly what it found, as always.
            signal.signal(
                signal.SIGHUP, signal.SIG_IGN if cancelled else previous_sighup
            )
        token.disarm()
        token.reset()
