"""The state inspector's model: a run's state dict as a browsable tree.

Circuitry state is a plain nested dict, and every template in an
orchestration addresses it by dot-path (``prime.draft.value``). So the
inspector is a filesystem browser over that dict: one node per key, the
path of the highlighted node is exactly what a template would write, and
selecting a node shows its value in full plus the ``meta`` of the effect
it belongs to — including, when scoring is on, the complexity score and
the per-signal breakdown that produced it.

Three sources feed the same tree:

*live* — snapshots a run publishes to a :class:`StateStore` while it is in
flight; *post-run* — the final state left in that store; *file* — a JSON
document ``cof run --out`` or ``--live-state`` wrote earlier. They differ
only in where the dict came from, so everything below takes a mapping and
nothing here imports Textual: the rules that decide what the inspector
shows are testable without booting an app.

Redaction is not re-applied here. ``runtime.effective_settings`` is
redacted by :mod:`circuitry.cli.redaction` on the way *into* state, and an
inspector that hid more than the stored value would misrepresent what a
``--out`` file actually contains.
"""

from __future__ import annotations

import json
import threading
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any

from .complexity import EffectComplexity
from .complexity import read as read_complexity
from .execution import DONE, FAILED, GLYPHS, PENDING, RUNNING, SKIPPED

__all__ = [
    "MAX_NODES",
    "MAX_PREVIEW",
    "EffectMeta",
    "LoadedState",
    "StateNode",
    "StateStore",
    "build_state_nodes",
    "detail_lines",
    "find_node",
    "flatten",
    "full_value_text",
    "load_state_file",
    "preview",
    "render_text",
]

#: Characters of a scalar shown inline on a tree row. The full value is
#: always one keypress away in the detail pane, so this only has to be
#: enough to recognise a value by.
MAX_PREVIEW = 56

#: Ceiling on the number of rows built from one state. A runaway state (a
#: loop over ten thousand items) must not freeze the view; what is left
#: out says so on the row that owns it.
MAX_NODES = 4000

#: Refuse to parse anything larger than this as a state file.
MAX_FILE_BYTES = 64 * 1024 * 1024

#: Keys whose children are structure rather than data, drawn but never
#: auto-expanded.
META_KEY = "meta"

#: Shown wherever a meta field is absent.
MISSING = "—"

_SCALAR_KINDS = {
    type(None): "null",
    bool: "boolean",
    int: "number",
    float: "number",
    str: "string",
}


# -- the tree ----------------------------------------------------------------


@dataclass(frozen=True)
class StateNode:
    """One key of the state dict, addressed the way a template would."""

    key: str
    #: Dot-path from the root of state — ``prime.over_items.iter_0.handle``.
    path: str
    value: Any
    #: ``mapping`` / ``list`` / ``string`` / ``number`` / ``boolean`` / ``null``.
    kind: str
    children: tuple[StateNode, ...] = ()
    #: ``meta`` of the nearest enclosing effect (this node's own, else the
    #: one it hangs under), so selecting ``…draft.value`` still explains
    #: which adapter produced it.
    meta: Mapping[str, Any] | None = None
    #: Path of the effect ``meta`` belongs to.
    meta_path: str = ""
    #: Children left out because the node cap was reached.
    omitted: int = 0

    @property
    def container(self) -> bool:
        return self.kind in ("mapping", "list")

    @property
    def preview(self) -> str:
        """One-line summary of the value, for the tree row."""
        return preview(self.value)

    @property
    def label(self) -> str:
        """The row as plain text: key, then what is under it."""
        return f"{self.key}  {self.preview}"


def _kind(value: Any) -> str:
    if isinstance(value, Mapping):
        return "mapping"
    if isinstance(value, (list, tuple)):
        return "list"
    return _SCALAR_KINDS.get(type(value), "value")


def _join(parent: str, key: str) -> str:
    """Append a mapping key to a dot-path, quoting one that is not a name.

    A key with a dot or a space in it cannot be written as ``a.b`` without
    changing what it addresses, so it gets the bracket form instead —
    copying a path that does not resolve would be worse than an ugly one.
    """
    if key.isidentifier():
        return f"{parent}.{key}" if parent else key
    quoted = json.dumps(key, ensure_ascii=False)
    return f"{parent}[{quoted}]" if parent else f"[{quoted}]"


