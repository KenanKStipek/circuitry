from __future__ import annotations

import logging
import time
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import TYPE_CHECKING, Any, Literal, Union

from ..adapters import Adapter
from ..output import console as _console
from .answers import parse_boolean_answer
from .disabled import is_enabled
from .scope import local_writes, scope_ctx
from .store import Store
from .templates import render_template

logger = logging.getLogger(__name__)

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
    template: str | None = None  # for mode: model
    expr: str | None = None  # for mode: cel
    #: mode: cel — raise instead of reading an unset ``state.`` path as
    #: false. For conditions where a missing field must not silently pick
    #: a branch (an order-exit rule, a safety gate).
    strict: bool = False


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

    name: str | None
    condition: ConditionDef
    then_effects: Sequence[EffectDef]
    else_effects: Sequence[EffectDef] = ()

    # Evaluation behavior
    threshold: float = 0.5  # for model-based decisions
    on_error: Literal["fail", "continue", "skip"] = "fail"

    # False = skip execution (condition + both branches) and write a disabled
    # node. The condition itself is never separately disableable — see
    # ``cli.profiles`` validation.
    enabled: bool = True

    # Free-form metadata, recorded on this conditional's own meta (named
    # only — an unnamed conditional has no node to carry it). Observability
    # tagging, not behavior — see DynamicDefinition.labels.
    labels: Mapping[str, Any] | None = None


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
        model_locked: bool = False,
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
        # See PromptRuntime: the run default was pinned by ``--model`` or a
        # profile's run-level ``model:``, so the complexity router defers to
        # it. Containers only carry the flag down to the prompts they run.
        self.model_locked = model_locked
        self.runtime_config = runtime_config or {}
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds
        self.verbose = verbose
        self.depth = depth
        self._ancestors = ancestors or []
        # Raw reply from the last `mode: model` evaluation, success or
        # failure — set inside _evaluate_model and read back in execute()
        # to record meta["answer"] alongside the parsed result. Token counts
        # ride the same path: this call bypasses PromptRuntime entirely, so
        # nowhere else ever sees what it spent.
        self._model_answer: str | None = None
        self._model_tokens_sent: int | None = None
        self._model_tokens_received: int | None = None

    def execute(self, *, store: Store, ctx: dict[str, Any]) -> None:
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
            meta["labels"] = dict(self.defn.labels) if self.defn.labels else None
            child_store = store.child(self.defn.name)
            # Before the condition is evaluated, so the decision's own start
            # brackets every start/complete pair its branch produces.
            store.fire_effect_start(self.defn.name, node)
            try:
                self._decide_and_run(
                    child_store=child_store, node=node, meta=meta, ctx=ctx
                )
            finally:
                # Balances the start on every exit: a taken branch, an
                # on_error: skip, and a failed condition or branch alike.
                store.fire_effect_complete(self.defn.name, node)
        else:
            self._decide_and_run(child_store=store, node=None, meta=None, ctx=ctx)

    def _decide_and_run(
        self,
        *,
        child_store: Store,
        node: dict[str, Any] | None,
        meta: dict[str, Any] | None,
        ctx: dict[str, Any],
    ) -> None:
        """Evaluate the condition and run the branch it selects."""
        from .dynamic import DynamicDefinition, DynamicRuntime
        from .loop import LoopDefinition, LoopRuntime
        from .prompt import PromptDefinition, PromptRuntime
        from .reflector import ReflectorDefinition, ReflectorRuntime
        from .tool import ToolDefinition, ToolRuntime
        from .use import UseDefinition, UseRuntime

        # Keys child_store already carried before this branch runs — for a
        # transparent (unnamed) conditional that is the enclosing scope's own
        # state (a loop iteration's prior siblings, an outer container's), so
        # the branch's own writes can be told apart from what it inherited.
        branch_baseline = frozenset(child_store.state)

        # Evaluate condition. A CEL expression that cannot be evaluated
        # raises (see ``cel_eval``); the failure is recorded on the effect
        # and, under the default ``on_error: fail``, propagates so the run
        # ends ``ok=False`` — the same shape a failing tool effect has.
        # Falling to the else branch is only ever an explicit opt-in via
        # ``on_error: continue``.
        try:
            result = self._evaluate_condition(ctx=ctx)
        except Exception as e:
            if meta:
                meta["error"] = str(e)
                meta["completed_at"] = _now_iso()
                if self.defn.condition.mode == "model":
                    meta["answer"] = self._model_answer
                    meta["adapter"] = getattr(self.adapter, "name", "unknown")
                    meta["model"] = self.model
                    meta["tokens_sent"] = self._model_tokens_sent
                    meta["tokens_received"] = self._model_tokens_received
            if self.defn.on_error == "fail":
                raise
            if self.defn.on_error == "skip":
                if node:
                    node["value"] = {"result": None, "branch": None, "effects": {}}
                return
            # continue - default to else branch
            logger.warning(
                "Conditional %r: condition failed (%s); on_error=continue, "
                "taking the else branch",
                self.defn.name or "<unnamed>",
                e,
            )
            result = False

        branch = "then" if result else "else"
        effects_to_run = self.defn.then_effects if result else self.defn.else_effects
        branch_names = frozenset(
            name
            for name in (getattr(e, "name", None) for e in effects_to_run)
            if isinstance(name, str) and name
        )

        if meta:
            meta["condition_result"] = result
            meta["branch"] = branch
            if self.defn.condition.mode == "model":
                meta["answer"] = self._model_answer
                meta["adapter"] = getattr(self.adapter, "name", "unknown")
                meta["model"] = self.model
                meta["tokens_sent"] = self._model_tokens_sent
                meta["tokens_received"] = self._model_tokens_received

        branch_indent = "  " * (self.depth + 1)
        if self.verbose:
            _console.print(f"{branch_indent}[info]branch: {branch}[/info]")

        # Execute selected branch effects
        executed_effects: list[dict[str, Any]] = []
        # Overlays are always rebuilt from the ctx the branch started with,
        # never from the previous overlay — see loop.py's _execute_body for
        # why layering copies on copies is the wrong move.
        base_ctx = ctx

        try:
            from .dynamic import (
                _EFFECT_STYLE,
                _effect_type_label,
                _elapsed_str,
                _skip_disabled_effect,
            )

            for idx, effect in enumerate(effects_to_run):
                effect_record = self._effect_record(effect=effect, index=idx)
                type_label = _effect_type_label(effect)
                icon, color = _EFFECT_STYLE.get(type_label, ("·", "white"))
                name = getattr(effect, "name", None) or "?"
                is_prompt = isinstance(effect, PromptDefinition)

                if not is_enabled(effect):
                    _skip_disabled_effect(
                        effect,
                        store=child_store,
                        indent=branch_indent,
                        icon=icon,
                        color=color,
                        verbose=self.verbose,
                    )
                    effect_record["disabled"] = True
                    executed_effects.append(effect_record)
                    # Expose the skip node to later branch steps on the same
                    # terms as a produced one (see the sibling merge below).
                    ctx = scope_ctx(
                        base_ctx, local_writes(child_store.state, branch_baseline, branch_names)
                    )
                    continue

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
                            model_locked=self.model_locked,
                            runtime_config=self.runtime_config,
                            dry_run=self.dry_run,
                            timeout_seconds=self.timeout_seconds,
                            verbose=self.verbose,
                            depth=self.depth + 1,
                            ancestors=self._ancestors,
                        ).execute(store=child_store, ctx=ctx)

                    elif isinstance(effect, DynamicDefinition):
                        DynamicRuntime(
                            effect,
                            adapter=self.adapter,
                            model=self.model,
                            model_locked=self.model_locked,
                            runtime_config=self.runtime_config,
                            dry_run=self.dry_run,
                            timeout_seconds=self.timeout_seconds,
                            verbose=self.verbose,
                            depth=self.depth + 2,
                            ancestors=self._ancestors,
                        ).execute(store=child_store, ctx_override=ctx)

                    elif isinstance(effect, ConditionalDefinition):
                        ConditionalRuntime(
                            effect,
                            adapter=self.adapter,
                            model=self.model,
                            model_locked=self.model_locked,
                            runtime_config=self.runtime_config,
                            dry_run=self.dry_run,
                            timeout_seconds=self.timeout_seconds,
                            verbose=self.verbose,
                            depth=self.depth + 1,
                            ancestors=self._ancestors,
                        ).execute(store=child_store, ctx=ctx)

                    elif isinstance(effect, LoopDefinition):
                        LoopRuntime(
                            effect,
                            adapter=self.adapter,
                            model=self.model,
                            model_locked=self.model_locked,
                            runtime_config=self.runtime_config,
                            dry_run=self.dry_run,
                            timeout_seconds=self.timeout_seconds,
                            verbose=self.verbose,
                            depth=self.depth + 1,
                            ancestors=self._ancestors,
                        ).execute(store=child_store, ctx=ctx)

                    elif isinstance(effect, ReflectorDefinition):
                        ReflectorRuntime(
                            effect,
                            adapter=self.adapter,
                            model=self.model,
                            model_locked=self.model_locked,
                            runtime_config=self.runtime_config,
                            dry_run=self.dry_run,
                            timeout_seconds=self.timeout_seconds,
                            verbose=self.verbose,
                        ).execute(store=child_store)

                    elif isinstance(effect, ToolDefinition):
                        ToolRuntime(
                            effect,
                            adapter=self.adapter,
                            model=self.model,
                            model_locked=self.model_locked,
                            runtime_config=self.runtime_config,
                            dry_run=self.dry_run,
                            timeout_seconds=self.timeout_seconds,
                            verbose=self.verbose,
                            depth=self.depth + 1,
                            ancestors=self._ancestors,
                        ).execute(store=child_store, ctx=ctx)

                    elif isinstance(effect, UseDefinition):
                        UseRuntime(
                            effect,
                            adapter=self.adapter,
                            model=self.model,
                            model_locked=self.model_locked,
                            runtime_config=self.runtime_config,
                            dry_run=self.dry_run,
                            timeout_seconds=self.timeout_seconds,
                            verbose=self.verbose,
                            depth=self.depth + 1,
                            ancestors=self._ancestors,
                        ).execute(store=child_store, ctx=ctx)

                    else:
                        raise TypeError(f"Unsupported effect type: {type(effect)}")

                    if self.verbose and not is_prompt:
                        elapsed = time.monotonic() - t0
                        _console.print(
                            f"{branch_indent}[ok]✓[/ok] [{color}]{icon}[/{color}]"
                            f" {name} [dim]{_elapsed_str(elapsed)}[/dim]"
                        )

                except Exception as _branch_exc:
                    if self.verbose and not is_prompt:
                        elapsed = time.monotonic() - t0
                        _console.print(
                            f"{branch_indent}[err]✗[/err] [{color}]{icon}[/{color}]"
                            f" {name} [dim]{_elapsed_str(elapsed)}[/dim]"
                        )
                    # Name the failing branch effect, the same way
                    # loop.py's body loop does — without this, a prompt's
                    # own error (which carries no name of its own) surfaces
                    # with nothing saying which effect in the branch failed.
                    effect_name = getattr(effect, "name", None)
                    if isinstance(effect_name, str) and effect_name:
                        raise RuntimeError(f"{effect_name}: {_branch_exc}") from _branch_exc
                    raise

                executed_effects.append(effect_record)

                # Make prior branch steps' outputs available to subsequent
                # branch steps under the canonical within-branch names — both
                # {{prime.<step>.value}} and the bare {{<step>.value}}. This is
                # what lets a step read a sibling earlier in the same branch,
                # in a loop body exactly as at the top level.
                ctx = scope_ctx(
                    base_ctx, local_writes(child_store.state, branch_baseline, branch_names)
                )

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
        return self._evaluate_model(ctx=ctx)

    def _evaluate_model(self, *, ctx: dict[str, Any]) -> bool:
        """Cybernetic evaluation: invoke model with rendered template."""
        self._model_answer = None
        self._model_tokens_sent = None
        self._model_tokens_received = None
        if self.dry_run:
            return True  # Default to then branch in dry run

        template = self.defn.condition.template or ""

        # Render template against context; a failure raises into the
        # caller's on_error handling rather than asking about raw text.
        rendered = render_template(template, ctx, label="if.template")

        # Invoke model to get yes/no decision
        prompt = f"""Evaluate the following condition and respond with ONLY 'yes' or 'no':

{rendered}

Answer (yes/no):"""

        res = self.adapter.generate(
            model=self.model,
            prompt=prompt,
            timeout_seconds=self.timeout_seconds,
        )
        self._model_answer = res.text
        self._model_tokens_sent = res.tokens_sent
        self._model_tokens_received = res.tokens_received

        # Parse response as a lenient yes/no; raises on an answer that
        # doesn't unambiguously read as one (see core.answers).
        return parse_boolean_answer(res.text or "")

    def _evaluate_cel(self, *, ctx: dict[str, Any]) -> bool:
        """Deterministic evaluation: evaluate CEL expression against state.

        Propagates ``CelEvaluationError`` — a broken expression is a
        defect, not a false condition. ``execute`` records it on the
        effect's ``meta.error`` and then honours ``on_error``. Under
        ``strict: true`` an unset ``state.`` path is such a defect too.
        """
        from .cel_eval import evaluate_cel

        return evaluate_cel(
            self.defn.condition.expr or "",
            ctx,
            strict=self.defn.condition.strict,
        )
