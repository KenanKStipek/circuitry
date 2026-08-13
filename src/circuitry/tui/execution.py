"""The live execution model: orchestration structure + run events → a tree.

Two inputs, one picture. The *plan* comes from the orchestration file and
never changes during a run — it is the shape the user wrote: effects in
order, loop bodies, conditional branches, nested dynamics. The *run
events* come from the launcher: ``state_observer`` snapshots and
``effect_complete`` notifications. Overlaying the second on the first
yields the tree the Run view draws, plus the numbers its footer
aggregates.

Nothing here imports Textual, so the rules that decide *what a running
orchestration looks like* are testable without booting an app — feed a
plan and a state snapshot in, assert on nodes and totals.

Status is read off each effect's own node in state, which the runtime
already maintains:

``meta.created_at`` written when the effect starts, ``meta.completed_at``
when it lands, ``meta.error`` when it raised, ``meta.disabled`` when it
was compiled out. A named loop grows ``iter_<n>`` children as it goes; a
named conditional records ``meta.branch``. Anonymous (transparent)
control effects write straight into their parent's node, so they resolve
against the same scope and take their status from their children.
"""

from __future__ import annotations

import re
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass, field, replace
from datetime import datetime
from typing import Any

__all__ = [
    "CONTAINER_KINDS",
    "DONE",
    "FAILED",
    "GLYPHS",
    "PENDING",
    "RUNNING",
    "SKIPPED",
    "ExecNode",
    "PlanNode",
    "RenderLine",
    "Totals",
    "build_tree",
    "count_effects",
    "effect_scope",
    "format_totals",
    "plan_from_orchestration",
    "render_lines",
    "render_text",
    "sum_tokens",
    "totals_for",
]

#: The key the compiler roots every orchestration under.
ROOT_KEY = "prime"

PENDING = "pending"
RUNNING = "running"
DONE = "done"
FAILED = "failed"
SKIPPED = "skipped"

#: One glyph per status, so the tree reads at a glance.
GLYPHS: dict[str, str] = {
    PENDING: "·",
    RUNNING: "◐",
    DONE: "✓",
    FAILED: "✗",
    SKIPPED: "⊘",
}

#: Effect kinds that hold other effects.
CONTAINER_KINDS = frozenset({"dynamic", "loop", "conditional", "reflector"})

#: Synthetic grouping rows: structure, not work. Excluded from done/total.
GROUP_KINDS = frozenset({"iteration", "branch"})

#: How the runtime spells each effect type, normalised to one name.
_KINDS: dict[str, str] = {
    "prompt": "prompt",
    "tool": "tool",
    "use": "use",
    "loop": "loop",
    "dynamic": "dynamic",
    "reflector": "reflector",
    "if": "conditional",
    "conditional": "conditional",
}

_ITER_KEY = re.compile(r"^iter_(\d+)$")


# -- the plan ----------------------------------------------------------------


@dataclass(frozen=True)
class PlanNode:
    """One effect as the orchestration file declares it."""

    name: str
    kind: str
    #: ``chain`` / ``tree`` for containers, empty otherwise.
    flow: str = ""
    #: ``each`` / ``while`` for loops, empty otherwise.
    mode: str = ""
    on_error: str = ""
    #: Body effects (loop), child effects (dynamic), then-branch (conditional).
    children: tuple[PlanNode, ...] = ()
    #: Else-branch of a conditional; empty for everything else.
    else_children: tuple[PlanNode, ...] = ()

    @property
    def label(self) -> str:
        """What the tree calls this effect."""
        return self.name or f"<{self.kind}>"


def plan_from_orchestration(orch: Mapping[str, Any]) -> tuple[PlanNode, ...]:
    """Read an orchestration mapping into a plan tree, in declaration order.

    Tolerant by design: an unparseable effect becomes a leaf rather than an
    exception, because a half-understood tree still beats a blank pane.
    """
    effects = orch.get("effects")
    if not isinstance(effects, list):
        effects = orch.get("steps")
    return _plan_nodes(effects)


