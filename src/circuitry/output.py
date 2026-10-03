from __future__ import annotations

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

console = Console(theme=THEME)

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
