from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
from typing import TYPE_CHECKING, Any, Literal, Optional, Sequence, Union

from ..adapters import Adapter
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
        dry_run: bool = False,
        timeout_seconds: int = 120,
    ):
        self.defn = definition
        self.adapter = adapter
        self.model = model
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds

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
                else:
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
    ) -> dict[str, Any]:
        """Execute all effects in the loop body for one iteration."""
        from .conditional import ConditionalDefinition, ConditionalRuntime
        from .dynamic import DynamicDefinition, DynamicRuntime
        from .prompt import PromptDefinition, PromptRuntime
        from .reflector import ReflectorDefinition, ReflectorRuntime

        # Create iteration-specific store if named
        if self.defn.name:
            iter_key = f"iter_{iteration}"
            iter_node = store.ensure_dict(iter_key)
            iter_store = Store(iter_node, on_write=store.on_write)
        else:
            iter_store = store

        for effect in self.defn.body:
            if isinstance(effect, PromptDefinition):
                PromptRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                ).execute(store=iter_store, ctx=ctx)

            elif isinstance(effect, DynamicDefinition):
                DynamicRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                ).execute(store=iter_store)

            elif isinstance(effect, ConditionalDefinition):
                ConditionalRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                ).execute(store=iter_store, ctx=ctx)

            elif isinstance(effect, LoopDefinition):
                LoopRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                ).execute(store=iter_store, ctx=ctx)

            elif isinstance(effect, ReflectorDefinition):
                ReflectorRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                ).execute(store=iter_store)

        return {}  # Could capture executed effects here if needed
