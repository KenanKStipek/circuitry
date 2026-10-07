"""SIGINT/SIGTERM-as-cancellation for the CLI's own run commands (#338, #356).

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
  - A *second* SIGINT/SIGTERM, while the first one's cleanup
    (``finally:`` blocks, state persistence) is still running, ends the
    process at once via ``os._exit`` — the same exit code, no traceback —
    rather than escaping as an uncaught exception from inside that cleanup.

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
#: what a second signal during cleanup exits with, matching the code the
#: first signal's own (uninterrupted) cleanup path would have produced.
_EXIT_CODES: dict[int, int] = {signal.SIGINT: 130, signal.SIGTERM: 143}


class SigTermInterrupt(KeyboardInterrupt):
    """Raised in the main thread when SIGTERM arrives during a run."""


def _make_handler(exc: type[BaseException]):
    def _handler(signum: int, frame: FrameType | None) -> None:
        del frame
        first = get_token().request(signum=signum)
        if not first:
            # A second SIGINT/SIGTERM while cleanup from the first is still
            # running ends the run at once, same exit code, no traceback
            # (#356) — the first signal's own cleanup gets no further say.
            os._exit(_EXIT_CODES.get(signum, 128 + signum))
        raise exc

    return _handler


@contextmanager
def sigterm_as_interrupt() -> Iterator[None]:
    """Make SIGINT/SIGTERM cancel the ``with`` body's run promptly (#356).

    A no-op off the main thread, so it is always safe to wrap a CLI command
    body in this regardless of how that command happens to be invoked.
    """
    if threading.current_thread() is not threading.main_thread():
        yield
        return

    token = get_token()
    token.reset()
    previous_sigint = signal.signal(signal.SIGINT, _make_handler(KeyboardInterrupt))
    previous_sigterm = signal.signal(signal.SIGTERM, _make_handler(SigTermInterrupt))
    try:
        yield
    finally:
        signal.signal(signal.SIGINT, previous_sigint)
        signal.signal(signal.SIGTERM, previous_sigterm)
        token.reset()
