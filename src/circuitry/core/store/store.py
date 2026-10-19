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

#: ``Store.report`` -- ``(path, level, message, data)``.
_ReportCallback = Callable[[str, str, str, Any], None]


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


def _under_prefix(prefix: str, relative: str) -> str:
    """*relative* nested under *prefix* — *prefix* itself when *relative* is
    empty (an unnamed loop/dynamic dispatching directly in a branch whose
    own relative path is ""), never a trailing-dot ``f"{prefix}."``.
    """
    return f"{prefix}.{relative}" if relative else prefix


def _prefixed_effect_cb(
    callback: _EffectCallback | None, prefix: str
) -> _EffectCallback | None:
    """Wrap a lifecycle callback so branch-relative paths nest under *prefix*."""
    if callback is None or not prefix:
        return callback

    def _forward(path: str, payload: dict[str, Any]) -> None:
        callback(_under_prefix(prefix, path), payload)

    return _forward


def _prefixed_dispatch_cb(
    callback: Callable[[str, int, int], None] | None, prefix: str
) -> Callable[[str, int, int], None] | None:
    """Wrap ``concurrent_dispatch`` so a branch-relative path nests under
    *prefix*, the same rule :func:`_prefixed_effect_cb` applies to
    ``start``/``end`` — a nested tree loop's/dynamic's own ``dispatch``
    (#475) must use the same absolute path those already do.
    """
    if callback is None or not prefix:
        return callback

    def _forward(path: str, concurrency: int, total: int) -> None:
        callback(_under_prefix(prefix, path), concurrency, total)

    return _forward


def _prefixed_branch_settled_cb(
    callback: Callable[[str], None] | None, prefix: str
) -> Callable[[str], None] | None:
    """Wrap ``branch_settled`` the same way :func:`_prefixed_dispatch_cb`
    wraps ``concurrent_dispatch`` (#475) — both must agree on a nested
    dispatch's path, since ``RunManager`` pairs one's settle points against
    the other's announced count.
    """
    if callback is None or not prefix:
        return callback

    def _forward(path: str) -> None:
        callback(_under_prefix(prefix, path))

    return _forward


