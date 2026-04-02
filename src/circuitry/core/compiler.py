from __future__ import annotations

import re
from typing import Any, Literal, Union, cast

from .conditional import ConditionalDefinition, ConditionDef
from .dynamic import DynamicDefinition
from .loop import LoopDefinition, LoopEachDef, LoopWhileDef
from .primes import REFLECTOR_PRIME_V1
from .prompt import (
    AssetRefDef,
    MessageDef,
    PromptDefinition,
    PromptType,
    RetryPolicyDef,
)
from .reflector import ReflectorDefinition
from .tool import ToolDefinition
from .use import UseDefinition

EffectDef = Union[
    DynamicDefinition,
    PromptDefinition,
    ReflectorDefinition,
    ConditionalDefinition,
    LoopDefinition,
    ToolDefinition,
    UseDefinition,
]

_NAME_PATTERN = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


def _scope_child(scope_path: str, child_name: str) -> str:
    return f"{scope_path}.{child_name}" if scope_path else child_name


def _validate_name(
    *,
    name: Any,
    effect_type: str,
    scope_path: str,
    effect_path: str,
) -> str:
    if not isinstance(name, str):
        raise ValueError(
            f"{effect_type} effect at '{effect_path}' must define a string 'name'."
        )

    if name.strip() == "":
        raise ValueError(
            f"{effect_type} effect at '{effect_path}' has an empty/whitespace-only name."
        )

    if name != name.strip():
        raise ValueError(
            f"Invalid name '{name}' for {effect_type} at '{effect_path}': "
            "leading/trailing whitespace is not allowed."
        )

    if "." in name:
        raise ValueError(
            f"Invalid name '{name}' for {effect_type} at '{effect_path}': "
            "'.' is not allowed in names."
        )

    if any(ch.isspace() for ch in name):
        raise ValueError(
            f"Invalid name '{name}' for {effect_type} at '{effect_path}': "
            "whitespace is not allowed in names."
        )

    if re.fullmatch(r"iter_\d+", name):
        raise ValueError(
            f"Invalid name '{name}' for {effect_type} at '{effect_path}': "
            "reserved loop iteration segment pattern 'iter_<n>' is not allowed."
        )

    if not _NAME_PATTERN.fullmatch(name):
        raise ValueError(
            f"Invalid name '{name}' for {effect_type} at '{effect_path}': "
            "expected pattern [A-Za-z_][A-Za-z0-9_]*."
        )

    # `scope_path` is included so callers can report deterministic addressing context.
    _ = scope_path
    return name


def _compile_effects_in_scope(
    *,
    effects: Any,
    scope_path: str,
    container_path: str,
) -> list[EffectDef]:
    if not isinstance(effects, list):
        raise ValueError(f"{container_path} must be a list of effects.")

    compiled: list[EffectDef] = []
    seen_names: dict[str, str] = {}

    for idx, effect in enumerate(effects):
        effect_path = f"{container_path}[{idx}]"
        if not isinstance(effect, dict):
            raise ValueError(
                f"Effect at '{effect_path}' must be an object/mapping, "
                f"got {type(effect).__name__}."
            )

        effect_type = (effect.get("type") or "").strip().lower()
        if not effect_type:
            raise ValueError(
                f"Effect at '{effect_path}' is missing required field 'type'."
            )

        raw_name = effect.get("name")
        if raw_name is not None:
            valid_name = _validate_name(
                name=raw_name,
                effect_type=effect_type,
                scope_path=scope_path,
                effect_path=effect_path,
            )
            if valid_name in seen_names:
                raise ValueError(
                    f"Duplicate effect name '{valid_name}' in scope '{scope_path}'. "
                    f"Seen at '{seen_names[valid_name]}' and '{effect_path}'."
                )
            seen_names[valid_name] = effect_path

        compiled.append(
            _compile_effect(
                effect,
                scope_path=scope_path,
                effect_path=effect_path,
            )
        )

    return compiled


_VALID_FLOWS: dict[str, Literal["chain", "tree"]] = {
    "chain": "chain", "chain_of_thought": "chain", "cot": "chain",
    "tree": "tree", "tree_of_thought": "tree", "tot": "tree",
}


def _normalize_flow(flow: str) -> Literal["chain", "tree"]:
    """Normalize flow aliases to canonical form."""
    key = (flow or "chain").strip().lower()
    canonical = _VALID_FLOWS.get(key)
    if canonical is not None:
        return canonical
    valid = ", ".join(sorted(_VALID_FLOWS.keys()))
    raise ValueError(f"Unknown flow value {flow!r}. Valid values are: {valid}.")


