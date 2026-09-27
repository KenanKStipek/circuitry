from __future__ import annotations

import os
from collections.abc import Callable
from pathlib import Path
from typing import Any

from ..core.saved_state import dumps_saved_state


def write_live_state(path: Path, state: dict[str, Any]) -> None:
    """Write state JSON atomically via tmp-file + rename.

    Silently skips the write if the state cannot be serialized to valid JSON
    (e.g. contains non-serializable objects mid-execution).
    """
    try:
        payload = dumps_saved_state(state) + "\n"
    except (TypeError, ValueError, OverflowError):
        return  # Skip — not valid JSON yet
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(payload, encoding="utf-8")
    os.replace(str(tmp), str(path))


def make_live_state_callback(path: Path) -> Callable[[dict[str, Any]], None]:
    """Return a closure suitable for Store.on_write that writes state atomically."""

    def _callback(state: dict[str, Any]) -> None:
        write_live_state(path, state)

    return _callback
