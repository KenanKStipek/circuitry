from __future__ import annotations

from typing import Any, Union

from .dynamic import DynamicDefinition
from .prompt import PromptDefinition, MessageDef, AssetRefDef, RetryPolicyDef
from .reflector import ReflectorDefinition
from .conditional import ConditionalDefinition, ConditionDef
from .loop import LoopDefinition, LoopWhileDef, LoopEachDef

from .primes import REFLECTOR_PRIME_V1

EffectDef = Union[
    DynamicDefinition,
    PromptDefinition,
    ReflectorDefinition,
    ConditionalDefinition,
    LoopDefinition,
]


def _normalize_flow(flow: str) -> str:
    """Normalize flow aliases to canonical form."""
    flow = (flow or "chain").strip().lower()
    if flow in ("chain", "chain_of_thought", "cot"):
        return "chain"
    if flow in ("tree", "tree_of_thought", "tot"):
        return "tree"
    return "chain"


def compile_orchestration(
    *, orch: dict[str, Any], root_name: str = "prime"
) -> DynamicDefinition:
    # Support both 'effects' (spec) and 'steps' (legacy)
    effects = orch.get("effects") or orch.get("steps") or []
    if not isinstance(effects, list):
        raise ValueError("Orchestration 'effects' must be a list.")

    compiled_effects: list[EffectDef] = [
        _compile_effect(s) for s in effects if isinstance(s, dict)
    ]

    flow = _normalize_flow(orch.get("flow") or orch.get("strategy") or "chain")

    return DynamicDefinition(
        name=root_name,
        effects=compiled_effects,
        flow=flow,
    )


def _compile_effect(effect: dict[str, Any]) -> EffectDef:
    effect_type = (effect.get("type") or "").strip().lower()
    name = effect.get("name")

    if effect_type == "prompt":
        if not name:
            raise ValueError("Prompt effect is missing 'name'.")
        return _compile_prompt(effect)

    if effect_type == "dynamic":
        if not name:
            raise ValueError("Dynamic effect is missing 'name'.")

        flow = _normalize_flow(effect.get("flow") or effect.get("strategy") or "chain")

        # Support both 'effects' (spec) and 'steps' (legacy)
        child_effects = effect.get("effects") or effect.get("steps") or []
        if not isinstance(child_effects, list):
            raise ValueError(f"Dynamic '{name}' effects must be a list.")

        compiled_children = [
            _compile_effect(s) for s in child_effects if isinstance(s, dict)
        ]

        return DynamicDefinition(
            name=name,
            effects=compiled_children,
            flow=flow,
        )

    if effect_type in ("conditional", "if"):
        return _compile_conditional(effect)

    if effect_type == "loop":
        return _compile_loop(effect)

    if effect_type == "reflector":
        if not name:
            raise ValueError("Reflector effect is missing 'name'.")

        flow = _normalize_flow(effect.get("flow") or effect.get("strategy") or "chain")

        # Support both 'effects' (spec) and 'steps' (legacy)
        inner_effects = effect.get("effects") or effect.get("steps") or []
        if not isinstance(inner_effects, list):
            raise ValueError(f"Reflector '{name}' effects must be a list.")

        compiled_inner = [
            _compile_effect(s) for s in inner_effects if isinstance(s, dict)
        ]

        inner_dynamic = DynamicDefinition(
            name="inner",
            effects=compiled_inner,
            flow=flow,
        )

        plan_from_step = effect.get("plan_from_step") or "propose_steps"
        max_iterations = int(effect.get("max_iterations") or 1)
        generated_key = effect.get("generated_key") or "generated"
        stop_on_done = bool(effect.get("stop_on_done", True))

        prime_template = effect.get("prime_template")  # optional override
        # Support both 'max_effects' (spec) and 'max_steps' (legacy)
        max_effects = int(effect.get("max_effects") or effect.get("max_steps") or 8)

        return ReflectorDefinition(
            name=name,
            inner=inner_dynamic,
            plan_from_step=str(plan_from_step),
            max_iterations=max_iterations,
            generated_key=str(generated_key),
            stop_on_done=stop_on_done,
            prime_template=str(prime_template)
            if isinstance(prime_template, str)
            else REFLECTOR_PRIME_V1,
            max_effects=max_effects,
        )

    raise ValueError(f"Unsupported effect type: {effect_type!r}")


def _compile_conditional(effect: dict[str, Any]) -> ConditionalDefinition:
    """Compile a conditional (if/then/else) effect."""
    name = effect.get("name")  # Optional for conditionals

    # Parse the 'if' condition
    if_def = effect.get("if")
    if not isinstance(if_def, dict):
        raise ValueError(
            "Conditional must have an 'if' field with condition definition."
        )

    mode = (if_def.get("mode") or "model").strip().lower()
    if mode not in ("model", "cel"):
        mode = "model"

    condition = ConditionDef(
        mode=mode,
        template=if_def.get("template") if mode == "model" else None,
        expr=if_def.get("expr") if mode == "cel" else None,
    )

    # Parse 'then' branch (required)
    then_effects = effect.get("then") or []
    if not isinstance(then_effects, list):
        raise ValueError("Conditional 'then' must be a list of effects.")

    compiled_then = [_compile_effect(s) for s in then_effects if isinstance(s, dict)]

    # Parse 'else' branch (optional)
    else_effects = effect.get("else") or []
    if not isinstance(else_effects, list):
        raise ValueError("Conditional 'else' must be a list of effects.")

    compiled_else = [_compile_effect(s) for s in else_effects if isinstance(s, dict)]

    # Additional options
    threshold = float(effect.get("threshold") or 0.5)
    on_error = (effect.get("on_error") or "fail").strip().lower()
    if on_error not in ("fail", "continue", "skip"):
        on_error = "fail"

    return ConditionalDefinition(
        name=name,
        condition=condition,
        then_effects=tuple(compiled_then),
        else_effects=tuple(compiled_else),
        threshold=threshold,
        on_error=on_error,
    )


