from __future__ import annotations

import logging
import threading
import time
from collections.abc import Sequence
from concurrent.futures import ThreadPoolExecutor, as_completed
from contextlib import nullcontext
from copy import deepcopy
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import TYPE_CHECKING, Any, Literal, Union

from ..adapters import Adapter
from ..output import console as _console
from .disabled import is_disabled_node, is_enabled
from .store import Store

logger = logging.getLogger(__name__)

if TYPE_CHECKING:
    from .conditional import ConditionalDefinition
    from .dynamic import DynamicDefinition
    from .prompt import PromptDefinition
    from .reflector import ReflectorDefinition
    from .tool import ToolDefinition


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


EffectDef = Union[
    "DynamicDefinition",
    "PromptDefinition",
    "ConditionalDefinition",
    "LoopDefinition",
    "ReflectorDefinition",
    "ToolDefinition",
]


@dataclass(frozen=True)
class LoopWhileDef:
    """Defines continuation condition for while loops."""

    mode: Literal["model", "cel"] = "model"
    template: str | None = None  # for mode: model
    expr: str | None = None  # for mode: cel


@dataclass(frozen=True)
class LoopEachDef:
    """Defines collection iteration for each loops."""

    in_path: str  # dot-delimited path into effective context
    as_name: str = "item"  # binding name for current element


@dataclass(frozen=True)
class LoopDefinition:
    """
    A Loop repeats execution of a body while a condition is true or over a collection.

    Per the spec:
    - type: loop
    - while: LoopWhileDef (mode + template/expr) OR
    - each: LoopEachDef (in + as)
    - body: list of effects
    - name: optional (named loop vs transparent control)
    """

    name: str | None
    body: Sequence[EffectDef]

    # Continuation strategy (exactly one of these should be set)
    while_def: LoopWhileDef | None = None
    each_def: LoopEachDef | None = None

    # Iteration bounds
    max_iterations: int = 100
    min_iterations: int = 0

    # Error behavior
    on_error: Literal["fail", "break", "continue"] = "fail"

    # Collection output: if set, aggregate this body effect's .value across all
    # iterations into an array written to prime.<loop_name>.collected.value
    collect: str | None = None

    # Execution topology for each-loops: "chain" = sequential (default),
    # "tree" = parallel iterations via ThreadPoolExecutor.
    # while-loops always run sequentially regardless of this setting.
    flow: Literal["chain", "tree"] = "chain"

    # Maximum parallel workers when flow="tree". None = unbounded.
    max_concurrency: int | None = None

    # False = skip execution (whole body, every iteration) and write a
    # disabled node.
    enabled: bool = True