def _plan_nodes(effects: Any) -> tuple[PlanNode, ...]:
    if not isinstance(effects, list):
        return ()
    return tuple(
        _plan_node(effect) for effect in effects if isinstance(effect, Mapping)
    )


def _plan_node(effect: Mapping[str, Any]) -> PlanNode:
    kind = _KINDS.get(str(effect.get("type") or "").strip().lower(), "effect")
    name = str(effect.get("name") or "")
    flow = str(effect.get("flow") or "")
    on_error = str(effect.get("on_error") or "")

    if kind == "loop":
        mode = "each" if isinstance(effect.get("each"), Mapping) else ""
        if not mode and effect.get("while") is not None:
            mode = "while"
        return PlanNode(
            name=name,
            kind=kind,
            flow=flow,
            mode=mode,
            on_error=on_error,
            children=_plan_nodes(effect.get("body")),
        )
    if kind == "conditional":
        return PlanNode(
            name=name,
            kind=kind,
            on_error=on_error,
            children=_plan_nodes(effect.get("then")),
            else_children=_plan_nodes(effect.get("else")),
        )
    if kind in ("dynamic", "reflector"):
        return PlanNode(
            name=name,
            kind=kind,
            flow=flow,
            on_error=on_error,
            children=_plan_nodes(effect.get("effects")),
        )
    return PlanNode(name=name, kind=kind, on_error=on_error)


# -- the live tree -----------------------------------------------------------


@dataclass(frozen=True)
class ExecNode:
    """One row of the execution tree: an effect, as of a state snapshot."""

    label: str
    kind: str
    status: str = PENDING
    #: Wall time between ``created_at`` and ``completed_at``, when both exist.
    elapsed: float | None = None
    tokens_sent: int = 0
    tokens_received: int = 0
    error: str | None = None
    #: What the orchestration does about this node's error, when it has one.
    on_error: str = ""
    #: Short annotation: the branch taken, the loop mode, the flow model.
    detail: str = ""
    #: ``tree`` when this node runs its children in parallel.
    flow: str = ""
    children: tuple[ExecNode, ...] = field(default_factory=tuple)

    @property
    def finished(self) -> bool:
        return self.status in (DONE, FAILED, SKIPPED)

    @property
    def parallel(self) -> bool:
        return self.flow == "tree"


def effect_scope(state: Mapping[str, Any]) -> Mapping[str, Any] | None:
    """The node every top-level effect writes under (``prime``)."""
    root = state.get(ROOT_KEY)
    return root if isinstance(root, Mapping) else None


def build_tree(
    plan: Sequence[PlanNode],
    state: Mapping[str, Any] | None = None,
    *,
    completed: Iterable[str] = (),
    running: bool | None = None,
) -> tuple[ExecNode, ...]:
    """Overlay a state snapshot on ``plan`` and return the rows to draw.

    ``completed`` carries effect paths seen via ``effect_complete``. Tree
    flow merges its children's state back only once every sibling lands,
    so those notifications are the only way to show a parallel sibling
    finishing ahead of its neighbours.

    ``running`` overrides what the snapshot implies about the run being
    in flight — a caller holding the run session knows before the first
    snapshot arrives.
    """
    snapshot = state or {}
    scope = effect_scope(snapshot)
    nodes = _nodes(plan, scope, set(completed), ROOT_KEY)
    root = _root_meta(scope)
    if _in_flight(root) if running is None else running:
        # The runtime writes an effect's node before it starts but only
        # publishes a snapshot when one lands, so the effect currently in
        # flight looks pending. Walk down from the running root marking
        # what must be under way: the next unfinished child of a chain,
        # every unfinished child of a parallel one.
        nodes = _advance(nodes, parallel=_flow_of(root) == "tree")
    return nodes


def _root_meta(scope: Mapping[str, Any] | None) -> Mapping[str, Any] | None:
    meta = scope.get("meta") if isinstance(scope, Mapping) else None
    return meta if isinstance(meta, Mapping) else None