class _Budget:
    """Mutable node allowance shared by one build."""

    def __init__(self, limit: int) -> None:
        self.left = limit

    def take(self) -> bool:
        if self.left <= 0:
            return False
        self.left -= 1
        return True


def build_state_nodes(
    state: Mapping[str, Any] | None, *, limit: int = MAX_NODES
) -> tuple[StateNode, ...]:
    """Read a state mapping into tree rows, ``prime`` first.

    ``prime`` leads because it is what a person came to look at; the
    runtime bookkeeping that sits beside it follows in its own order.
    """
    if not isinstance(state, Mapping):
        return ()
    budget = _Budget(limit)
    ordered = sorted(state.items(), key=lambda item: str(item[0]) != "prime")
    rows: list[StateNode] = []
    for key, value in ordered:
        if not budget.take():
            break
        rows.append(_node(str(key), _join("", str(key)), value, None, "", budget))
    return tuple(rows)


def _node(
    key: str,
    path: str,
    value: Any,
    meta: Mapping[str, Any] | None,
    meta_path: str,
    budget: _Budget,
) -> StateNode:
    kind = _kind(value)
    own_meta, own_meta_path = meta, meta_path
    if isinstance(value, Mapping):
        candidate = value.get(META_KEY)
        if isinstance(candidate, Mapping):
            # This node is an effect: its own meta explains it and
            # everything nested under it.
            own_meta, own_meta_path = candidate, path
    children, omitted = _children(value, path, own_meta, own_meta_path, budget)
    return StateNode(
        key=key,
        path=path,
        value=value,
        kind=kind,
        children=children,
        meta=own_meta,
        meta_path=own_meta_path,
        omitted=omitted,
    )


def _children(
    value: Any,
    path: str,
    meta: Mapping[str, Any] | None,
    meta_path: str,
    budget: _Budget,
) -> tuple[tuple[StateNode, ...], int]:
    if isinstance(value, Mapping):
        items = [(str(key), _join(path, str(key)), item) for key, item in value.items()]
    elif isinstance(value, (list, tuple)):
        items = [
            (f"[{index}]", f"{path}[{index}]", item) for index, item in enumerate(value)
        ]
    else:
        return (), 0

    rows: list[StateNode] = []
    for key, child_path, child in items:
        if not budget.take():
            return tuple(rows), len(items) - len(rows)
        rows.append(_node(key, child_path, child, meta, meta_path, budget))
    return tuple(rows), 0


def flatten(nodes: Sequence[StateNode]) -> list[StateNode]:
    """Every node of the tree, parents before children."""
    out: list[StateNode] = []
    for node in nodes:
        out.append(node)
        out.extend(flatten(node.children))
    return out


def find_node(nodes: Sequence[StateNode], path: str) -> StateNode | None:
    """The node at ``path``, or ``None`` when the tree no longer has one."""
    for node in flatten(nodes):
        if node.path == path:
            return node
    return None


def render_text(nodes: Sequence[StateNode]) -> str:
    """The whole tree as indented plain text (what tests assert against)."""
    lines: list[str] = []
    _render(nodes, 0, lines)
    return "\n".join(lines)


def _render(nodes: Sequence[StateNode], depth: int, out: list[str]) -> None:
    for node in nodes:
        out.append(f"{'  ' * depth}{node.label}")
        _render(node.children, depth + 1, out)
        if node.omitted:
            out.append(f"{'  ' * (depth + 1)}… {node.omitted} more")


# -- values ------------------------------------------------------------------


def preview(value: Any) -> str:
    """A one-line stand-in for a value, sized for a tree row."""
    if isinstance(value, Mapping):
        count = len(value)
        return "{}" if not count else f"{{{count} key{'s' if count != 1 else ''}}}"
    if isinstance(value, (list, tuple)):
        count = len(value)
        return "[]" if not count else f"[{count} item{'s' if count != 1 else ''}]"
    if isinstance(value, str):
        return _string_preview(value)
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    return _clip(repr(value))


def _string_preview(value: str) -> str:
    single = " ".join(value.split())
    body = _clip(single)
    if len(value) > MAX_PREVIEW or single != value:
        return f'"{body}"  ({len(value)} chars)'
    return f'"{body}"'


def _clip(text: str) -> str:
    return text if len(text) <= MAX_PREVIEW else f"{text[: MAX_PREVIEW - 1]}…"


