"""CLI logging: make the library's own warnings visible on stderr.

``circuitry/__init__.py`` attaches a ``NullHandler`` to the ``circuitry``
logger for embedding callers, which otherwise swallows every
``logger.warning()`` in the library (a malformed config, a failed template
render, a plugin hook failure, ...). The CLI adds a real handler on top.
"""

from __future__ import annotations

import logging
import sys
from typing import TextIO

_LOGGER_NAME = "circuitry"
_handler: logging.StreamHandler[TextIO] | None = None


def configure_cli_logging(*, verbose: bool = False) -> None:
    """(Re-)attach a stderr handler to the ``circuitry`` logger.

    ``WARNING`` by default, ``INFO`` under ``--verbose``. Writes to stderr
    only, so ``--json``/``--print`` stdout stays machine-readable. Safe to
    call on every command invocation: replaces any handler from a previous
    call rather than flushing it, since a test runner (CliRunner) closes its
    captured stderr between invocations and flushing a closed stream raises.
    """
    global _handler
    logger = logging.getLogger(_LOGGER_NAME)
    if _handler is not None:
        logger.removeHandler(_handler)
    _handler = logging.StreamHandler(sys.stderr)
    _handler.setFormatter(logging.Formatter("%(levelname)s: %(message)s"))
    logger.addHandler(_handler)
    logger.setLevel(logging.INFO if verbose else logging.WARNING)
