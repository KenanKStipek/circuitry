from __future__ import annotations

import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import TYPE_CHECKING, Any, Literal, Optional, Sequence, Union

from ..adapters import Adapter
from ..output import console as _console
from .store import Store

if TYPE_CHECKING:
    from .conditional import ConditionalDefinition
    from .dynamic import DynamicDefinition
    from .prompt import PromptDefinition
    from .reflector import ReflectorDefinition


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


EffectDef = Union[
    "DynamicDefinition",
    "PromptDefinition",
    "ConditionalDefinition",
    "LoopDefinition",
    "ReflectorDefinition",
]


@dataclass(frozen=True)
class LoopWhileDef:
    """Defines continuation condition for while loops."""

    mode: Literal["model", "cel"] = "model"
    template: Optional[str] = None  # for mode: model
    expr: Optional[str] = None  # for mode: cel


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

    name: Optional[str]
    body: Sequence[EffectDef]

    # Continuation strategy (exactly one of these should be set)
    while_def: Optional[LoopWhileDef] = None
    each_def: Optional[LoopEachDef] = None

    # Iteration bounds
    max_iterations: int = 100
    min_iterations: int = 0

    # Error behavior
    on_error: Literal["fail", "break", "continue"] = "fail"

    # Collection output: if set, aggregate this body effect's .value across all
    # iterations into an array written to prime.<loop_name>.collected.value
    collect: Optional[str] = None

    # Execution topology for each-loops: "chain" = sequential (default),
    # "tree" = parallel iterations via ThreadPoolExecutor.
    # while-loops always run sequentially regardless of this setting.
    flow: Literal["chain", "tree"] = "chain"

    # Maximum parallel workers when flow="tree". None = unbounded.
    max_concurrency: Optional[int] = None


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
    ):
        self.defn = definition
        self.adapter = adapter
        self.model = model
        self.runtime_config = runtime_config or {}
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds
        self.verbose = verbose
        self.depth = depth

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
            child_store = Store(node, on_write=store.on_write)
            iterations_effects: list[dict[str, Any]] = []
        else:
            node = None
            meta = None
            child_store = store
            iterations_effects = []

        iteration_count = 0
        termination_reason = "max_iterations"

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
                    # Parallel iteration: submit all at once, collect results in order
                    capped = collection[: self.defn.max_iterations]
                    total = len(capped)

                    iter_ctxs: list[tuple[int, dict[str, Any]]] = []
                    for idx, item in enumerate(capped):
                        iter_ctx = dict(ctx)
                        iter_ctx[self.defn.each_def.as_name] = item
                        iter_ctx["_loop_index"] = idx
                        iter_ctxs.append((idx, iter_ctx))

                    results: dict[int, dict[str, Any]] = {}
                    errors: dict[int, Exception] = {}

                    with ThreadPoolExecutor(
                        max_workers=self.defn.max_concurrency
                    ) as executor:
                        future_to_idx = {
                            executor.submit(
                                self._execute_body,
                                store=child_store,
                                ctx=iter_ctx,
                                iteration=idx,
                                parallel=True,
                            ): idx
                            for idx, iter_ctx in iter_ctxs
                        }
                        for future in as_completed(future_to_idx):
                            i = future_to_idx[future]
                            try:
                                results[i] = future.result()
                            except Exception as exc:
                                errors[i] = exc

                    # Assemble results in original order
                    for idx in range(total):
                        if idx in results:
                            iterations_effects.append(results[idx])
                            iteration_count += 1

                    if errors:
                        if self.defn.on_error == "fail":
                            termination_reason = "error"
                            raise next(iter(errors.values()))
                        elif self.defn.on_error == "break":
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

                        if self.verbose:
                            iter_indent = "  " * (self.depth + 1)
                            _console.print(f"{iter_indent}[info]iter {idx + 1}/{total}[/info]")

                        # Bind current item to context
                        iter_ctx = dict(ctx)
                        iter_ctx[self.defn.each_def.as_name] = item
                        iter_ctx["_loop_index"] = idx

                        try:
                            iter_effects = self._execute_body(
                                store=child_store,
                                ctx=iter_ctx,
                                iteration=idx,
                            )
                            iterations_effects.append(iter_effects)
                            iteration_count += 1
                        except Exception:
                            if self.defn.on_error == "fail":
                                termination_reason = "error"
                                raise
                            elif self.defn.on_error == "break":
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

                    if self.verbose:
                        iter_indent = "  " * (self.depth + 1)
                        _console.print(
                            f"{iter_indent}[info]iter {iteration_count + 1}[/info]"
                        )

                    try:
                        iter_effects = self._execute_body(
                            store=child_store,
                            ctx=ctx,
                            iteration=iteration_count,
                        )
                        iterations_effects.append(iter_effects)
                        iteration_count += 1
                    except Exception:
                        if self.defn.on_error == "fail":
                            termination_reason = "error"
                            raise
                        elif self.defn.on_error == "break":
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
                    collected: list[Any] = []
                    for i in range(iteration_count):
                        iter_node = node.get(f"iter_{i}")
                        if isinstance(iter_node, dict):
                            effect_node = iter_node.get(self.defn.collect)
                            if isinstance(effect_node, dict):
                                collected.append(effect_node.get("value"))
                    node["collected"] = {"value": collected}

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
                    collected_err: list[Any] = []
                    for i in range(iteration_count):
                        iter_node = node.get(f"iter_{i}")
                        if isinstance(iter_node, dict):
                            effect_node = iter_node.get(self.defn.collect)
                            if isinstance(effect_node, dict):
                                collected_err.append(effect_node.get("value"))
                    node["collected"] = {"value": collected_err}
            raise

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
                return []
            if current is None:
                return []

        if isinstance(current, list):
            return current
        return []

    def _evaluate_condition(self, *, ctx: dict[str, Any]) -> bool:
        """Evaluate the while condition and return a boolean result."""
        if not self.defn.while_def:
            return False

        if self.defn.while_def.mode == "cel":
            return self._evaluate_cel(ctx=ctx)
        else:
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
        if not self.defn.while_def:
            return False

        expr = self.defn.while_def.expr or ""

        if not expr.strip():
            return False

        try:
            # Build evaluation context with 'state' as root
            eval_ctx = {"state": ctx}

            # Convert CEL to Python
            py_expr = self._cel_to_python(expr)

            # Evaluate safely
            result = eval(py_expr, {"__builtins__": {}}, eval_ctx)
            return bool(result)
        except Exception:
            return False

    def _cel_to_python(self, expr: str) -> str:
        """Convert simple CEL expressions to Python."""
        import re

        # Replace dot notation with bracket notation for nested access
        def replace_dots(match: re.Match) -> str:
            parts = match.group(0).split(".")
            result = parts[0]
            for part in parts[1:]:
                result += f'["{part}"]'
            return result

        pattern = r"\bstate(?:\.[a-zA-Z_][a-zA-Z0-9_]*)+"
        converted = re.sub(pattern, replace_dots, expr)

        converted = converted.replace("==", " == ").replace("!=", " != ")
        converted = converted.replace("&&", " and ").replace("||", " or ")

        return converted

    def _execute_body(
        self,
        *,
        store: Store,
        ctx: dict[str, Any],
        iteration: int,
        parallel: bool = False,
    ) -> dict[str, Any]:
        """Execute all effects in the loop body for one iteration."""
        from .conditional import ConditionalDefinition, ConditionalRuntime
        from .dynamic import DynamicDefinition, DynamicRuntime, _effect_type_label
        from .prompt import PromptDefinition, PromptRuntime
        from .reflector import ReflectorDefinition, ReflectorRuntime

        # Create iteration-specific store if named
        if self.defn.name:
            iter_key = f"iter_{iteration}"
            iter_node = store.ensure_dict(iter_key)
            iter_store = Store(iter_node, on_write=store.on_write)
        else:
            iter_store = store

        from .dynamic import _EFFECT_STYLE, _elapsed_str

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

            if self.verbose and not is_prompt:
                _console.print(
                    f"{body_indent}[info]→[/info] [{color}]{icon}[/{color}]"
                    f" {name}"
                )

            t0 = time.monotonic()
            try:
                if is_prompt:
                    PromptRuntime(
                        effect,
                        adapter=self.adapter,
                        model=self.model,
                        runtime_config=self.runtime_config,
                        dry_run=self.dry_run,
                        timeout_seconds=self.timeout_seconds,
                        verbose=self.verbose,
                        depth=self.depth + 1,
                        # In parallel mode: print a static "→" start line instead of
                        # an animated Live spinner (multiple concurrent Live instances conflict).
                        cb_start=(
                            lambda _n=name, _ico=icon, _col=color, _ind=body_indent: _console.print(
                                f"{_ind}[info]→[/info] [{_col}]{_ico}[/{_col}] {_n}"
                            )
                        ) if parallel else None,
                        cb_done=_console.print if parallel else None,
                        cb_error=_console.print if parallel else None,
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
                    ).execute(store=iter_store)

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

                if self.verbose and not is_prompt:
                    elapsed = time.monotonic() - t0
                    _console.print(
                        f"{body_indent}[ok]✓[/ok] [{color}]{icon}[/{color}]"
                        f" {name} [dim]{_elapsed_str(elapsed)}[/dim]"
                    )

            except Exception:
                if self.verbose and not is_prompt:
                    elapsed = time.monotonic() - t0
                    _console.print(
                        f"{body_indent}[err]✗[/err] [{color}]{icon}[/{color}]"
                        f" {name} [dim]{_elapsed_str(elapsed)}[/dim]"
                    )
                raise

            executed.append(effect_record)

        return {
            "executed_effects": executed,
            "count": len(executed),
        }
