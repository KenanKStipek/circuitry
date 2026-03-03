from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any, Callable


def write_live_state(path: Path, state: dict[str, Any]) -> None:
    """Write state JSON atomically via tmp-file + rename."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(state) + "\n", encoding="utf-8")
    os.replace(str(tmp), str(path))


def make_live_state_callback(path: Path) -> Callable[[dict[str, Any]], None]:
    """Return a closure suitable for Store.on_write that writes state atomically."""

    def _callback(state: dict[str, Any]) -> None:
        write_live_state(path, state)

    return _callback
