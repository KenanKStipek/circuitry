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
    Child stores created via ``child()`` share the parent's lock,
    ``on_write`` callback, and the ``effect_start`` / ``effect_complete``
    callbacks so that concurrent access from parallel (tree) execution paths
    is serialised correctly and per-effect lifecycle hooks see the canonical
    absolute state path of every effect result.
    """

    state: dict[str, Any]
    on_write: Optional[Callable[[dict[str, Any]], None]] = None
    effect_complete: Optional[
        Callable[[str, dict[str, Any]], None]
    ] = None
    effect_start: Optional[
        Callable[[str, dict[str, Any]], None]
    ] = None
    _path_prefix: str = ""
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
        """Return a child Store rooted at *path*, sharing the same lock,
        on_write callback, effect lifecycle callbacks, and accumulating an
        absolute path prefix for canonical effect paths."""
        node = self.ensure_dict(path)
        new_prefix = f"{self._path_prefix}.{path}" if self._path_prefix else path
        return Store(
            state=node,
            on_write=self.on_write,
            effect_complete=self.effect_complete,
            effect_start=self.effect_start,
            _path_prefix=new_prefix,
            _lock=self._lock,
        )

    def effect_path(self, name: str) -> str:
        """The canonical dotted path of the effect *name* in this store."""
        return f"{self._path_prefix}.{name}" if self._path_prefix else name

    def fire_effect_start(self, name: str, effect_node: dict[str, Any]) -> None:
        """Notify ``effect_start`` (if set) that *name* is about to dispatch.

        The mirror of :meth:`fire_effect_complete`: same canonical dotted
        path, same callback shape. The payload is the effect's live state
        node as it stands *before* dispatch — meta the runtime has already
        recorded (adapter, model, and the complexity score when scoring is
        enabled) is therefore visible at start.
        """
        if self.effect_start is None:
            return
        self.effect_start(self.effect_path(name), effect_node)

    def fire_effect_complete(
        self, name: str, effect_result: dict[str, Any]
    ) -> None:
        """Notify ``effect_complete`` (if set) that *name* finished writing.

        Builds the canonical dotted path from the store's prefix + name
        and invokes the callback. Failures inside the callback are the
        callback's responsibility (the runtime catches via
        ``invoke_plugins`` semantics).

        Every effect that fires this also fires :meth:`fire_effect_start`
        first — including the error paths, so the pair stays balanced when
        an effect fails.
        """
        if self.effect_complete is None:
            return
        self.effect_complete(self.effect_path(name), effect_result)

    def dump_json(self, out_path: Path, *, pretty: bool = False) -> None:
        out_path.parent.mkdir(parents=True, exist_ok=True)
        if pretty:
            out_path.write_text(
                json.dumps(self.state, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )
        else:
            out_path.write_text(json.dumps(self.state) + "\n", encoding="utf-8")
