from __future__ import annotations

import time
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import TYPE_CHECKING, Any, Literal, Optional, Sequence, Union

from ..adapters import Adapter
from ..output import console as _console
from .store import Store

if TYPE_CHECKING:
    from .dynamic import DynamicDefinition
    from .loop import LoopDefinition
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
class ConditionDef:
    """Defines how a condition is evaluated."""

    mode: Literal["model", "cel"] = "model"
    template: Optional[str] = None  # for mode: model
    expr: Optional[str] = None  # for mode: cel


@dataclass(frozen=True)
class ConditionalDefinition:
    """
    A Conditional evaluates an 'if' condition and executes exactly one branch.

    Per the spec:
    - type: conditional | if
    - if: ConditionDef (mode + template/expr)
    - then: list of effects
    - else: optional list of effects
    - name: optional (named decision vs transparent control)
    """

    name: Optional[str]
    condition: ConditionDef
    then_effects: Sequence[EffectDef]
    else_effects: Sequence[EffectDef] = ()

    # Evaluation behavior
    threshold: float = 0.5  # for model-based decisions
    on_error: Literal["fail", "continue", "skip"] = "fail"


class ConditionalRuntime:
    """
    Executes a ConditionalDefinition:
      1) Evaluate the condition (model or CEL)
      2) Select exactly one branch (then or else)
      3) Execute the selected branch's effects
      4) Record outcomes according to recording mode
    """

    def __init__(
        self,
        definition: ConditionalDefinition,
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
        from .dynamic import DynamicDefinition, DynamicRuntime
        from .loop import LoopDefinition, LoopRuntime
        from .prompt import PromptDefinition, PromptRuntime
        from .reflector import ReflectorDefinition, ReflectorRuntime

        # Named decision: create a node for this conditional
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
            meta["mode"] = self.defn.condition.mode
            meta["threshold"] = self.defn.threshold
            child_store = Store(node, on_write=store.on_write)
        else:
            node = None
            meta = None
            child_store = store

        # Evaluate condition
        try:
            result = self._evaluate_condition(ctx=ctx)
        except Exception as e:
            if meta:
                meta["error"] = str(e)
                meta["completed_at"] = _now_iso()
            if self.defn.on_error == "fail":
                raise
            elif self.defn.on_error == "skip":
                if node:
                    node["value"] = {"result": None, "branch": None, "effects": {}}
                return
            else:  # continue - default to else branch
                result = False

        branch = "then" if result else "else"
        effects_to_run = self.defn.then_effects if result else self.defn.else_effects

        if meta:
            meta["condition_result"] = result
            meta["branch"] = branch

        branch_indent = "  " * (self.depth + 1)
        if self.verbose:
            _console.print(f"{branch_indent}[info]branch: {branch}[/info]")

        # Execute selected branch effects
        executed_effects: list[dict[str, Any]] = []

        try:
            from .dynamic import _EFFECT_STYLE, _effect_type_label, _elapsed_str

            for idx, effect in enumerate(effects_to_run):
                effect_record = self._effect_record(effect=effect, index=idx)
                type_label = _effect_type_label(effect)
                icon, color = _EFFECT_STYLE.get(type_label, ("·", "white"))
                name = getattr(effect, "name", None) or "?"
                is_prompt = isinstance(effect, PromptDefinition)

                if self.verbose and not is_prompt:
                    _console.print(
                        f"{branch_indent}[info]→[/info] [{color}]{icon}[/{color}]"
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
                        ).execute(store=child_store, ctx=ctx)

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
                        ).execute(store=child_store)

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
                        ).execute(store=child_store, ctx=ctx)

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
                        ).execute(store=child_store, ctx=ctx)

                    elif isinstance(effect, ReflectorDefinition):
                        ReflectorRuntime(
                            effect,
                            adapter=self.adapter,
                            model=self.model,
                            runtime_config=self.runtime_config,
                            dry_run=self.dry_run,
                            timeout_seconds=self.timeout_seconds,
                            verbose=self.verbose,
                        ).execute(store=child_store)

                    if self.verbose and not is_prompt:
                        elapsed = time.monotonic() - t0
                        _console.print(
                            f"{branch_indent}[ok]✓[/ok] [{color}]{icon}[/{color}]"
                            f" {name} [dim]{_elapsed_str(elapsed)}[/dim]"
                        )

                except Exception:
                    if self.verbose and not is_prompt:
                        elapsed = time.monotonic() - t0
                        _console.print(
                            f"{branch_indent}[err]✗[/err] [{color}]{icon}[/{color}]"
                            f" {name} [dim]{_elapsed_str(elapsed)}[/dim]"
                        )
                    raise

                executed_effects.append(effect_record)

            if node:
                node["value"] = {
                    "result": result,
                    "branch": branch,
                    "effects": executed_effects,
                }
                if meta:
                    meta["completed_at"] = _now_iso()

        except Exception as e:
            if node:
                node["value"] = {
                    "result": result,
                    "branch": branch,
                    "effects": executed_effects,
                }
            if meta:
                meta["error"] = str(e)
                meta["completed_at"] = _now_iso()
            raise

    def _effect_record(self, *, effect: EffectDef, index: int) -> dict[str, Any]:
        effect_name = getattr(effect, "name", None)
        return {
            "index": index,
            "type": type(effect).__name__,
            "name": effect_name if isinstance(effect_name, str) else None,
        }

    def _evaluate_condition(self, *, ctx: dict[str, Any]) -> bool:
        """Evaluate the condition and return a boolean result."""
        if self.defn.condition.mode == "cel":
            return self._evaluate_cel(ctx=ctx)
        else:
            return self._evaluate_model(ctx=ctx)

    def _evaluate_model(self, *, ctx: dict[str, Any]) -> bool:
        """Cybernetic evaluation: invoke model with rendered template."""
        if self.dry_run:
            return True  # Default to then branch in dry run

        template = self.defn.condition.template or ""

        # Render template against context
        try:
            import chevron  # type: ignore[import-untyped]

            rendered = chevron.render(template, ctx)
        except Exception:
            rendered = template

        # Invoke model to get yes/no decision
        prompt = f"""Evaluate the following condition and respond with ONLY 'yes' or 'no':

{rendered}

Answer (yes/no):"""

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
        expr = self.defn.condition.expr or ""

        if not expr.strip():
            return False

        # Simple CEL-like evaluation using Python
        # For a full implementation, use a CEL library like cel-python
        # This is a simplified version that handles common patterns
        try:
            # Build evaluation context with 'state' as root
            eval_ctx = {"state": ctx}

            # Handle common CEL patterns
            # Convert state.foo.bar to state["foo"]["bar"]
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
        # state.input.foo -> state["input"]["foo"]
        def replace_dots(match: re.Match) -> str:
            parts = match.group(0).split(".")
            result = parts[0]
            for part in parts[1:]:
                result += f'["{part}"]'
            return result

        # Match identifiers with dots (but not inside strings)
        pattern = r"\bstate(?:\.[a-zA-Z_][a-zA-Z0-9_]*)+"
        converted = re.sub(pattern, replace_dots, expr)

        # Handle == and !=
        converted = converted.replace("==", " == ").replace("!=", " != ")

        # Handle && and ||
        converted = converted.replace("&&", " and ").replace("||", " or ")

        return converted