def compile_orchestration(
    *, orch: dict[str, Any], root_name: str = "prime"
) -> DynamicDefinition:
    # Support both 'effects' (spec) and 'steps' (legacy)
    effects = orch.get("effects") or orch.get("steps") or []
    compiled_effects = _compile_effects_in_scope(
        effects=effects,
        scope_path=root_name,
        container_path=f"{root_name}.effects",
    )

    flow = _normalize_flow(orch.get("flow") or orch.get("strategy") or "chain")

    return DynamicDefinition(
        name=root_name,
        effects=compiled_effects,
        flow=flow,
    )


def _compile_effect(
    effect: dict[str, Any], *, scope_path: str, effect_path: str
) -> EffectDef:
    effect_type = (effect.get("type") or "").strip().lower()
    name = effect.get("name")

    if effect_type == "prompt":
        if name is None:
            raise ValueError(
                f"Prompt effect at '{effect_path}' is missing required field 'name'."
            )
        _validate_name(
            name=name,
            effect_type="prompt",
            scope_path=scope_path,
            effect_path=effect_path,
        )
        return _compile_prompt(effect)

    if effect_type == "dynamic":
        if name is None:
            raise ValueError(
                f"Dynamic effect at '{effect_path}' is missing required field 'name'."
            )
        valid_name = _validate_name(
            name=name,
            effect_type="dynamic",
            scope_path=scope_path,
            effect_path=effect_path,
        )

        flow = _normalize_flow(effect.get("flow") or effect.get("strategy") or "chain")

        # Support both 'effects' (spec) and 'steps' (legacy)
        child_effects = effect.get("effects") or effect.get("steps") or []
        child_scope = _scope_child(scope_path, valid_name)
        compiled_children = _compile_effects_in_scope(
            effects=child_effects,
            scope_path=child_scope,
            container_path=f"{effect_path}.effects",
        )

        return DynamicDefinition(
            name=valid_name,
            effects=compiled_children,
            flow=flow,
        )

    if effect_type in ("conditional", "if"):
        return _compile_conditional(
            effect, scope_path=scope_path, effect_path=effect_path
        )

    if effect_type == "loop":
        return _compile_loop(effect, scope_path=scope_path, effect_path=effect_path)

    if effect_type == "tool":
        if name is None:
            raise ValueError(
                f"Tool effect at '{effect_path}' is missing required field 'name'."
            )
        _validate_name(
            name=name,
            effect_type="tool",
            scope_path=scope_path,
            effect_path=effect_path,
        )
        return _compile_tool(effect, scope_path=scope_path, effect_path=effect_path)

    if effect_type == "use":
        if name is None:
            raise ValueError(
                f"Use effect at '{effect_path}' is missing required field 'name'."
            )
        _validate_name(
            name=name,
            effect_type="use",
            scope_path=scope_path,
            effect_path=effect_path,
        )
        return _compile_use(effect, scope_path=scope_path, effect_path=effect_path)

    if effect_type == "reflector":
        if name is None:
            raise ValueError(
                f"Reflector effect at '{effect_path}' is missing required field 'name'."
            )
        valid_name = _validate_name(
            name=name,
            effect_type="reflector",
            scope_path=scope_path,
            effect_path=effect_path,
        )

        flow = _normalize_flow(effect.get("flow") or effect.get("strategy") or "chain")

        # Support both 'effects' (spec) and 'steps' (legacy)
        inner_effects = effect.get("effects") or effect.get("steps") or []
        inner_scope = _scope_child(scope_path, valid_name)
        compiled_inner = _compile_effects_in_scope(
            effects=inner_effects,
            scope_path=inner_scope,
            container_path=f"{effect_path}.effects",
        )

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
            name=valid_name,
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

    raise ValueError(f"Unsupported effect type at '{effect_path}': {effect_type!r}")