class LoopRuntime:
    """
    Executes a LoopDefinition:
      1) Determine continuation strategy (while/each)
      2) For each iteration:
         - Check continuation condition
         - Execute body effects
         - Update state
      3) Record outcomes according to recording mode
    """

    def __init__(
        self,
        definition: LoopDefinition,
        *,
        adapter: Adapter,
        model: str,
        runtime_config: dict[str, Any] | None = None,
        dry_run: bool = False,
        timeout_seconds: int = 120,
        verbose: bool = False,
        depth: int = 0,
        ancestors: list | None = None,
    ):
        self.defn = definition
        self.adapter = adapter
        self.model = model
        self.runtime_config = runtime_config or {}
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds
        self.verbose = verbose
        self.depth = depth
        self._ancestors = ancestors or []

    def execute(self, *, store: Store, ctx: dict[str, Any]) -> None:
        # Named loop: create a node for this loop
        # Transparent control: effects merge directly into parent
        is_named = bool(self.defn.name)

        if is_named:
            assert self.defn.name is not None
            node = store.ensure_dict(self.defn.name)
            node.setdefault("value", None)
            meta = node.get("meta")
            if not isinstance(meta, dict):
                meta = {}
                node["meta"] = meta
            meta["created_at"] = _now_iso()
            meta["max_iterations"] = self.defn.max_iterations
            meta["min_iterations"] = self.defn.min_iterations
            child_store = store.child(self.defn.name)
            iterations_effects: list[dict[str, Any]] = []
            # Before the first iteration, so the loop's own start brackets
            # every start/complete pair its body produces.
            store.fire_effect_start(self.defn.name, node)
        else:
            node = None
            meta = None
            child_store = store
            iterations_effects = []

        iteration_count = 0
        termination_reason = "max_iterations"

        # Build ancestor context for children (this loop is now a parent)
        from .dynamic import _EFFECT_STYLE as _ES
        from .dynamic import AncestorContext

        _loop_t0 = time.monotonic()
        _loop_icon, _loop_color = _ES.get(f"loop:{self.defn.flow}", ("↻", "yellow"))
        self._child_ancestors = list(self._ancestors)
        self._child_ancestors.append(AncestorContext(
            name=self.defn.name or "loop",
            icon=_loop_icon,
            color=_loop_color,
            start=_loop_t0,
            indent="  " * self.depth,
        ))

        try:
            if self.defn.each_def:
                # Collection iteration mode
                if meta:
                    meta["mode"] = "each"
                    meta["each_in_path"] = self.defn.each_def.in_path
                    meta["each_as"] = self.defn.each_def.as_name

                collection = self._resolve_collection(ctx)

                if not collection:
                    termination_reason = "collection_exhausted"
                elif self.defn.flow == "tree":
                    # Parallel iteration: submit all at once, collect results in order.
                    # Each thread gets a deepcopy of ctx to prevent nested mutation
                    # bleed, and its own isolated Store to avoid concurrent dict writes.
                    capped = collection[: self.defn.max_iterations]
                    total = len(capped)

                    iter_ctxs: list[tuple[int, dict[str, Any]]] = []
                    for idx, item in enumerate(capped):
                        iter_ctx = deepcopy(ctx)
                        iter_ctx[self.defn.each_def.as_name] = item
                        iter_ctx["_loop_index"] = idx
                        iter_ctxs.append((idx, iter_ctx))

                    # Per-thread isolated stores: each thread writes into its own
                    # Store({}) so there is zero contention during parallel execution.
                    isolated_stores: dict[int, Store] = {
                        idx: Store(state={})
                        for idx in range(total)
                    }

                    results: dict[int, dict[str, Any]] = {}
                    errors: dict[int, Exception] = {}

                    # Build animated per-iteration tracker for verbose display
                    tree_tracker: _LoopIterTracker | None = None
                    if self.verbose and total > 0 and self.defn.body:
                        from .dynamic import _EFFECT_STYLE, _effect_type_label
                        _tl = _effect_type_label(self.defn.body[0])
                        _icon, _color = _EFFECT_STYLE.get(_tl, ("◆", "cyan"))
                        _bname = getattr(self.defn.body[0], "name", None) or "?"
                        tree_tracker = _LoopIterTracker(
                            total=total,
                            name=_bname,
                            indent="  " * (self.depth + 1),
                            icon=_icon,
                            color=_color,
                            ancestors=self._child_ancestors,
                        )

                    if tree_tracker is not None:
                        from rich.live import Live
                        live_ctx: Any = Live(
                            tree_tracker,
                            refresh_per_second=10,
                            transient=True,
                            console=_console,
                        )
                    else:
                        live_ctx = nullcontext()

                    with live_ctx:
                        with ThreadPoolExecutor(
                            max_workers=self.defn.max_concurrency
                        ) as executor:
                            future_to_idx = {
                                executor.submit(
                                    self._execute_body,
                                    store=isolated_stores[idx],
                                    ctx=iter_ctx,
                                    iteration=idx,
                                    parallel=True,
                                    tracker=tree_tracker,
                                    iter_label=f"[{idx}]",
                                ): idx
                                for idx, iter_ctx in iter_ctxs
                            }
                            for future in as_completed(future_to_idx):
                                i = future_to_idx[future]
                                try:
                                    results[i] = future.result()
                                except Exception as exc:
                                    errors[i] = exc

                    # Merge isolated stores back into child_store sequentially
                    for idx in range(total):
                        for key, value in isolated_stores[idx].state.items():
                            child_store.state[key] = value

                    # Fire on_write once after merge
                    if store.on_write:
                        store.on_write(store.state)

                    # Assemble results in original order
                    for idx in range(total):
                        if idx in results:
                            iterations_effects.append(results[idx])
                            iteration_count += 1

                    if errors:
                        if self.defn.on_error == "fail":
                            termination_reason = "error"
                            raise next(iter(errors.values()))
                        if self.defn.on_error == "break":
                            termination_reason = "error"
                        # continue: already skipped failed iterations above
                    else:
                        termination_reason = "collection_exhausted"
                else:
                    # Sequential iteration (default)
                    total = len(collection)
                    for idx, item in enumerate(collection):
                        if idx >= self.defn.max_iterations:
                            termination_reason = "max_iterations"
                            break

                        # Bind current item to context
                        iter_ctx = dict(ctx)
                        iter_ctx[self.defn.each_def.as_name] = item
                        iter_ctx["_loop_index"] = idx

                        try:
                            iter_effects = self._execute_body(
                                store=child_store,
                                ctx=iter_ctx,
                                iteration=idx,
                                iter_label=f"[{idx}]",
                            )
                            iterations_effects.append(iter_effects)
                            iteration_count += 1
                        except Exception:
                            if self.defn.on_error == "fail":
                                termination_reason = "error"
                                raise
                            if self.defn.on_error == "break":
                                termination_reason = "error"
                                break
                            # continue: skip this iteration
                    else:
                        termination_reason = "collection_exhausted"

            elif self.defn.while_def:
                # Condition-based iteration mode
                if meta:
                    meta["mode"] = self.defn.while_def.mode

                while iteration_count < self.defn.max_iterations:
                    # Check continuation condition
                    should_continue = self._evaluate_condition(ctx=ctx)

                    if (
                        not should_continue
                        and iteration_count >= self.defn.min_iterations
                    ):
                        termination_reason = "condition_false"
                        break

                    ctx["_loop_index"] = iteration_count
                    try:
                        iter_effects = self._execute_body(
                            store=child_store,
                            ctx=ctx,
                            iteration=iteration_count,
                            iter_label=f"[{iteration_count}]",
                        )
                        iterations_effects.append(iter_effects)
                        iteration_count += 1
                    except Exception:
                        if self.defn.on_error == "fail":
                            termination_reason = "error"
                            raise
                        if self.defn.on_error == "break":
                            termination_reason = "error"
                            break
                        # continue: skip this iteration
                        iteration_count += 1

            if node:
                node["value"] = {
                    "iterations": iteration_count,
                    "termination": {
                        "reason": termination_reason,
                    },
                    "effects_by_iteration": iterations_effects,
                }
                if meta:
                    meta["completed_at"] = _now_iso()

                # collect: aggregate the named body effect's .value across all iterations
                if self.defn.collect:
                    node["collected"] = {
                        "value": self._collect_values(node, iteration_count)
                    }

            if is_named and self.defn.name:
                store.fire_effect_complete(self.defn.name, node or {})

        except Exception as e:
            if meta:
                meta["error"] = str(e)
                meta["completed_at"] = _now_iso()
            if node:
                node["value"] = {
                    "iterations": iteration_count,
                    "termination": {
                        "reason": "error",
                        "detail": str(e),
                    },
                    "effects_by_iteration": iterations_effects,
                }
                if self.defn.collect:
                    node["collected"] = {
                        "value": self._collect_values(node, iteration_count)
                    }
            if is_named and self.defn.name:
                # Balances the start fired before the first iteration — a
                # loop that blew up still closes its pair.
                store.fire_effect_complete(self.defn.name, node or {})
            raise

    def _collect_values(
        self, node: dict[str, Any], iteration_count: int
    ) -> list[Any]:
        """Aggregate the ``collect`` target's value across every iteration.

        A disabled collect target contributes no slot at all: its node exists
        (value ``None``, ``meta.disabled``), but a caller reading ``collected``
        wants the values that were actually produced, so the skip is elided
        rather than surfacing as a run of ``None`` entries.
        """
        key = self.defn.collect
        if not key:
            return []
        collected: list[Any] = []
        for i in range(iteration_count):
            iter_node = node.get(f"iter_{i}")
            if not isinstance(iter_node, dict):
                continue
            effect_node = iter_node.get(key)
            if not isinstance(effect_node, dict):
                continue
            if is_disabled_node(effect_node):
                continue
            collected.append(effect_node.get("value"))
        return collected

    def _resolve_collection(self, ctx: dict[str, Any]) -> list[Any]:
        """Resolve the collection path to an actual list."""
        if not self.defn.each_def:
            return []

        path = self.defn.each_def.in_path

        # Navigate the path
        current: Any = ctx
        for part in path.split("."):
            if not part:
                continue
            if isinstance(current, dict):
                current = current.get(part)
            else:
                logger.warning(
                    "Loop collection path %r hit non-dict at segment %r; returning empty list",
                    path, part,
                )
                return []
            if current is None:
                logger.warning(
                    "Loop collection path %r resolved to None at segment %r; returning empty list",
                    path, part,
                )
                return []

        if isinstance(current, list):
            return current
        logger.warning(
            "Loop collection path %r resolved to %s instead of list; returning empty list",
            path, type(current).__name__,
        )
        return []

    def _evaluate_condition(self, *, ctx: dict[str, Any]) -> bool:
        """Evaluate the while condition and return a boolean result."""
        if not self.defn.while_def:
            return False

        if self.defn.while_def.mode == "cel":
            return self._evaluate_cel(ctx=ctx)
        return self._evaluate_model(ctx=ctx)

    def _evaluate_model(self, *, ctx: dict[str, Any]) -> bool:
        """Cybernetic evaluation: invoke model with rendered template."""
        if self.dry_run:
            return False  # Stop loop in dry run after first iteration

        template = self.defn.while_def.template if self.defn.while_def else ""

        # Render template against context
        try:
            import chevron  # type: ignore[import-untyped]

            rendered = chevron.render(template, ctx)
        except Exception:
            logger.warning("Loop while-template rendering failed; using raw template", exc_info=True)
            rendered = template

        # Invoke model to get yes/no decision
        prompt = f"""Evaluate the following condition and respond with ONLY 'yes' or 'no':

{rendered}

Should the loop continue? Answer (yes/no):"""

        res = self.adapter.generate(
            model=self.model,
            prompt=prompt,
            timeout_seconds=self.timeout_seconds,
        )

        # Parse response as boolean
        answer = (res.text or "").strip().lower()
        return answer in ("yes", "true", "1", "y")

    def _evaluate_cel(self, *, ctx: dict[str, Any]) -> bool:
        """Deterministic evaluation: evaluate CEL expression against state."""
        from .cel_eval import evaluate_cel

        if not self.defn.while_def:
            return False

        return evaluate_cel(self.defn.while_def.expr or "", ctx)

    def _execute_body(
        self,
        *,
        store: Store,
        ctx: dict[str, Any],
        iteration: int,
        parallel: bool = False,
        tracker: _LoopIterTracker | None = None,
        iter_label: str | None = None,
    ) -> dict[str, Any]:
        """Execute all effects in the loop body for one iteration."""
        from .conditional import ConditionalDefinition, ConditionalRuntime
        from .dynamic import DynamicDefinition, DynamicRuntime, _effect_type_label
        from .prompt import PromptDefinition, PromptRuntime
        from .reflector import ReflectorDefinition, ReflectorRuntime
        from .tool import ToolDefinition, ToolRuntime
        from .use import UseDefinition, UseRuntime

        # Create iteration-specific store if named
        if self.defn.name:
            iter_key = f"iter_{iteration}"
            iter_store = store.child(iter_key)
        else:
            iter_store = store

        from .dynamic import _EFFECT_STYLE, _elapsed_str, _skip_disabled_effect

        body_indent = "  " * (self.depth + 1)
        executed: list[dict[str, Any]] = []
        for effect in self.defn.body:
            effect_record = {
                "type": type(effect).__name__,
                "name": getattr(effect, "name", None),
            }
            type_label = _effect_type_label(effect)
            icon, color = _EFFECT_STYLE.get(type_label, ("·", "white"))
            name = getattr(effect, "name", None) or "?"
            is_prompt = isinstance(effect, PromptDefinition)
            is_tool = isinstance(effect, ToolDefinition)

            if not is_enabled(effect):
                _skip_disabled_effect(
                    effect,
                    store=iter_store,
                    indent=body_indent,
                    icon=icon,
                    color=color,
                    verbose=self.verbose,
                )
                effect_record["disabled"] = True
                executed.append(effect_record)
                # Expose the skip node to later body effects on the same terms
                # as a produced one (see the sibling merge below).
                if self.defn.name:
                    ctx = {**ctx, **iter_store.state}
                continue

            if self.verbose and not is_prompt and not is_tool:
                _console.print(
                    f"{body_indent}[info]→[/info] [{color}]{icon}[/{color}]"
                    f" {name}"
                )

            t0 = time.monotonic()
            try:
                if is_prompt:
                    if tracker is not None:
                        def _cb_start(_i=iteration):
                            return (tracker.on_start(_i))
                        def _cb_done(line, _i=iteration):
                            return (tracker.on_done(_i, line))
                        def _cb_error(line, _i=iteration):
                            return (tracker.on_error(_i, line))
                        def _cb_running(t, e, _i=iteration):
                            return (tracker.on_running(_i, t, e))
                    elif parallel:
                        def _cb_start(_n=name, _ico=icon, _col=color, _ind=body_indent):
                            return (_console.print(
                                                        f"{_ind}[info]→[/info] [{_col}]{_ico}[/{_col}] {_n}"
                                                    ))
                        _cb_done = _console.print
                        _cb_error = _console.print
                        _cb_running = None
                    else:
                        _cb_start = None
                        _cb_done = None
                        _cb_error = None
                        _cb_running = None

                    PromptRuntime(
                        effect,
                        adapter=self.adapter,
                        model=self.model,
                        runtime_config=self.runtime_config,
                        dry_run=self.dry_run,
                        timeout_seconds=self.timeout_seconds,
                        verbose=self.verbose,
                        depth=self.depth + 1,
                        cb_start=_cb_start,
                        cb_done=_cb_done,
                        cb_error=_cb_error,
                        cb_running=_cb_running,
                        display_name=f"{name} {iter_label}" if iter_label else None,
                        ancestors=self._child_ancestors if tracker is None else None,
                    ).execute(store=iter_store, ctx=ctx)

                elif isinstance(effect, DynamicDefinition):
                    DynamicRuntime(
                        effect,
                        adapter=self.adapter,
                        model=self.model,
                        runtime_config=self.runtime_config,
                        dry_run=self.dry_run,
                        timeout_seconds=self.timeout_seconds,
                        verbose=self.verbose,
                        depth=self.depth + 2,
                        ancestors=self._child_ancestors,
                    ).execute(store=iter_store, ctx_override=ctx)

                elif isinstance(effect, ConditionalDefinition):
                    ConditionalRuntime(
                        effect,
                        adapter=self.adapter,
                        model=self.model,
                        runtime_config=self.runtime_config,
                        dry_run=self.dry_run,
                        timeout_seconds=self.timeout_seconds,
                        verbose=self.verbose,
                        depth=self.depth + 1,
                        ancestors=self._child_ancestors,
                    ).execute(store=iter_store, ctx=ctx)

                elif isinstance(effect, LoopDefinition):
                    LoopRuntime(
                        effect,
                        adapter=self.adapter,
                        model=self.model,
                        runtime_config=self.runtime_config,
                        dry_run=self.dry_run,
                        timeout_seconds=self.timeout_seconds,
                        verbose=self.verbose,
                        depth=self.depth + 1,
                        ancestors=self._child_ancestors,
                    ).execute(store=iter_store, ctx=ctx)

                elif isinstance(effect, ReflectorDefinition):
                    ReflectorRuntime(
                        effect,
                        adapter=self.adapter,
                        model=self.model,
                        runtime_config=self.runtime_config,
                        dry_run=self.dry_run,
                        timeout_seconds=self.timeout_seconds,
                        verbose=self.verbose,
                    ).execute(store=iter_store)

                elif is_tool:
                    ToolRuntime(
                        effect,
                        runtime_config=self.runtime_config,
                        dry_run=self.dry_run,
                        timeout_seconds=self.timeout_seconds,
                        verbose=self.verbose,
                        depth=self.depth + 1,
                        display_name=f"{name} {iter_label}" if iter_label else None,
                        ancestors=self._child_ancestors if tracker is None else None,
                    ).execute(store=iter_store, ctx=ctx)

                elif isinstance(effect, UseDefinition):
                    UseRuntime(
                        effect,
                        adapter=self.adapter,
                        model=self.model,
                        runtime_config=self.runtime_config,
                        dry_run=self.dry_run,
                        timeout_seconds=self.timeout_seconds,
                        verbose=self.verbose,
                        depth=self.depth + 1,
                        ancestors=self._child_ancestors,
                    ).execute(store=iter_store, ctx=ctx)

                else:
                    raise TypeError(f"Unsupported effect type: {type(effect)}")

                if self.verbose and not is_prompt and not is_tool:
                    elapsed = time.monotonic() - t0
                    _console.print(
                        f"{body_indent}[ok]✓[/ok] [{color}]{icon}[/{color}]"
                        f" {name} [dim]{_elapsed_str(elapsed)}[/dim]"
                    )

            except Exception:
                if self.verbose and not is_prompt and not is_tool:
                    elapsed = time.monotonic() - t0
                    _console.print(
                        f"{body_indent}[err]✗[/err] [{color}]{icon}[/{color}]"
                        f" {name} [dim]{_elapsed_str(elapsed)}[/dim]"
                    )
                raise

            executed.append(effect_record)

            # Make prior body effects' outputs available to subsequent body
            # effects via short paths (e.g. {{backdrop.value}}).  This mirrors
            # how DynamicRuntime chain flow exposes sibling writes through the
            # shared root context dict.
            if self.defn.name:
                ctx = {**ctx, **iter_store.state}

        return {
            "executed_effects": executed,
            "count": len(executed),
        }