def full_value_text(value: Any) -> str:
    """The value in full: strings verbatim, everything else as JSON."""
    if isinstance(value, str):
        return value
    try:
        return json.dumps(value, indent=2, ensure_ascii=False, sort_keys=False, default=str)
    except (TypeError, ValueError):  # pragma: no cover - default=str covers most
        return repr(value)


def type_line(node: StateNode) -> str:
    """The node's type and size — what the detail pane leads with."""
    if node.kind == "mapping":
        return f"mapping · {len(node.children)} key{'s' if len(node.children) != 1 else ''}"
    if node.kind == "list":
        return f"list · {len(node.children)} item{'s' if len(node.children) != 1 else ''}"
    if node.kind == "string":
        text = str(node.value)
        lines = text.count("\n") + 1
        return f"string · {len(text)} chars · {lines} line{'s' if lines != 1 else ''}"
    return node.kind


# -- the per-effect meta panel -----------------------------------------------


@dataclass(frozen=True)
class EffectMeta:
    """The bookkeeping the runtime writes beside every effect's value."""

    adapter: str = ""
    model: str = ""
    tokens_sent: int | None = None
    tokens_received: int | None = None
    created_at: str = ""
    completed_at: str = ""
    error: str = ""
    branch: str = ""
    disabled: bool = False
    #: The complexity score, when the effect has one. ``None`` is the
    #: ordinary case: scoring is off by default and only prompts are scored.
    complexity: EffectComplexity | None = None

    @classmethod
    def from_mapping(cls, meta: Mapping[str, Any] | None) -> EffectMeta:
        if not isinstance(meta, Mapping):
            return cls()
        return cls(
            adapter=_text(meta.get("adapter")),
            model=_text(meta.get("model")),
            tokens_sent=_count(meta.get("tokens_sent")),
            tokens_received=_count(meta.get("tokens_received")),
            created_at=_text(meta.get("created_at")),
            completed_at=_text(meta.get("completed_at")),
            error=_text(meta.get("error")),
            branch=_text(meta.get("branch")),
            disabled=bool(meta.get("disabled")),
            complexity=read_complexity(meta),
        )

    @property
    def status(self) -> str:
        """The same five statuses the execution view draws."""
        if self.disabled:
            return SKIPPED
        if self.error:
            return FAILED
        if self.completed_at:
            return DONE
        if self.created_at:
            return RUNNING
        return PENDING

    @property
    def elapsed(self) -> float | None:
        start, end = _time(self.created_at), _time(self.completed_at)
        if start is None or end is None:
            return None
        return max((end - start).total_seconds(), 0.0)

    def rows(self) -> list[tuple[str, str]]:
        """Label/value pairs for the meta panel, in reading order."""
        elapsed = self.elapsed
        completed = self.completed_at or MISSING
        if elapsed is not None:
            completed = f"{completed}  ({elapsed:.1f}s)"
        tokens = MISSING
        if self.tokens_sent is not None or self.tokens_received is not None:
            tokens = f"↑{self.tokens_sent or 0} ↓{self.tokens_received or 0}"
        rows = [
            ("status", f"{GLYPHS.get(self.status, '·')} {self.status}"),
            ("adapter", self.adapter or MISSING),
            ("model", self.model or MISSING),
            ("tokens", tokens),
            ("created", self.created_at or MISSING),
            ("completed", completed),
        ]
        if self.complexity is not None:
            rows.append(("complexity", self.complexity.summary))
        if self.branch:
            rows.append(("branch", self.branch))
        if self.error:
            rows.append(("error", self.error))
        return rows


def _text(value: Any) -> str:
    return "" if value is None else str(value)


def _count(value: Any) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def _time(value: str) -> datetime | None:
    if not value:
        return None
    try:
        return datetime.fromisoformat(value)
    except ValueError:
        return None


def detail_lines(node: StateNode | None, *, width: int = 12) -> list[str]:
    """The detail pane for ``node``: its path, its meta, then its value.

    A scored effect gets one section more: the signal breakdown behind its
    complexity score, strongest signal first, with the ones that account
    for most of the number marked. A score is a claim about the prompt,
    and this is the argument for it.
    """
    if node is None:
        return ["Nothing selected.", "", "Move the cursor in the tree to inspect a value."]
    lines = [node.path, type_line(node), ""]
    if node.meta is not None:
        meta = EffectMeta.from_mapping(node.meta)
        lines.append(f"meta — {node.meta_path or node.path}")
        lines += [f"  {label.ljust(width)}{value}" for label, value in meta.rows()]
        lines.append("")
        if meta.complexity is not None:
            lines.append("complexity signals")
            lines += [f"  {line}" for line in meta.complexity.breakdown_lines()]
            lines.append("")
    lines.append("value")
    body = full_value_text(node.value)
    lines += body.split("\n") if body else [MISSING]
    if node.omitted:
        lines += ["", f"{node.omitted} child nodes not shown (state exceeds {MAX_NODES} rows)."]
    return lines


