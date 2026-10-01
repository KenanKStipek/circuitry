from __future__ import annotations

import threading
from collections.abc import Callable
from copy import deepcopy
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ..saved_state import dumps_saved_state

#: ``Store.effect_start`` / ``Store.effect_complete``.
_EffectCallback = Callable[[str, dict[str, Any]], None]


def replace_node(
    state: dict[str, Any], parts: list[str], replacement: dict[str, Any]
) -> dict[str, Any]:
    """A shallow copy of *state* with the node at *parts* swapped out.

    Only the dicts along the path are copied; everything else stays a live
    reference, which is all a snapshot consumer (JSON dump, deepcopy) needs.
    """
    head, rest = parts[0], parts[1:]
    if not rest:
        return {**state, head: replacement}
    inner = state.get(head)
    if not isinstance(inner, dict):
        return state
    return {**state, head: replace_node(inner, rest, replacement)}


def _prefixed_effect_cb(
    callback: _EffectCallback | None, prefix: str
) -> _EffectCallback | None:
    """Wrap a lifecycle callback so branch-relative paths nest under *prefix*."""
    if callback is None or not prefix:
        return callback

    def _forward(path: str, payload: dict[str, Any]) -> None:
        callback(f"{prefix}.{path}", payload)

    return _forward


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

    They also carry a reference to the *root* state dict, so that a write
    deep in the tree still publishes a whole-run snapshot to ``on_write``
    rather than the subtree it happened to occur in — a subtree snapshot
    would overwrite ``--live-state`` with a fragment that no consumer can
    place.
    """

    state: dict[str, Any]
    on_write: Callable[[dict[str, Any]], None] | None = None
    effect_complete: Callable[[str, dict[str, Any]], None] | None = None
    effect_start: Callable[[str, dict[str, Any]], None] | None = None
    #: Fired once, synchronously, by a ``flow: tree`` loop or a parallel
    #: ``dynamic`` right before it submits its branches to a thread pool —
    #: ``(effect_path, branch_count)``. The one listener today is MCP's
    #: ``RunManager``, which uses the count to know how many "settle points"
    #: (a registered prompt, or a branch that finished without one) to wait
    #: for before ``start_run`` reports a snapshot, instead of guessing from
    #: a fixed debounce window that a scheduling delay can race (#237).
    concurrent_dispatch: Callable[[str, int], None] | None = None
    _path_prefix: str = ""
    _lock: threading.RLock = field(default_factory=threading.RLock, repr=False)
    #: The top-level state dict this store is a view into; None for a root.
    _root_state: dict[str, Any] | None = field(default=None, repr=False)
    #: The true run-root state dict, preserved through the isolation a
    #: parallel/tree branch applies to ``root_state``. None means this
    #: store's own ``root_state`` already is the true root.
    _true_root: dict[str, Any] | None = field(default=None, repr=False)

    @property
    def root_state(self) -> dict[str, Any]:
        """The whole-run state dict — this store's own state if it is a root."""
        return self.state if self._root_state is None else self._root_state

    @property
    def true_root_state(self) -> dict[str, Any]:
        """The whole-run state dict, even inside an isolated parallel/tree branch.

        ``root_state`` is deliberately reset to a branch's own isolated
        snapshot by :meth:`parallel_branches`, so each branch's ``on_write``
        publishes its own data rather than the whole run. This reference
        survives that isolation, so code that needs the actual run root
        regardless of nesting — e.g. a reflector reading a top-level
        ``goal`` effect — can still reach it from inside a ``flow: tree``
        dynamic or a parallel loop branch.
        """
        return self.root_state if self._true_root is None else self._true_root

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
                self.on_write(self.root_state)

    def child(self, path: str) -> Store:
        """Return a child Store rooted at *path*, sharing the same lock,
        on_write callback, effect lifecycle callbacks, and root state, and
        accumulating an absolute path prefix for canonical effect paths."""
        node = self.ensure_dict(path)
        new_prefix = f"{self._path_prefix}.{path}" if self._path_prefix else path
        return Store(
            state=node,
            on_write=self.on_write,
            effect_complete=self.effect_complete,
            effect_start=self.effect_start,
            concurrent_dispatch=self.concurrent_dispatch,
            _path_prefix=new_prefix,
            _lock=self._lock,
            _root_state=self.root_state,
            _true_root=self.true_root_state,
        )

    def parallel_branches(self, count: int) -> list[Store]:
        """One isolated store per parallel branch of this store's node.

        Isolated state, shared observation — the bargain a ``use`` child
        strikes, made per branch of a ``flow: tree`` loop or dynamic. Each
        branch writes into its own fresh state dict, so concurrent branches
        never touch shared dicts and never read each other's writes; the
        caller merges them back into this store's node, in index order, once
        every branch has finished.

        What a branch inherits is everything an observer needs to see it
        while it runs: the shared lock, the effect lifecycle callbacks with
        paths nested under this store's own (``prime.lp`` + ``iter_3.step``),
        and an ``on_write`` that republishes the whole run with every
        branch's latest snapshot laid over this node. A branch hands over a
        deep copy taken on its own thread — the only thread that writes it —
        so a snapshot never shares a dict another branch is still mutating.

        A branch's own ``root_state`` is reset to its isolated snapshot (so
        the ``on_write`` above publishes the right thing), but it still
        carries ``true_root_state`` through to the real run root, unaffected
        by the isolation.
        """
        latest: list[dict[str, Any] | None] = [None] * count
        on_write = self.on_write
        parts = self._path_prefix.split(".") if self._path_prefix else []

        def _publisher(index: int) -> Callable[[dict[str, Any]], None]:
            def _publish(branch_snapshot: dict[str, Any]) -> None:
                assert on_write is not None
                frozen = deepcopy(branch_snapshot)
                with self._lock:
                    latest[index] = frozen
                    node = dict(self.state)
                    for snapshot in latest:
                        if snapshot is not None:
                            node.update(snapshot)
                    on_write(
                        replace_node(self.root_state, parts, node) if parts else node
                    )

            return _publish

        return [
            Store(
                state={},
                on_write=None if on_write is None else _publisher(index),
                effect_complete=_prefixed_effect_cb(
                    self.effect_complete, self._path_prefix
                ),
                effect_start=_prefixed_effect_cb(self.effect_start, self._path_prefix),
                _lock=self._lock,
                _true_root=self.true_root_state,
            )
            for index in range(count)
        ]

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

    def fire_concurrent_dispatch(self, name: str, branch_count: int) -> None:
        """Notify ``concurrent_dispatch`` (if set) that *name* is about to run
        *branch_count* branches concurrently — called once, before any of
        them starts, from the same thread that submits them to the pool.
        """
        if self.concurrent_dispatch is None:
            return
        self.concurrent_dispatch(self.effect_path(name), branch_count)

    def dump_json(self, out_path: Path, *, pretty: bool = False) -> None:
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(
            dumps_saved_state(self.state, pretty=pretty) + "\n", encoding="utf-8"
        )