def _in_flight(meta: Mapping[str, Any] | None) -> bool:
    if meta is None:
        return False
    return bool(meta.get("created_at")) and not (
        meta.get("completed_at") or meta.get("error")
    )


def _flow_of(meta: Mapping[str, Any] | None) -> str:
    flow = meta.get("flow") if meta else None
    return str(flow) if isinstance(flow, str) else ""


def _advance(nodes: Sequence[ExecNode], *, parallel: bool) -> tuple[ExecNode, ...]:
    """Mark the children of a running node that must be running too."""
    unfinished = [index for index, node in enumerate(nodes) if not node.finished]
    if not unfinished:
        return tuple(nodes)
    current = set(unfinished) if parallel else {unfinished[0]}
    return tuple(
        _running_now(node) if index in current else node
        for index, node in enumerate(nodes)
    )


def _running_now(node: ExecNode) -> ExecNode:
    return replace(
        node,
        status=RUNNING if node.status == PENDING else node.status,
        children=_advance(node.children, parallel=node.parallel),
    )


def _nodes(
    plan: Sequence[PlanNode],
    scope: Mapping[str, Any] | None,
    completed: set[str],
    path: str,
) -> tuple[ExecNode, ...]:
    return tuple(_node(item, scope, completed, path) for item in plan)


def _node(
    plan: PlanNode,
    scope: Mapping[str, Any] | None,
    completed: set[str],
    path: str,
) -> ExecNode:
    # A named effect owns a node in state; an anonymous control effect is
    # transparent — its children write into the parent's node.
    if plan.name:
        node = scope.get(plan.name) if isinstance(scope, Mapping) else None
        node = node if isinstance(node, Mapping) else None
        child_scope = node
        node_path = f"{path}.{plan.name}"
    else:
        node = None
        child_scope = scope
        node_path = path

    meta = node.get("meta") if isinstance(node, Mapping) else None
    meta = meta if isinstance(meta, Mapping) else None

    children = _children(plan, node, child_scope, completed, node_path)
    status = _status(meta, children, done=node_path in completed)
    if status == RUNNING:
        children = _advance(children, parallel=plan.flow == "tree")
    error = str(meta.get("error")) if meta and meta.get("error") else None

    return ExecNode(
        label=plan.label,
        kind=plan.kind,
        status=status,
        elapsed=_elapsed(meta),
        tokens_sent=_tokens(meta, "tokens_sent"),
        tokens_received=_tokens(meta, "tokens_received"),
        error=error,
        on_error=plan.on_error if error else "",
        detail=_detail(plan, meta),
        flow=plan.flow,
        children=children,
    )


def _children(
    plan: PlanNode,
    node: Mapping[str, Any] | None,
    scope: Mapping[str, Any] | None,
    completed: set[str],
    path: str,
) -> tuple[ExecNode, ...]:
    if plan.kind == "loop":
        return _loop_children(plan, scope, completed, path)
    if plan.kind == "conditional":
        return _branch_children(plan, node, scope, completed, path)
    return _nodes(plan.children, scope, completed, path)


def _loop_children(
    plan: PlanNode,
    scope: Mapping[str, Any] | None,
    completed: set[str],
    path: str,
) -> tuple[ExecNode, ...]:
    """Iterations as they appear; the body as a preview until one does."""
    iterations = _iteration_keys(scope)
    if not iterations:
        return _nodes(plan.children, None, completed, path)
    rows: list[ExecNode] = []
    for index, key in iterations:
        iter_scope = scope.get(key) if isinstance(scope, Mapping) else None
        body = _nodes(
            plan.children,
            iter_scope if isinstance(iter_scope, Mapping) else None,
            completed,
            f"{path}.{key}",
        )
        rows.append(
            ExecNode(
                label=f"iter {index}",
                kind="iteration",
                status=_rollup(body),
                children=body,
            )
        )
    return tuple(rows)