def _compile_conditional(
    effect: dict[str, Any], *, scope_path: str, effect_path: str
) -> ConditionalDefinition:
    """Compile a conditional (if/then/else) effect."""
    name = effect.get("name")  # Optional for conditionals
    validated_name: str | None = None
    if name is not None:
        validated_name = _validate_name(
            name=name,
            effect_type="conditional",
            scope_path=scope_path,
            effect_path=effect_path,
        )

    # Parse the 'if' condition
    if_def = effect.get("if")
    if not isinstance(if_def, dict):
        raise ValueError(
            f"Conditional at '{effect_path}' must have an 'if' field with "
            "condition definition."
        )

    mode_raw = str(if_def.get("mode") or "model").strip().lower()
    mode: Literal["model", "cel"] = "cel" if mode_raw == "cel" else "model"

    if mode == "model" and not if_def.get("template"):
        raise ValueError(
            f"Conditional at '{effect_path}': mode 'model' requires a 'template' field."
        )
    if mode == "cel" and not if_def.get("expr"):
        raise ValueError(
            f"Conditional at '{effect_path}': mode 'cel' requires an 'expr' field."
        )

    condition = ConditionDef(
        mode=mode,
        template=if_def.get("template") if mode == "model" else None,
        expr=if_def.get("expr") if mode == "cel" else None,
    )

    # Parse 'then' branch (required)
    then_effects = effect.get("then") or []
    branch_scope = (
        _scope_child(scope_path, validated_name) if validated_name else scope_path
    )
    compiled_then = _compile_effects_in_scope(
        effects=then_effects,
        scope_path=branch_scope,
        container_path=f"{effect_path}.then",
    )

    # Parse 'else' branch (optional)
    else_effects = effect.get("else") or []
    compiled_else = _compile_effects_in_scope(
        effects=else_effects,
        scope_path=branch_scope,
        container_path=f"{effect_path}.else",
    )

    # Additional options
    threshold = float(effect.get("threshold") or 0.5)
    on_error_raw = str(effect.get("on_error") or "fail").strip().lower()
    on_error: Literal["fail", "continue", "skip"] = (
        cast(Literal["fail", "continue", "skip"], on_error_raw)
        if on_error_raw in ("fail", "continue", "skip")
        else "fail"
    )

    return ConditionalDefinition(
        name=validated_name,
        condition=condition,
        then_effects=tuple(compiled_then),
        else_effects=tuple(compiled_else),
        threshold=threshold,
        on_error=on_error,
    )


def _compile_loop(
    effect: dict[str, Any], *, scope_path: str, effect_path: str
) -> LoopDefinition:
    """Compile a loop (while/each) effect."""
    name = effect.get("name")  # Optional for loops
    validated_name: str | None = None
    if name is not None:
        validated_name = _validate_name(
            name=name,
            effect_type="loop",
            scope_path=scope_path,
            effect_path=effect_path,
        )

    # Parse body (required)
    body_effects = effect.get("body") or []
    body_scope = (
        _scope_child(scope_path, validated_name) if validated_name else scope_path
    )
    compiled_body = _compile_effects_in_scope(
        effects=body_effects,
        scope_path=body_scope,
        container_path=f"{effect_path}.body",
    )

    # Determine loop mode: while or each
    while_def = None
    each_def = None

    if "while" in effect:
        while_config = effect.get("while")
        if isinstance(while_config, dict):
            mode_raw = str(while_config.get("mode") or "model").strip().lower()
            mode: Literal["model", "cel"] = "cel" if mode_raw == "cel" else "model"
            if mode == "model" and not while_config.get("template"):
                raise ValueError(
                    f"Loop while at '{effect_path}': mode 'model' requires a 'template' field."
                )
            if mode == "cel" and not while_config.get("expr"):
                raise ValueError(
                    f"Loop while at '{effect_path}': mode 'cel' requires an 'expr' field."
                )
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
    on_error_raw = str(effect.get("on_error") or "fail").strip().lower()
    on_error: Literal["fail", "break", "continue"] = (
        cast(Literal["fail", "break", "continue"], on_error_raw)
        if on_error_raw in ("fail", "break", "continue")
        else "fail"
    )

    # Collection output: name of the body effect whose .value to aggregate
    collect_raw = effect.get("collect")
    collect: str | None = str(collect_raw).strip() if collect_raw is not None else None

    # Execution topology for each-loops
    flow = _normalize_flow(effect.get("flow") or "chain")

    # Max parallel workers (only meaningful when flow="tree")
    max_concurrency_raw = effect.get("max_concurrency")
    max_concurrency: int | None = (
        int(max_concurrency_raw) if max_concurrency_raw is not None else None
    )

    return LoopDefinition(
        name=validated_name,
        body=tuple(compiled_body),
        while_def=while_def,
        each_def=each_def,
        max_iterations=max_iterations,
        min_iterations=min_iterations,
        on_error=on_error,
        collect=collect,
        flow=flow,
        max_concurrency=max_concurrency,
    )


