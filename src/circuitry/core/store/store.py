from __future__ import annotations

import json
import threading
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Optional


@dataclass
class Store:
    """
    Nested state store with optional persistence callbacks.

    Thread-safe: all mutations are protected by a reentrant lock.
    Child stores created via ``child()`` share the parent's lock and
    ``on_write`` callback so that concurrent access from parallel
    (tree) execution paths is serialised correctly.
    """

    state: dict[str, Any]
    on_write: Optional[Callable[[dict[str, Any]], None]] = None
    _lock: threading.RLock = field(default_factory=threading.RLock, repr=False)

    def get(self, path: str, default: Any = None) -> Any:
        cur: Any = self.state
        for part in path.split("."):
            if not isinstance(cur, dict) or part not in cur:
                return default
            cur = cur[part]
        return cur

    def ensure_dict(self, path: str) -> dict[str, Any]:
        with self._lock:
            cur: Any = self.state
            parts = [p for p in path.split(".") if p]
            for p in parts:
                if not isinstance(cur, dict):
                    raise TypeError(
                        f"Cannot descend into non-dict at '{p}' in path '{path}'"
                    )
                nxt = cur.get(p)
                if not isinstance(nxt, dict):
                    nxt = {}
                    cur[p] = nxt
                cur = nxt
            if not isinstance(cur, dict):
                raise TypeError(f"Expected dict at path '{path}', got {type(cur)}")
            return cur

    def set(self, path: str, value: Any) -> None:
        with self._lock:
            parts = [p for p in path.split(".") if p]
            if not parts:
                raise ValueError("Path cannot be empty")
            parent_path = ".".join(parts[:-1])
            key = parts[-1]
            parent = self.ensure_dict(parent_path) if parent_path else self.state
            parent[key] = value
            if self.on_write:
                self.on_write(self.state)

    def child(self, path: str) -> "Store":
        """Return a child Store rooted at *path*, sharing the same lock and on_write."""
        node = self.ensure_dict(path)
        return Store(state=node, on_write=self.on_write, _lock=self._lock)

    def dump_json(self, out_path: Path, *, pretty: bool = False) -> None:
        out_path.parent.mkdir(parents=True, exist_ok=True)
        if pretty:
            out_path.write_text(
                json.dumps(self.state, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )
        else:
            out_path.write_text(json.dumps(self.state) + "\n", encoding="utf-8")