def _iteration_keys(scope: Mapping[str, Any] | None) -> list[tuple[int, str]]:
    if not isinstance(scope, Mapping):
        return []
    found: list[tuple[int, str]] = []
    for key in scope:
        match = _ITER_KEY.match(str(key))
        if match is not None:
            found.append((int(match.group(1)), str(key)))
    return sorted(found)


def _branch_children(
    plan: PlanNode,
    node: Mapping[str, Any] | None,
    scope: Mapping[str, Any] | None,
    completed: set[str],
    path: str,
) -> tuple[ExecNode, ...]:
    """The taken branch once it is known, both branches before that."""
    meta = node.get("meta") if isinstance(node, Mapping) else None
    branch = meta.get("branch") if isinstance(meta, Mapping) else None

    def group(name: str, plans: Sequence[PlanNode], live: bool) -> ExecNode:
        rows = _nodes(plans, scope if live else None, completed, path)
        return ExecNode(
            label=name,
            kind="branch",
            status=_rollup(rows) if live else PENDING,
            children=rows,
        )

    if branch == "then":
        return (group("then", plan.children, True),)
    if branch == "else":
        return (group("else", plan.else_children, True),)
    groups = [group("then", plan.children, False)]
    if plan.else_children:
        groups.append(group("else", plan.else_children, False))
    return tuple(groups)


def _status(
    meta: Mapping[str, Any] | None, children: Sequence[ExecNode], *, done: bool
) -> str:
    if meta is not None:
        if meta.get("disabled"):
            return SKIPPED
        if meta.get("error"):
            return FAILED
        if meta.get("completed_at"):
            return DONE
        if meta.get("created_at"):
            return RUNNING
    if done:
        return DONE
    # No node of its own (anonymous control, or state not written yet):
    # the children say whether anything is happening down there.
    return _rollup(children) if children else PENDING


def _rollup(children: Sequence[ExecNode]) -> str:
    """A group's status is the summary of what is underneath it."""
    if not children:
        return PENDING
    if any(child.status == FAILED for child in children):
        return FAILED
    if any(child.status == RUNNING for child in children):
        return RUNNING
    if all(child.status == SKIPPED for child in children):
        return SKIPPED
    if all(child.finished for child in children):
        return DONE
    if any(child.finished for child in children):
        return RUNNING
    return PENDING


def _detail(plan: PlanNode, meta: Mapping[str, Any] | None) -> str:
    bits: list[str] = []
    if plan.kind == "loop":
        # The plan is the honest source here: a while-loop's ``meta.mode``
        # records how its *condition* is evaluated, not how it iterates.
        if plan.mode:
            bits.append(plan.mode)
        if plan.flow == "tree":
            bits.append("parallel")
    elif plan.kind == "conditional":
        branch = meta.get("branch") if meta else None
        if branch:
            bits.append(f"→ {branch}")
    elif plan.kind in ("dynamic", "reflector"):
        flow = str(meta.get("flow")) if meta and meta.get("flow") else plan.flow
        if flow:
            bits.append("parallel" if flow == "tree" else flow)
    return " ".join(bits)


def _elapsed(meta: Mapping[str, Any] | None) -> float | None:
    if meta is None:
        return None
    start = _parse_time(meta.get("created_at"))
    end = _parse_time(meta.get("completed_at"))
    if start is None or end is None:
        return None
    return max((end - start).total_seconds(), 0.0)


def _parse_time(value: Any) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        return datetime.fromisoformat(value)
    except ValueError:
        return None


def _tokens(meta: Mapping[str, Any] | None, key: str) -> int:
    if meta is None:
        return 0
    value = meta.get(key)
    return value if isinstance(value, int) and not isinstance(value, bool) else 0


# -- aggregates --------------------------------------------------------------


@dataclass(frozen=True)
class Totals:
    """What the footer bar reports for the run as a whole."""

    tokens_sent: int = 0
    tokens_received: int = 0
    done: int = 0
    total: int = 0
    elapsed: float | None = None