class _LoopIterTracker:
    """
    Tracks running state for parallel loop iterations.
    Rendered as a multi-line animated block inside a single rich.live.Live context.
    After Live exits (transient), done_lines are printed as static output.
    """

    _SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"

    def __init__(
        self,
        total: int,
        name: str,
        indent: str,
        icon: str,
        color: str,
        ancestors: list | None = None,
    ) -> None:
        self._lock = threading.Lock()
        self._total = total
        self._name = name
        self._states: list[str] = ["pending"] * total
        self._targets: list[str] = [""] * total
        self._estimated: list[int] = [0] * total
        self._item_starts: list[float | None] = [None] * total
        self._start = time.monotonic()
        self._indent = indent
        self._icon = icon
        self._color = color
        self._ancestors = ancestors or []

    def on_start(self, idx: int) -> None:
        with self._lock:
            if idx < self._total:
                self._states[idx] = "running"
                self._item_starts[idx] = time.monotonic()

    def on_running(self, idx: int, target: str, estimated_out: int) -> None:
        with self._lock:
            if idx < self._total:
                self._targets[idx] = target
                self._estimated[idx] = estimated_out

    def on_done(self, idx: int, line: str) -> None:
        with self._lock:
            if idx < self._total:
                self._states[idx] = "done"
        _console.print(line)

    def on_error(self, idx: int, line: str) -> None:
        with self._lock:
            if idx < self._total:
                self._states[idx] = "error"
        _console.print(line)

    def __rich__(self) -> str:
        from .dynamic import _elapsed_str, _render_ancestors

        now = time.monotonic()
        spinner_char = self._SPINNER[int(now * 8) % len(self._SPINNER)]
        with self._lock:
            states = list(self._states)
            targets = list(self._targets)
            estimated = list(self._estimated)
            item_starts = list(self._item_starts)
        ic = self._icon
        co = self._color

        # Render ancestor context lines above the iteration items
        lines = _render_ancestors(self._ancestors, self._SPINNER)

        for idx, state in enumerate(states):
            label = self._name if self._total == 1 else f"{self._name} [{idx}]"
            if state == "running":
                t = targets[idx]
                e = estimated[idx]
                parts: list[str] = []
                if t:
                    parts.append(t)
                if item_starts[idx] is not None:
                    parts.append(_elapsed_str(now - item_starts[idx]))
                if e:
                    parts.append(f"~{e}tok ↑")
                dim_suffix = f" [dim]{' | '.join(parts)}[/dim]" if parts else ""
                lines.append(
                    f"{self._indent}[info]{spinner_char}[/info] [{co}]{ic}[/{co}] {label}{dim_suffix}"
                )
            elif state == "pending":
                lines.append(f"{self._indent}[dim]· {ic} {label}[/dim]")
            # done/error: already printed above via on_done/on_error; omit from live display
        return "\n".join(lines)
