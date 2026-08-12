"""TUI-mode logging: keep log records off the terminal, keep files intact.

A single ``logger.info`` written straight to stdout while Textual owns the
screen shreds the frame. While the app runs we detach the *console* handlers
from the loggers we manage and leave everything else — file handlers,
syslog, custom sinks — exactly as configured, so a ``--log-file`` style setup
keeps receiving every record.
"""

from __future__ import annotations

import logging
import sys
from collections.abc import Iterator
from contextlib import contextmanager
from typing import IO, Any

__all__ = ["is_console_handler", "tui_logging"]

#: Loggers whose console handlers are detached when no explicit list is given.
_DEFAULT_LOGGER_NAMES = ("", "circuitry")


def _std_streams() -> tuple[object, ...]:
    """The streams that count as "the terminal" right now.

    Resolved on every call: under pytest ``sys.stdout`` is a capture object
    that replaces the real one, and a handler bound to it is still a console
    handler as far as the running app is concerned.
    """
    return tuple(
        stream
        for stream in (sys.stdout, sys.stderr, sys.__stdout__, sys.__stderr__)
        if stream is not None
    )


def _writes_to_terminal(stream: IO[Any] | None) -> bool:
    return stream is not None and any(stream is std for std in _std_streams())


def is_console_handler(handler: logging.Handler) -> bool:
    """True when ``handler`` would paint over the Textual frame.

    File handlers are explicitly exempt even though they subclass
    ``StreamHandler``; ``rich``-style handlers are caught through their
    ``console.file``.
    """
    if isinstance(handler, logging.FileHandler):
        return False
    if _writes_to_terminal(getattr(handler, "stream", None)):
        return True
    # rich.logging.RichHandler and friends: not a StreamHandler, but its
    # console still writes to stdout/stderr.
    console = getattr(handler, "console", None)
    return console is not None and _writes_to_terminal(getattr(console, "file", None))


def _resolve(loggers: tuple[logging.Logger, ...]) -> tuple[logging.Logger, ...]:
    if loggers:
        return loggers
    return tuple(logging.getLogger(name) for name in _DEFAULT_LOGGER_NAMES)


@contextmanager
def tui_logging(*loggers: logging.Logger) -> Iterator[list[logging.Handler]]:
    """Detach console handlers for the duration of the block.

    Yields the handlers that were detached (empty when logging was already
    quiet). Handlers are restored in their original order on the way out,
    including when the body raises.

    A :class:`logging.NullHandler` is parked on any logger left with no
    handlers at all: without it Python falls back to ``logging.lastResort``,
    which writes warnings to stderr — the exact corruption we are avoiding.
    """
    targets = _resolve(loggers)
    detached: list[tuple[logging.Logger, list[logging.Handler]]] = []
    placeholders: list[tuple[logging.Logger, logging.Handler]] = []

    for logger in targets:
        removed = [handler for handler in logger.handlers if is_console_handler(handler)]
        for handler in removed:
            logger.removeHandler(handler)
        if removed:
            detached.append((logger, removed))
        if not logger.handlers:
            placeholder = logging.NullHandler()
            logger.addHandler(placeholder)
            placeholders.append((logger, placeholder))

    try:
        yield [handler for _, handlers in detached for handler in handlers]
    finally:
        for logger, placeholder in placeholders:
            logger.removeHandler(placeholder)
        for logger, handlers in detached:
            for handler in handlers:
                logger.addHandler(handler)