def _prefixed_report_cb(
    callback: _ReportCallback | None, prefix: str
) -> _ReportCallback | None:
    """Wrap ``report`` the same way :func:`_prefixed_effect_cb` wraps
    ``effect_start``/``effect_complete`` (#605) — a ``report`` fired from
    inside a parallel branch must nest under this store's own path too.
    """
    if callback is None or not prefix:
        return callback

    def _forward(path: str, level: str, message: str, data: Any) -> None:
        callback(_under_prefix(prefix, path), level, message, data)

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
    #: ``(effect_path, concurrency, total)``. ``concurrency`` already
    #: accounts for ``max_concurrency`` (the most that can ever be pending at
    #: once, not the total item count); MCP's ``RunManager`` is the one
    #: listener that uses it, to know how many "settle points" (a registered
    #: prompt, or a branch that finished without one) to wait for before
    #: ``start_run`` reports a snapshot, instead of guessing from a fixed
    #: debounce window that a scheduling delay can race (#237). ``total`` is
    #: the true branch count — the loop's item total, or ``len(effects)`` for
    #: a dynamic — added for ``--events``'s own `dispatch` event (#423).
    concurrent_dispatch: Callable[[str, int, int], None] | None = None
    #: Fired once per branch of a dispatch announced via ``concurrent_dispatch``,
    #: as soon as that branch's own execution genuinely finishes (whether or
    #: not it ever registered a prompt) — ``(effect_path,)``. Lets a listener
    #: lower how many settle points it's still owed by a branch that will
    #: never produce one (a tool-only branch, a ``when:`` skip), rather than
    #: waiting out the full safety-net budget for a count that can never be
    #: reached (#237).
    branch_settled: Callable[[str], None] | None = None
    #: Fired by a ``report`` effect (#605), never by any other — a report
    #: never fires ``effect_start``/``effect_complete``, so it never counts
    #: as work and never appears as a start/end event.
    #: ``(path, level, message, data)``: ``path`` is the report's own state
    #: path when it is named, otherwise the path of the container it runs
    #: in (see ``Store.fire_report``); ``data`` is the already-resolved
    #: value (``None`` when the report has none).
    report: _ReportCallback | None = None
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
    def own_path(self) -> str:
        """This store's own absolute dotted path prefix (``""`` at a run root).

        What ``effect_path`` composes a name onto — exposed directly for a
        caller that needs *this* store's own identity rather than a named
        child's, such as a transparent (unnamed) conditional/loop's model
        call (``core.effect_identity``), which is the container itself, not
        a further-named effect under it.
        """
        return self._path_prefix

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
            branch_settled=self.branch_settled,
            report=self.report,
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
                concurrent_dispatch=_prefixed_dispatch_cb(
                    self.concurrent_dispatch, self._path_prefix
                ),
                branch_settled=_prefixed_branch_settled_cb(
                    self.branch_settled, self._path_prefix
                ),
                report=_prefixed_report_cb(self.report, self._path_prefix),
                _lock=self._lock,
                _true_root=self.true_root_state,
            )
            for index in range(count)
        ]

    def effect_path(self, name: str) -> str:
        """The canonical dotted path of the effect *name* in this store."""
        if name is None:
            raise ValueError(
                "Store.effect_path requires a name; an unnamed container "
                "has no further-named path of its own -- use own_path"
            )
        return f"{self._path_prefix}.{name}" if self._path_prefix else name

    def fire_effect_start(self, name: str, effect_node: dict[str, Any]) -> None:
        """Notify ``effect_start`` (if set) that *name* is about to dispatch,
        and republish this store's own live view.

        The mirror of :meth:`fire_effect_complete`: same canonical dotted
        path, same callback shape. The payload is the effect's live state
        node as it stands *before* dispatch — meta the runtime has already
        recorded (adapter, model, and the complexity score when scoring is
        enabled) is therefore visible at start.

        Also calls ``on_write`` (if set) with this store's own
        ``root_state`` — every effect mutates its node directly rather than
        through :meth:`set` (so the mirror never otherwise wakes while it
        runs), and at the run's own root that direct mutation already lands
        in the one shared state dict the mirror serialises, with no further
        help needed. A tree-flow pass or a ``use`` child's root is an
        *isolated* dict instead (``Store.parallel_branches``, ``use.py``'s
        ``_child_on_write``) — nothing ever reads it until something
        publishes a snapshot, so without this call a running effect inside
        one stays invisible to ``--live-state`` until it finishes (#634).
        """
        if self.on_write is not None:
            self.on_write(self.root_state)
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

    def fire_concurrent_dispatch(
        self, name: str | None, concurrency: int, total: int
    ) -> None:
        """Notify ``concurrent_dispatch`` (if set) that *name* is about to run
        up to *concurrency* branches at once, out of *total* branches overall
        — called once, before any of them starts, from the same thread that
        submits them to the pool.

        *name* is ``None`` for an unnamed ``flow: tree`` loop (a dynamic is
        always named) — its branches run directly in this store rather than
        a further-named child, so the dispatch belongs to this store's own
        path (:attr:`own_path`), the enclosing container the passes actually
        run in, rather than ``effect_path``'s literal ``"None"`` suffix if
        handed straight through (#470).
        """
        if self.concurrent_dispatch is None:
            return
        path = self.own_path if name is None else self.effect_path(name)
        self.concurrent_dispatch(path, concurrency, total)

    def fire_branch_settled(self, name: str | None) -> None:
        """Notify ``branch_settled`` (if set) that one branch of *name*'s
        dispatch just finished — called once per branch, as it completes.

        *name* is ``None`` for an unnamed ``flow: tree`` loop, the same
        case :meth:`fire_concurrent_dispatch` handles — falls back to this
        store's own path (:attr:`own_path`) rather than ``effect_path``'s
        literal ``"None"`` suffix if handed straight through (#470, #475).
        """
        if self.branch_settled is None:
            return
        path = self.own_path if name is None else self.effect_path(name)
        self.branch_settled(path)

    def fire_report(
        self, name: str | None, *, level: str, message: str, data: Any
    ) -> None:
        """Notify ``report`` (if set) that a ``report`` effect published
        *message* (#605).

        *name* is ``None`` for an unnamed report — the same fallback
        :meth:`fire_concurrent_dispatch`/:meth:`fire_branch_settled` use:
        the path of the container it runs in (:attr:`own_path`) rather than
        ``effect_path``'s literal ``"None"`` suffix if handed straight
        through. Never fires ``effect_start``/``effect_complete``, so a
        report never counts as work and never appears as a start/end event.
        """
        if self.report is None:
            return
        path = self.own_path if name is None else self.effect_path(name)
        self.report(path, level, message, data)

    def dump_json(self, out_path: Path, *, pretty: bool = False) -> None:
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(
            dumps_saved_state(self.state, pretty=pretty) + "\n", encoding="utf-8"
        )
