"""Textual terminal UI for Circuitry.

The UI itself lives behind the optional ``tui`` extra so the base install
stays dependency-light. Nothing in this module imports ``textual`` at
import time — :func:`textual_available` probes for it, and the app is
imported lazily inside :func:`run_tui`.
"""

from __future__ import annotations

import importlib.util
import sys
from typing import IO, Any

__all__ = [
    "INSTALL_HINT",
    "MISSING_EXTRA_MESSAGE",
    "run_tui",
    "should_launch_tui",
    "textual_available",
]

INSTALL_HINT = 'pip install "circuitry-cof[tui]"'

MISSING_EXTRA_MESSAGE = (
    "The Circuitry TUI requires the optional 'tui' extra (textual).\n"
    f"Install it with:\n\n    {INSTALL_HINT}\n"
)


def textual_available() -> bool:
    """Return True when the optional ``textual`` dependency is importable."""
    try:
        return importlib.util.find_spec("textual") is not None
    except (ImportError, ValueError):
        # A partially-installed or shadowed ``textual`` raises rather than
        # returning None; treat it the same as "not installed".
        return False


def _isatty(stream: IO[Any] | None) -> bool:
    """True when ``stream`` is a live terminal, tolerating odd streams."""
    isatty = getattr(stream, "isatty", None)
    if isatty is None:
        return False
    try:
        return bool(isatty())
    except (OSError, ValueError):
        # Closed or detached streams (pytest capture, daemonized runs).
        return False


def should_launch_tui() -> bool:
    """Decide whether a bare ``cof`` invocation should open the TUI.

    Both ends of the pipe must be a terminal — ``cof | cat`` and
    ``cof < file`` keep the plain CLI behaviour — and the optional extra
    must be installed.
    """
    return _isatty(sys.stdin) and _isatty(sys.stdout) and textual_available()


def run_tui() -> None:
    """Launch the Textual app. Requires the ``tui`` extra to be installed.

    Console logging is detached for the lifetime of the app so a stray
    ``logger.info`` cannot paint over the frame; file handlers keep receiving
    every record, and the original handlers are restored on exit.
    """
    from .app import CircuitryApp
    from .log import tui_logging

    with tui_logging():
        CircuitryApp().run()