# -- file mode ---------------------------------------------------------------


@dataclass(frozen=True)
class LoadedState:
    """The outcome of opening a state file — the state, or why there is none."""

    path: Path | None = None
    state: dict[str, Any] | None = None
    error: str = ""
    #: What to try next; shown under the error.
    hint: str = ""

    @property
    def ok(self) -> bool:
        return self.state is not None and not self.error


def load_state_file(path: Path) -> LoadedState:
    """Read a ``--out`` / ``--live-state`` document into a state mapping.

    Every failure is a value, not an exception: a half-written live-state
    file is an ordinary thing to open by accident, and the view has to be
    able to say so and stay usable.
    """
    try:
        size = path.stat().st_size
    except FileNotFoundError:
        return LoadedState(
            path=path,
            error=f"No such file: {path}",
            hint="Point this at what a run wrote with --out or --live-state.",
        )
    except OSError as exc:
        return LoadedState(path=path, error=f"Cannot read {path}: {exc.strerror or exc}")
    if path.is_dir():
        return LoadedState(
            path=path,
            error=f"{path} is a directory.",
            hint="Name the state JSON file inside it.",
        )
    if size > MAX_FILE_BYTES:
        return LoadedState(
            path=path,
            error=f"{path} is {size} bytes — too large to inspect.",
            hint=f"The inspector reads files up to {MAX_FILE_BYTES} bytes.",
        )
    try:
        raw = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        return LoadedState(
            path=path,
            error=f"Cannot read {path}: {exc}",
            hint="A state file is UTF-8 JSON.",
        )
    try:
        payload = json.loads(raw)
    except json.JSONDecodeError as exc:
        return LoadedState(
            path=path,
            error=f"Not valid JSON — {exc.msg} (line {exc.lineno}, column {exc.colno}).",
            hint=(
                "A --live-state file is rewritten after every effect; if the run "
                "is still going, open it again in a moment."
            ),
        )
    if not isinstance(payload, dict):
        return LoadedState(
            path=path,
            error=f"Top-level JSON is a {_kind(payload)}, not an object.",
            hint="State files hold an object with prime and runtime at the top.",
        )
    return LoadedState(path=path, state=payload)


# -- the live source ---------------------------------------------------------


@dataclass
class StateStore:
    """The run state views publish to and the inspector reads.

    Deliberately dumb: publishing swaps a reference and bumps a revision
    under a lock, so a run thread never waits on the UI, and the inspector
    polls that revision on its own timer instead of being pushed at. A run
    that outlives the screen it was launched from keeps updating this, so
    switching to the inspector mid-run shows the run.
    """

    state: dict[str, Any] = field(default_factory=dict)
    #: Bumped on every publish; the inspector redraws only when it changes.
    revision: int = 0
    #: What is being watched — usually the orchestration's label.
    label: str = ""
    running: bool = False
    _lock: threading.Lock = field(default_factory=threading.Lock, repr=False)

    @property
    def empty(self) -> bool:
        return not self.state

    def begin(self, label: str = "") -> None:
        """A run started: clear the last one and mark the store live."""
        with self._lock:
            self.state = {}
            self.label = label
            self.running = True
            self.revision += 1

    def publish(self, state: dict[str, Any], *, label: str | None = None) -> None:
        """Adopt a snapshot. Called from the run's worker thread."""
        with self._lock:
            self.state = state
            if label is not None:
                self.label = label
            self.revision += 1

    def finish(self, state: dict[str, Any] | None = None) -> None:
        """The run ended; keep its final state for post-run inspection."""
        with self._lock:
            if state:
                self.state = state
            self.running = False
            self.revision += 1

    def snapshot(self) -> tuple[int, dict[str, Any], bool]:
        """``(revision, state, running)`` read consistently."""
        with self._lock:
            return self.revision, self.state, self.running
