"""SIGTERM-as-cancellation for the CLI's own run commands (#338).

Ctrl-C/SIGINT already cancels a run: CPython's default SIGINT handler
raises ``KeyboardInterrupt`` in the main thread with no setup required, and
``runtime_shim.run`` treats that exactly like any other failure (#270 F6).
SIGTERM (``kill <pid>``, a process manager, a system shutdown) has no such
default — the interpreter just dies — so a run killed that way wrote
neither ``--out`` nor its persistence snapshot and could never be resumed.

``sigterm_as_interrupt`` closes that gap by installing a SIGTERM handler,
for the duration of one run, that raises :class:`SigTermInterrupt` in the
main thread — a ``KeyboardInterrupt`` subclass, so every place that already
treats `KeyboardInterrupt` as "an interrupted, still-resumable run" (the
``except (Exception, KeyboardInterrupt)`` in ``runtime_shim.run``, the
``BaseException`` catch around ``finally:`` in ``core.dynamic``) keeps
working unchanged, while callers that care which signal it was can still
``isinstance`` for this specific subclass to pick exit code 143 over 130.

Only `cof run`/`run-library` install this, only while they run, and only on
the main thread: ``signal.signal`` itself raises off the main thread, and
even if it didn't, an embedder (the SDK, the MCP server, the REST host)
calling into this package from a worker thread must keep its own SIGTERM
handling — this module must never install anything on its behalf.
"""

from __future__ import annotations

import signal
import threading
from collections.abc import Iterator
from contextlib import contextmanager
from types import FrameType


class SigTermInterrupt(KeyboardInterrupt):
    """Raised in the main thread when SIGTERM arrives during a run."""


def _raise_sigterm(signum: int, frame: FrameType | None) -> None:
    del signum, frame
    raise SigTermInterrupt


@contextmanager
def sigterm_as_interrupt() -> Iterator[None]:
    """Make SIGTERM behave like Ctrl-C for the duration of the ``with`` body.

    A no-op off the main thread, so it is always safe to wrap a CLI command
    body in this regardless of how that command happens to be invoked.
    """
    if threading.current_thread() is not threading.main_thread():
        yield
        return

    previous = signal.signal(signal.SIGTERM, _raise_sigterm)
    try:
        yield
    finally:
        signal.signal(signal.SIGTERM, previous)