def sum_tokens(state: Mapping[str, Any] | None) -> tuple[int, int]:
    """Sum ``tokens_sent`` / ``tokens_received`` over every effect's meta.

    Walks the state itself rather than the rendered tree, so the footer
    counts effects the tree cannot show (a sub-orchestration's internals,
    reflector-generated work) exactly as a final-state audit would.
    """
    sent = received = 0
    stack: list[Any] = [state or {}]
    while stack:
        current = stack.pop()
        if isinstance(current, Mapping):
            meta = current.get("meta")
            if isinstance(meta, Mapping):
                sent += _tokens(meta, "tokens_sent")
                received += _tokens(meta, "tokens_received")
            for key, value in current.items():
                if key != "meta":
                    stack.append(value)
        elif isinstance(current, list):
            stack.extend(current)
    return sent, received


def count_effects(nodes: Sequence[ExecNode]) -> tuple[int, int]:
    """``(finished, total)`` over the real effects in the tree."""
    done = total = 0
    for node in nodes:
        if node.kind not in GROUP_KINDS:
            total += 1
            if node.finished:
                done += 1
        child_done, child_total = count_effects(node.children)
        done += child_done
        total += child_total
    return done, total


def totals_for(
    nodes: Sequence[ExecNode],
    state: Mapping[str, Any] | None = None,
    *,
    elapsed: float | None = None,
) -> Totals:
    """Aggregate the footer numbers for one snapshot."""
    sent, received = sum_tokens(state)
    done, total = count_effects(nodes)
    return Totals(
        tokens_sent=sent,
        tokens_received=received,
        done=done,
        total=total,
        elapsed=elapsed,
    )


# -- rendering ---------------------------------------------------------------


@dataclass(frozen=True)
class RenderLine:
    """One drawable row: its text, and the status that colours it."""

    text: str
    status: str


def render_lines(nodes: Sequence[ExecNode]) -> list[RenderLine]:
    """Flatten the tree into rows with box-drawing connectors."""
    lines: list[RenderLine] = []
    _render(nodes, "", lines)
    return lines


def _render(nodes: Sequence[ExecNode], prefix: str, out: list[RenderLine]) -> None:
    for index, node in enumerate(nodes):
        last = index == len(nodes) - 1
        stem = "└─ " if last else "├─ "
        out.append(RenderLine(f"{prefix}{stem}{_row(node)}", node.status))
        below = f"{prefix}{'   ' if last else '│  '}"
        if node.error:
            out.append(RenderLine(f"{below}   ↳ {_error_text(node)}", FAILED))
        _render(node.children, below, out)


def _row(node: ExecNode) -> str:
    parts = [GLYPHS.get(node.status, "·"), node.label]
    if node.detail:
        parts.append(node.detail)
    stats = _stats(node)
    if stats:
        parts.append(stats)
    return " ".join(parts)


def _stats(node: ExecNode) -> str:
    bits: list[str] = []
    if node.elapsed is not None:
        bits.append(_seconds(node.elapsed))
    if node.tokens_sent or node.tokens_received:
        bits.append(f"↑{node.tokens_sent} ↓{node.tokens_received}")
    return f"({', '.join(bits)})" if bits else ""


def _error_text(node: ExecNode) -> str:
    outcome = f" [on_error: {node.on_error}]" if node.on_error else ""
    return f"{node.error}{outcome}"


def _seconds(value: float) -> str:
    if value >= 60:
        minutes, rest = divmod(value, 60)
        return f"{int(minutes)}m{rest:04.1f}s"
    return f"{value:.1f}s"


def render_text(nodes: Sequence[ExecNode]) -> str:
    """The whole tree as plain text (what tests assert against)."""
    return "\n".join(line.text for line in render_lines(nodes))


def format_totals(totals: Totals) -> str:
    """The footer bar: tokens both ways, wall clock, effects done."""
    elapsed = "—" if totals.elapsed is None else _seconds(totals.elapsed)
    return (
        f"↑{totals.tokens_sent} ↓{totals.tokens_received} tok"
        f"  ·  {elapsed}"
        f"  ·  {totals.done}/{totals.total} effects"
    )