def _compile_tool(
    effect: dict[str, Any], *, scope_path: str, effect_path: str
) -> ToolDefinition:
    """Compile a tool effect."""
    name = effect.get("name")
    if not name:
        raise ValueError(f"Tool effect at '{effect_path}' is missing 'name'.")

    provider = effect.get("provider")
    if not isinstance(provider, str) or not provider.strip():
        raise ValueError(
            f"Tool effect '{name}' at '{effect_path}' is missing required field 'provider'."
        )

    params = effect.get("params") or {}
    if not isinstance(params, dict):
        params = {}

    prompt = effect.get("prompt")
    if prompt is not None and not isinstance(prompt, str):
        prompt = None

    model = effect.get("model")
    if model is not None and not isinstance(model, str):
        model = None

    timeout_ms = effect.get("timeout_ms")
    if timeout_ms is not None:
        timeout_ms = int(timeout_ms)

    on_error_raw = str(effect.get("on_error") or "fail").strip().lower()
    on_error: Literal["fail", "skip", "continue"] = (
        cast(Literal["fail", "skip", "continue"], on_error_raw)
        if on_error_raw in ("fail", "skip", "continue")
        else "fail"
    )

    description = effect.get("description")
    if description is not None and not isinstance(description, str):
        description = None

    _ = scope_path  # used by caller for deterministic addressing context
    return ToolDefinition(
        name=name,
        provider=provider.strip(),
        params=params,
        prompt=prompt,
        model=model,
        timeout_ms=timeout_ms,
        on_error=on_error,
        description=description,
    )


def _compile_use(
    effect: dict[str, Any], *, scope_path: str, effect_path: str
) -> UseDefinition:
    """Compile a use (sub-orchestration) effect."""
    name = effect.get("name")
    if not name:
        raise ValueError(f"Use effect at '{effect_path}' is missing 'name'.")

    orchestration = effect.get("orchestration")
    if not isinstance(orchestration, str) or not orchestration.strip():
        raise ValueError(
            f"Use effect '{name}' at '{effect_path}' is missing required field 'orchestration'."
        )

    inputs = effect.get("inputs") or None
    if inputs is not None and not isinstance(inputs, dict):
        inputs = None

    outputs = effect.get("outputs") or None
    if outputs is not None and not isinstance(outputs, dict):
        outputs = None

    on_error_raw = str(effect.get("on_error") or "fail").strip().lower()
    on_error: Literal["fail", "skip", "continue"] = (
        cast(Literal["fail", "skip", "continue"], on_error_raw)
        if on_error_raw in ("fail", "skip", "continue")
        else "fail"
    )

    description = effect.get("description")
    if description is not None and not isinstance(description, str):
        description = None

    _ = scope_path
    return UseDefinition(
        name=name,
        orchestration=orchestration.strip(),
        inputs=inputs,
        outputs=outputs,
        on_error=on_error,
        description=description,
    )


def _compile_prompt(effect: dict[str, Any]) -> PromptDefinition:
    """Compile a prompt effect with full spec support."""
    name = effect.get("name")
    if not name:
        raise ValueError("Prompt effect is missing 'name'.")

    # Prompt type (read early — affects input form requirements)
    prompt_type_raw = str(effect.get("prompt_type") or "text").strip().lower()

    if prompt_type_raw == "image":
        raise ValueError(
            f"Prompt '{name}': prompt_type 'image' is no longer supported. "
            "Use a tool effect with provider: comfyui instead. "
            "See the orchestration reference for migration instructions."
        )

    # Primary input form: template or messages
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

    if not template and not messages:
        raise ValueError(f"Prompt '{name}' must have 'template' or 'messages'.")
    if prompt_type_raw not in (
        "text",
        "json",
        "boolean",
        "tool",
        "number",
        "array",
        "object",
    ):
        prompt_type_raw = "text"
    prompt_type: PromptType = cast(PromptType, prompt_type_raw)

    # Schema for validation
    schema = effect.get("schema")
    if schema is not None and not isinstance(schema, dict):
        schema = None

    if prompt_type in ("json", "object", "array") and schema is None:
        raise ValueError(
            f"Prompt '{name}': prompt_type '{prompt_type}' requires a 'schema' field."
        )

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
    on_error_raw = str(effect.get("on_error") or "fail").strip().lower()
    on_error: Literal["fail", "skip", "continue"] = (
        cast(Literal["fail", "skip", "continue"], on_error_raw)
        if on_error_raw in ("fail", "skip", "continue")
        else "fail"
    )

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
