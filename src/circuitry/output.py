from __future__ import annotations

import os
import threading
from collections.abc import Callable, Generator
from contextlib import contextmanager
from typing import Any, TypeVar

from rich.console import Console
from rich.theme import Theme

THEME = Theme(
    {
        "info": "dim cyan",
        "ok": "green",
        "warn": "yellow",
        "err": "bold red",
        "path": "magenta",
        "title": "bold white",
    }
)


class ResilientConsole(Console):
    """A ``Console`` whose ``print`` tolerates a gone terminal (#356 follow-up).

    A terminal hangup (SIGHUP) or a reader closing its end of a pipe makes
    the next write to stdout/stderr raise ``OSError`` (EIO) or
    ``BrokenPipeError`` (EPIPE, itself an ``OSError`` subclass) — and this
    console is also used for a container's own verbose progress reporting
    from inside its ``finally:`` (``core.dynamic`` et al.), where a print
    failure must never abort the rest of that cleanup. Best-effort: once
    nothing is reading, there is nothing useful left to do but drop the
    line and keep going.
    """

    def print(self, *args: Any, **kwargs: Any) -> None:
        try:
            super().print(*args, **kwargs)
        except OSError:
            # A plain OSError (EIO: a genuine terminal hangup, not a
            # closed pipe) never reaches `on_broken_pipe` below -- Rich's
            # own `_check_buffer` only intercepts `BrokenPipeError`
            # specifically. Treat it exactly the same way regardless, for
            # the same reason (see `_give_up_on_write`'s own docstring).
            self._give_up_on_write()

    def on_broken_pipe(self) -> None:
        """Rich's own default (``Console.on_broken_pipe``) catches a
        ``BrokenPipeError`` from this same write and turns it into
        ``raise SystemExit(1)`` -- bypassing the plain ``except OSError``
        above entirely, since ``SystemExit`` isn't one, and overriding
        whatever exit code the caller (`cli.app`'s own SIGTERM/SIGHUP
        mapping) was about to raise instead. See `_give_up_on_write`.
        """
        self._give_up_on_write()

    def _give_up_on_write(self) -> None:
        """Stop this console from writing at all, and make sure nothing
        later finds out the hard way that its fd is still broken.

        ``self.quiet = True`` stops *this* console from attempting any
        further write of its own. That alone still leaves a second trap:
        CPython's own interpreter-shutdown flush of ``sys.stdout``/
        ``sys.stderr`` runs after ``main()`` returns, completely outside
        any Python-level ``try/except`` this package could ever install
        -- if that flush also hits the same broken fd, Python discards
        whatever exit code was actually raised and replaces it with 120
        instead (see ``flush_std_files`` in CPython's own
        ``Modules/main.c``). ``dup2``-ing ``/dev/null`` onto this
        console's own fd (stdout or stderr, whichever ``self.file`` says
        this instance actually wraps -- not hardcoded to stdout the way
        Rich's own ``on_broken_pipe`` default is) makes that later
        shutdown flush a silent no-op instead of a second failure.
        """
        self.quiet = True
        try:
            devnull = os.open(os.devnull, os.O_WRONLY)
            try:
                os.dup2(devnull, self.file.fileno())
            finally:
                os.close(devnull)
        except OSError:
            pass


console = ResilientConsole(theme=THEME)

T = TypeVar("T")

#: Guards every interactive live region this process ever opens on
#: ``console`` — a loop's progress status line, a tree loop's iteration
#: tracker, a prompt/tool spinner. Rich renders one live region per console;
#: nesting them is unsafe to rely on (some rich versions raise
#: ``LiveError`` instead of stacking harmlessly), so rather than depend on
#: that, at most one region may be active across the whole process at a
#: time. Ownership is decided and released while holding the lock, but the
#: live region itself is entered and exited *outside* the lock — holding a
#: non-reentrant lock across a caller's ``with`` body would deadlock the
#: next live region requested on the same thread (#271/#331 finding 1).
#: Whichever region claims the console first wins; everything that would
#: nest under it is silently skipped (``None``, no crash, no garbled
#: output) rather than queued or raised.
_live_region_lock = threading.Lock()
_live_region_active = False


@contextmanager
def live_region(make_live: Callable[[], T]) -> Generator[T | None, None, None]:
    """Claim the single process-wide live-region slot, or yield ``None`` if
    another live region already holds it. ``make_live`` is called (and its
    result entered as a context manager) only once ownership is secured."""
    global _live_region_active
    with _live_region_lock:
        owner = not _live_region_active
        if owner:
            _live_region_active = True
    if not owner:
        yield None
        return
    try:
        live_cm: Any = make_live()
        with live_cm as entered:
            yield entered
    finally:
        with _live_region_lock:
            _live_region_active = False