def _compile_loop(effect: dict[str, Any]) -> LoopDefinition:
    """Compile a loop (while/each) effect."""
    name = effect.get("name")  # Optional for loops

    # Parse body (required)
    body_effects = effect.get("body") or []
    if not isinstance(body_effects, list):
        raise ValueError("Loop 'body' must be a list of effects.")

    compiled_body = [_compile_effect(s) for s in body_effects if isinstance(s, dict)]

    # Determine loop mode: while or each
    while_def = None
    each_def = None

    if "while" in effect:
        while_config = effect.get("while")
        if isinstance(while_config, dict):
            mode = (while_config.get("mode") or "model").strip().lower()
            if mode not in ("model", "cel"):
                mode = "model"
            while_def = LoopWhileDef(
                mode=mode,
                template=while_config.get("template") if mode == "model" else None,
                expr=while_config.get("expr") if mode == "cel" else None,
            )

    if "each" in effect:
        each_config = effect.get("each")
        if isinstance(each_config, dict):
            in_path = each_config.get("in") or ""
            as_name = each_config.get("as") or "item"
            each_def = LoopEachDef(
                in_path=str(in_path),
                as_name=str(as_name),
            )

    # Iteration bounds
    max_iterations = int(effect.get("max_iterations") or 100)
    min_iterations = int(effect.get("min_iterations") or 0)

    # Error behavior
    on_error = (effect.get("on_error") or "fail").strip().lower()
    if on_error not in ("fail", "break", "continue"):
        on_error = "fail"

    return LoopDefinition(
        name=name,
        body=tuple(compiled_body),
        while_def=while_def,
        each_def=each_def,
        max_iterations=max_iterations,
        min_iterations=min_iterations,
        on_error=on_error,
    )


def _compile_prompt(effect: dict[str, Any]) -> PromptDefinition:
    """Compile a prompt effect with full spec support."""
    name = effect.get("name")
    if not name:
        raise ValueError("Prompt effect is missing 'name'.")

    # Primary input form: exactly one of template or messages
    template = effect.get("template")
    messages_raw = effect.get("messages")

    messages = None
    if messages_raw and isinstance(messages_raw, list):
        messages = tuple(
            MessageDef(
                role=m.get("role", "user"),
                content=m.get("content", ""),
            )
            for m in messages_raw
            if isinstance(m, dict)
        )

    # At least one input form must be provided
    if not template and not messages:
        raise ValueError(f"Prompt '{name}' must have 'template' or 'messages'.")

    # Prompt type
    prompt_type = (effect.get("prompt_type") or "text").strip().lower()
    if prompt_type not in (
        "text",
        "json",
        "boolean",
        "tool",
        "number",
        "array",
        "object",
    ):
        prompt_type = "text"

    # Schema for validation
    schema = effect.get("schema")
    if schema is not None and not isinstance(schema, dict):
        schema = None

    # Model configuration
    model = effect.get("model")
    provider = effect.get("provider")
    provider_fallbacks = effect.get("provider_fallbacks")
    if provider_fallbacks and not isinstance(provider_fallbacks, list):
        provider_fallbacks = None

    # Execution parameters
    params = effect.get("params")
    if params is not None and not isinstance(params, dict):
        params = None

    timeout_ms = effect.get("timeout_ms")
    if timeout_ms is not None:
        timeout_ms = int(timeout_ms)

    deterministic = bool(effect.get("deterministic", False))

    # Prompt-local inputs
    inputs = effect.get("inputs")
    if inputs is not None and not isinstance(inputs, dict):
        inputs = None

    # Assets
    assets_raw = effect.get("assets")
    assets = None
    if assets_raw and isinstance(assets_raw, list):
        assets = tuple(
            AssetRefDef(
                kind=a.get("kind", ""),
                ref=a.get("ref", ""),
            )
            for a in assets_raw
            if isinstance(a, dict)
        )

    # Retries
    retries_raw = effect.get("retries")
    retries = None
    if retries_raw and isinstance(retries_raw, dict):
        retries = RetryPolicyDef(
            max_attempts=int(retries_raw.get("max_attempts") or 1),
            backoff_ms=int(retries_raw.get("backoff_ms") or 1000),
        )

    # Error behavior
    on_error = (effect.get("on_error") or "fail").strip().lower()
    if on_error not in ("fail", "skip", "continue"):
        on_error = "fail"

    # Description
    description = effect.get("description")
    if description is not None and not isinstance(description, str):
        description = None

    return PromptDefinition(
        name=name,
        template=template,
        messages=messages,
        prompt_type=prompt_type,
        schema=schema,
        model=model,
        provider=provider,
        provider_fallbacks=tuple(provider_fallbacks) if provider_fallbacks else None,
        params=params,
        timeout_ms=timeout_ms,
        deterministic=deterministic,
        inputs=inputs,
        assets=assets,
        retries=retries,
        on_error=on_error,
        description=description,
    )
