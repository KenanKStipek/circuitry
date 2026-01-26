from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Literal, Optional, Sequence, TYPE_CHECKING, Union

from .store import Store
from ..adapters import Adapter

if TYPE_CHECKING:
    from .dynamic import DynamicDefinition
    from .prompt import PromptDefinition


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


EffectDef = Union["DynamicDefinition", "PromptDefinition", "ConditionalDefinition"]


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
        dry_run: bool = False,
        timeout_seconds: int = 120,
    ):
        self.defn = definition
        self.adapter = adapter
        self.model = model
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds

    def execute(self, *, store: Store, ctx: dict[str, Any]) -> None:
        from .dynamic import DynamicRuntime
        from .prompt import PromptRuntime, PromptDefinition

        # Named decision: create a node for this conditional
        # Transparent control: effects merge directly into parent
        is_named = bool(self.defn.name)

        if is_named:
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

        # Execute selected branch effects
        executed_effects: dict[str, Any] = {}

        try:
            for effect in effects_to_run:
                if isinstance(effect, PromptDefinition):
                    PromptRuntime(
                        effect,
                        adapter=self.adapter,
                        model=self.model,
                        dry_run=self.dry_run,
                        timeout_seconds=self.timeout_seconds,
                    ).execute(store=child_store, ctx=ctx)

                elif hasattr(effect, "effects"):  # DynamicDefinition
                    DynamicRuntime(
                        effect,
                        adapter=self.adapter,
                        model=self.model,
                        dry_run=self.dry_run,
                        timeout_seconds=self.timeout_seconds,
                    ).execute(store=child_store)

                elif isinstance(effect, ConditionalDefinition):
                    ConditionalRuntime(
                        effect,
                        adapter=self.adapter,
                        model=self.model,
                        dry_run=self.dry_run,
                        timeout_seconds=self.timeout_seconds,
                    ).execute(store=child_store, ctx=ctx)

            if node:
                node["value"] = {
                    "result": result,
                    "branch": branch,
                    "effects": executed_effects,
                }
                if meta:
                    meta["completed_at"] = _now_iso()

        except Exception as e:
            if meta:
                meta["error"] = str(e)
                meta["completed_at"] = _now_iso()
            raise

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
            import chevron

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
