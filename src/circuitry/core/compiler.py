from __future__ import annotations

import re
from dataclasses import replace
from typing import Any, Literal, cast

from .cel_eval import validate_cel_syntax
from .conditional import ConditionalDefinition, ConditionDef
from .dynamic import DynamicDefinition
from .expect import ExpectDef
from .loop import LoopDefinition, LoopEachDef, LoopWhileDef
from .outputs import normalize_outputs
from .primes import REFLECTOR_PRIME_V1
from .prompt import (
    AssetRefDef,
    MessageDef,
    PromptDefinition,
    PromptType,
    RetryPolicyDef,
)
from .reflector import ReflectorDefinition
from .state_ns import (
    validate_bare_input_refs,
    validate_cel_expr,
    validate_each_in_path,
    validate_reference_path,
)
from .templates import template_syntax_error
from .tool import _SECURITY_SENSITIVE_PARAM_KEYS, ToolDefinition, param_reference
from .use import UseDefinition, reference_path

EffectDef = (
    DynamicDefinition
    | PromptDefinition
    | ReflectorDefinition
    | ConditionalDefinition
    | LoopDefinition
    | ToolDefinition
    | UseDefinition
)

_NAME_PATTERN = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")

#: Collide with structural slots the runtime itself writes onto an effect's
#: own node (``value``, ``meta``) or merges into the context (``input``,
#: ``prime``, ``runtime``) — see issue #260 part 3.
_RESERVED_EFFECT_NAMES = frozenset({"value", "meta", "input", "prime", "runtime"})


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

    if name in _RESERVED_EFFECT_NAMES:
        raise ValueError(
            f"Invalid name '{name}' for {effect_type} at '{effect_path}': "
            f"'{name}' is reserved — it collides with a structural slot the "
            "runtime itself writes ('value', 'meta', 'input', 'prime', "
            "'runtime'). Choose a different name."
        )

    if not _NAME_PATTERN.fullmatch(name):
        raise ValueError(
            f"Invalid name '{name}' for {effect_type} at '{effect_path}': "
            "expected pattern [A-Za-z_][A-Za-z0-9_]*."
        )

    # `scope_path` is included so callers can report deterministic addressing context.
    _ = scope_path
    return name


def _check_templates(value: Any, *, effect_path: str, field: str) -> None:
    """Reject a malformed Mustache template (in *value*, walked recursively).

    Every string a tool's ``params`` holds is rendered, so nested mappings and
    lists are walked; non-string leaves are not templates.
    """
    if isinstance(value, str):
        reason = template_syntax_error(value)
        if reason is not None:
            raise ValueError(
                f"{effect_path}.{field}: malformed Mustache template: {reason}"
            )
    elif isinstance(value, dict):
        for key, item in value.items():
            _check_templates(item, effect_path=effect_path, field=f"{field}.{key}")
    elif isinstance(value, list):
        for index, item in enumerate(value):
            _check_templates(item, effect_path=effect_path, field=f"{field}[{index}]")


def _compile_retries(effect: dict[str, Any]) -> RetryPolicyDef | None:
    """``retries:`` on a ``tool``/``use`` effect — same shape and defaults as
    a prompt's (#273)."""
    retries_raw = effect.get("retries")
    if not retries_raw or not isinstance(retries_raw, dict):
        return None
    return RetryPolicyDef(
        max_attempts=int(retries_raw.get("max_attempts") or 1),
        backoff_ms=int(retries_raw.get("backoff_ms") or 1000),
    )


def _compile_expect(
    effect: dict[str, Any],
    *,
    effect_type: str,
    effect_path: str,
    loop_names: frozenset[str] = frozenset(),
) -> ExpectDef | None:
    """``expect:`` on a ``tool``/``use`` effect (#273).

    A bare string is shorthand for ``{mode: cel, expr: <string>}``. A CEL
    expression here is over this effect's own ``value``/``meta`` plus
    ``state`` — the three-binding convention ``cel_eval.evaluate_cel_expect``
    documents — so ``validate_cel_expr``'s ``state.<key>`` namespace check
    still applies (an expression that never mentions ``state.`` passes it
    trivially) while the syntax check (``validate_cel_syntax``) always does.
    """
    raw = effect.get("expect")
    if raw is None:
        return None
    if isinstance(raw, str):
        expr = raw.strip()
        if not expr:
            raise ValueError(
                f"{effect_type} effect at '{effect_path}': 'expect' must not be "
                "an empty string."
            )
        validate_cel_expr(expr, effect_path=effect_path, extra_names=loop_names)
        validate_cel_syntax(expr, effect_path=effect_path, label="expect CEL expression")
        return ExpectDef(mode="cel", expr=expr)
    if isinstance(raw, dict):
        mode_raw = str(raw.get("mode") or "cel").strip().lower()
        mode: Literal["cel", "model"] = "model" if mode_raw == "model" else "cel"
        if mode == "cel":
            expr = raw.get("expr")
            if not isinstance(expr, str) or not expr.strip():
                raise ValueError(
                    f"{effect_type} effect at '{effect_path}': expect mode 'cel' "
                    "requires a non-empty 'expr' field."
                )
            expr = expr.strip()
            validate_cel_expr(expr, effect_path=effect_path, extra_names=loop_names)
            validate_cel_syntax(expr, effect_path=effect_path, label="expect CEL expression")
            return ExpectDef(mode="cel", expr=expr)
        template = raw.get("template")
        if not isinstance(template, str) or not template.strip():
            raise ValueError(
                f"{effect_type} effect at '{effect_path}': expect mode 'model' "
                "requires a non-empty 'template' field."
            )
        _check_templates(template, effect_path=effect_path, field="expect.template")
        return ExpectDef(mode="model", template=template)
    raise ValueError(
        f"{effect_type} effect at '{effect_path}': 'expect' must be a CEL string "
        "or a mapping with 'mode'."
    )


def _tree_loop_prev_references(effects: Any, name: str) -> bool:
    """Whether any string anywhere under *effects* references ``prime.<name>.prev``.

    Walked over the raw (pre-compile) effect dicts, the same way lint's
    ``_reference_strings`` does — every template, CEL expression, tool param
    and ``use`` input is just a string carrying state paths, so none of them
    can be told apart usefully here.
    """
    pattern = re.compile(rf"\bprime\.{re.escape(name)}\.prev\b")

    def scan(value: Any) -> bool:
        if isinstance(value, str):
            return bool(pattern.search(value))
        if isinstance(value, dict):
            return any(scan(v) for v in value.values())
        if isinstance(value, list):
            return any(scan(v) for v in value)
        return False

    return scan(effects)


def _compile_effects_in_scope(
    *,
    effects: Any,
    scope_path: str,
    container_path: str,
    loop_names: frozenset[str] = frozenset(),
    # Shared with a sibling call compiling the same scope's 'finally' (or
    # body) list, so a finally effect reusing a body effect's name is
    # caught as the same duplicate-name error a body/body collision is —
    # both land on the same state node (core.dynamic runs 'finally' against
    # the same child_store as the body, see _compile_effect's own comment).
    seen_names: dict[str, str] | None = None,
) -> list[EffectDef]:
    if not isinstance(effects, list):
        raise ValueError(f"{container_path} must be a list of effects.")

    compiled: list[EffectDef] = []
    if seen_names is None:
        seen_names = {}

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
                loop_names=loop_names,
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
    # Bare {{name}} refs to this document's declared interface inputs are a
    # hard error — caller inputs live under the `input` namespace.
    validate_bare_input_refs(orch)
    root_seen_names: dict[str, str] = {}
    compiled_effects = _compile_effects_in_scope(
        effects=effects,
        scope_path=root_name,
        container_path=f"{root_name}.effects",
        seen_names=root_seen_names,
    )

    # Cleanup effects, allowed at the document root as well as on a
    # `dynamic` effect (see `_compile_effect`'s `finally` guard) — same
    # scope as the main effects, so e.g. a started server's pid is in reach.
    finally_raw = orch.get("finally") or []
    compiled_finally = (
        _compile_effects_in_scope(
            effects=finally_raw,
            scope_path=root_name,
            container_path=f"{root_name}.finally",
            seen_names=root_seen_names,
        )
        if finally_raw
        else []
    )

    flow = _normalize_flow(orch.get("flow") or orch.get("strategy") or "chain")

    return DynamicDefinition(
        name=root_name,
        effects=compiled_effects,
        flow=flow,
        finally_effects=tuple(compiled_finally),
    )


def apply_effect_overrides(
    root: DynamicDefinition, overrides: dict[str, dict[str, Any]]
) -> tuple[DynamicDefinition, set[str]]:
    """Apply a profile's per-effect model/provider/enabled/routing overlay onto a tree.

    Rebuilds the immutable ``*Definition`` chain bottom-up via
    ``dataclasses.replace`` so frozen semantics are preserved — this never
    mutates ``root`` or any of its descendants. Dotted override keys use the
    same addressing as runtime state paths (relative to the ``prime`` root),
    matching ``profiles.collect_orchestration_effect_paths``.

    ``enabled: false`` marks the matched effect *and every descendant* as
    disabled, so an inspector of the compiled tree sees the same truth the
    runtime acts on (a disabled container never executes its subtree — see
    ``core.disabled``).

    Returns the rebuilt root plus the set of override keys that matched an
    effect somewhere in the tree.
    """
    if not overrides:
        return root, set()

    matched: set[str] = set()
    new_children = [
        _overlay_effect(child, path="", overrides=overrides, matched=matched)
        for child in root.effects
    ]
    new_finally = [
        _overlay_effect(child, path="", overrides=overrides, matched=matched)
        for child in root.finally_effects
    ]
    return replace(root, effects=new_children, finally_effects=new_finally), matched


def _overlay_effect(
    node: EffectDef,
    *,
    path: str,
    overrides: dict[str, dict[str, Any]],
    matched: set[str],
) -> EffectDef:
    name = getattr(node, "name", None)
    own_path = _scope_child(path, name) if name else path

    override = overrides.get(own_path) if own_path else None
    disable = False
    if override:
        matched.add(own_path)
        field_overrides: dict[str, Any] = {}
        if "model" in override and hasattr(node, "model"):
            field_overrides["model"] = override["model"]
        if "provider" in override and hasattr(node, "provider"):
            field_overrides["provider"] = override["provider"]
        if "routing" in override and hasattr(node, "routing_override"):
            field_overrides["routing_override"] = override["routing"]
        if field_overrides:
            node = replace(node, **field_overrides)
        disable = override.get("enabled") is False

    if isinstance(node, DynamicDefinition):
        node = replace(
            node,
            effects=[
                _overlay_effect(c, path=own_path, overrides=overrides, matched=matched)
                for c in node.effects
            ],
            # 'finally' shares its dynamic's own scope (same child_scope the
            # compiler gives its body, not a 'finally' segment of its own —
            # see _compile_effect) so a profile addresses a cleanup effect
            # exactly like a body one.
            finally_effects=[
                _overlay_effect(c, path=own_path, overrides=overrides, matched=matched)
                for c in node.finally_effects
            ],
        )
    elif isinstance(node, ReflectorDefinition):
        node = replace(
            node,
            inner=replace(
                node.inner,
                effects=[
                    _overlay_effect(
                        c, path=own_path, overrides=overrides, matched=matched
                    )
                    for c in node.inner.effects
                ],
            ),
        )
    elif isinstance(node, ConditionalDefinition):
        node = replace(
            node,
            then_effects=tuple(
                _overlay_effect(c, path=own_path, overrides=overrides, matched=matched)
                for c in node.then_effects
            ),
            else_effects=tuple(
                _overlay_effect(c, path=own_path, overrides=overrides, matched=matched)
                for c in node.else_effects
            ),
        )
    elif isinstance(node, LoopDefinition):
        node = replace(
            node,
            body=tuple(
                _overlay_effect(c, path=own_path, overrides=overrides, matched=matched)
                for c in node.body
            ),
        )

    if disable:
        node = _disable_subtree(node)

    return node


def _disable_subtree(node: EffectDef) -> EffectDef:
    """Return *node* (and every descendant) rebuilt with ``enabled=False``."""
    node = replace(node, enabled=False)

    if isinstance(node, DynamicDefinition):
        return replace(
            node,
            effects=[_disable_subtree(c) for c in node.effects],
            finally_effects=[_disable_subtree(c) for c in node.finally_effects],
        )
    if isinstance(node, ReflectorDefinition):
        inner = cast(DynamicDefinition, _disable_subtree(node.inner))
        return replace(node, inner=inner)
    if isinstance(node, ConditionalDefinition):
        return replace(
            node,
            then_effects=tuple(_disable_subtree(c) for c in node.then_effects),
            else_effects=tuple(_disable_subtree(c) for c in node.else_effects),
        )
    if isinstance(node, LoopDefinition):
        return replace(node, body=tuple(_disable_subtree(c) for c in node.body))
    return node


def collect_effect_groups(node: EffectDef) -> set[str]:
    """Every ``group:`` name set on a tool/prompt effect anywhere under *node*.

    Does not descend into a ``use`` effect's child — that document isn't
    compiled yet when its parent is; its own ``group:`` fields are checked
    against the same known groups when it loads (see ``core.use``).
    """
    groups: set[str] = set()
    if isinstance(node, (ToolDefinition, PromptDefinition)):
        if node.group is not None:
            groups.add(node.group)
    elif isinstance(node, DynamicDefinition):
        for child in (*node.effects, *node.finally_effects):
            groups |= collect_effect_groups(child)
    elif isinstance(node, ReflectorDefinition):
        groups |= collect_effect_groups(node.inner)
    elif isinstance(node, ConditionalDefinition):
        for child in (*node.then_effects, *node.else_effects):
            groups |= collect_effect_groups(child)
    elif isinstance(node, LoopDefinition):
        for child in node.body:
            groups |= collect_effect_groups(child)
    return groups


def unknown_concurrency_group_errors(
    root: EffectDef, known_groups: frozenset[str]
) -> list[str]:
    """One error per ``group:`` name under *root* that ``known_groups`` —
    ``runtime.concurrency_groups``'s own keys — doesn't define (#274)."""
    unknown = sorted(collect_effect_groups(root) - known_groups)
    if not unknown:
        return []
    known_desc = ", ".join(sorted(known_groups)) if known_groups else "(none configured)"
    return [
        f"group {name!r} is not defined in runtime.concurrency_groups — "
        f"known groups: {known_desc}."
        for name in unknown
    ]


def _compile_effect(
    effect: dict[str, Any],
    *,
    scope_path: str,
    effect_path: str,
    loop_names: frozenset[str] = frozenset(),
) -> EffectDef:
    effect_type = (effect.get("type") or "").strip().lower()
    name = effect.get("name")

    if effect_type != "dynamic" and "finally" in effect:
        raise ValueError(
            f"{effect_type or 'unknown'} effect at '{effect_path}': 'finally' is "
            "only allowed on a 'dynamic' effect or the document root."
        )

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
        return _compile_prompt(effect, effect_path=effect_path)

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
        dynamic_seen_names: dict[str, str] = {}
        compiled_children = _compile_effects_in_scope(
            effects=child_effects,
            scope_path=child_scope,
            container_path=f"{effect_path}.effects",
            loop_names=loop_names,
            seen_names=dynamic_seen_names,
        )

        # Cleanup effects — same scope as this dynamic's own children, so
        # they see state as it stands (e.g. a started server's pid). Shares
        # dynamic_seen_names with the body above so a finally effect reusing
        # a body effect's name is caught as a duplicate, not a silent
        # same-node overwrite at runtime.
        finally_raw = effect.get("finally") or []
        compiled_finally = (
            _compile_effects_in_scope(
                effects=finally_raw,
                scope_path=child_scope,
                container_path=f"{effect_path}.finally",
                loop_names=loop_names,
                seen_names=dynamic_seen_names,
            )
            if finally_raw
            else []
        )

        # Max parallel workers (only meaningful when flow="tree")
        max_concurrency_raw = effect.get("max_concurrency")
        max_concurrency: int | None = (
            int(max_concurrency_raw) if max_concurrency_raw is not None else None
        )

        stop_on_error = bool(effect.get("stop_on_error", False))

        dyn_on_error_raw = str(effect.get("on_error") or "fail").strip().lower()
        dyn_on_error: Literal["fail", "skip", "continue"] = (
            cast(Literal["fail", "skip", "continue"], dyn_on_error_raw)
            if dyn_on_error_raw in ("fail", "skip", "continue")
            else "fail"
        )

        labels_raw = effect.get("labels")
        labels = dict(labels_raw) if isinstance(labels_raw, dict) else None

        return DynamicDefinition(
            name=valid_name,
            effects=compiled_children,
            flow=flow,
            max_concurrency=max_concurrency,
            stop_on_error=stop_on_error,
            on_error=dyn_on_error,
            labels=labels,
            finally_effects=tuple(compiled_finally),
        )

    if effect_type in ("conditional", "if"):
        return _compile_conditional(
            effect, scope_path=scope_path, effect_path=effect_path, loop_names=loop_names
        )

    if effect_type == "loop":
        return _compile_loop(
            effect, scope_path=scope_path, effect_path=effect_path, loop_names=loop_names
        )

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
        return _compile_tool(
            effect, scope_path=scope_path, effect_path=effect_path, loop_names=loop_names
        )

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
        return _compile_use(
            effect,
            scope_path=scope_path,
            effect_path=effect_path,
            loop_names=loop_names,
        )

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
            loop_names=loop_names,
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
    effect: dict[str, Any],
    *,
    scope_path: str,
    effect_path: str,
    loop_names: frozenset[str] = frozenset(),
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
    if mode == "model":
        _check_templates(
            if_def.get("template"), effect_path=effect_path, field="if.template"
        )
    if mode == "cel" and not if_def.get("expr"):
        raise ValueError(
            f"Conditional at '{effect_path}': mode 'cel' requires an 'expr' field."
        )
    if mode == "cel":
        expr = str(if_def.get("expr") or "")
        validate_cel_expr(expr, effect_path=effect_path, extra_names=loop_names)
        validate_cel_syntax(
            expr, effect_path=effect_path, effect_name=validated_name
        )

    condition = ConditionDef(
        mode=mode,
        template=if_def.get("template") if mode == "model" else None,
        expr=if_def.get("expr") if mode == "cel" else None,
        strict=bool(if_def.get("strict")) if mode == "cel" else False,
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
        loop_names=loop_names,
    )

    # Parse 'else' branch (optional)
    else_effects = effect.get("else") or []
    compiled_else = _compile_effects_in_scope(
        effects=else_effects,
        scope_path=branch_scope,
        container_path=f"{effect_path}.else",
        loop_names=loop_names,
    )

    # Additional options
    threshold = float(effect.get("threshold") or 0.5)
    on_error_raw = str(effect.get("on_error") or "fail").strip().lower()
    on_error: Literal["fail", "continue", "skip"] = (
        cast(Literal["fail", "continue", "skip"], on_error_raw)
        if on_error_raw in ("fail", "continue", "skip")
        else "fail"
    )

    labels_raw = effect.get("labels")
    labels = dict(labels_raw) if isinstance(labels_raw, dict) else None

    return ConditionalDefinition(
        name=validated_name,
        condition=condition,
        then_effects=tuple(compiled_then),
        else_effects=tuple(compiled_else),
        threshold=threshold,
        on_error=on_error,
        labels=labels,
    )


def _compile_loop(
    effect: dict[str, Any],
    *,
    scope_path: str,
    effect_path: str,
    loop_names: frozenset[str] = frozenset(),
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

    # Exactly one mode. With neither — say a misspelled `whlie:` — the loop
    # would run zero passes and still report a clean termination.
    modes = [key for key in ("while", "each") if key in effect]
    if len(modes) != 1:
        found = "both" if modes else "neither"
        raise ValueError(
            f"Loop at '{effect_path}' must set exactly one of 'while' or 'each' "
            f"(found {found})."
        )
    if not isinstance(effect[modes[0]], dict):
        raise ValueError(
            f"Loop at '{effect_path}': '{modes[0]}' must be a mapping."
        )

    # This loop's own CEL-visible bindings, added to whatever an enclosing
    # loop already contributed, so a nested loop's body sees both — the
    # inner loop's `iter`/`as` shadow the outer's at runtime (see loop.py),
    # and both are legal `state.<key>` names for CEL at this nesting depth.
    body_loop_names = loop_names
    if "while" in effect or "each" in effect:
        body_loop_names = body_loop_names | {"iter"}
    each_as_name: str | None = None
    if "each" in effect:
        each_config = effect.get("each")
        if isinstance(each_config, dict):
            each_as_name = str(each_config.get("as") or "item")
            body_loop_names = body_loop_names | {each_as_name}

    # Parse body (required)
    body_effects = effect.get("body") or []
    body_scope = (
        _scope_child(scope_path, validated_name) if validated_name else scope_path
    )
    compiled_body = _compile_effects_in_scope(
        effects=body_effects,
        scope_path=body_scope,
        container_path=f"{effect_path}.body",
        loop_names=body_loop_names,
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
            if mode == "model":
                _check_templates(
                    while_config.get("template"),
                    effect_path=effect_path,
                    field="while.template",
                )
            if mode == "cel" and not while_config.get("expr"):
                raise ValueError(
                    f"Loop while at '{effect_path}': mode 'cel' requires an 'expr' field."
                )
            if mode == "cel":
                while_expr = str(while_config.get("expr") or "")
                validate_cel_expr(
                    while_expr, effect_path=effect_path, extra_names=body_loop_names
                )
                validate_cel_syntax(
                    while_expr,
                    effect_path=effect_path,
                    effect_name=validated_name,
                    label="Loop while CEL expression",
                )
            while_def = LoopWhileDef(
                mode=mode,
                template=while_config.get("template") if mode == "model" else None,
                expr=while_config.get("expr") if mode == "cel" else None,
                strict=bool(while_config.get("strict")) if mode == "cel" else False,
            )

    if "each" in effect:
        each_config = effect.get("each")
        if isinstance(each_config, dict):
            in_path = str(each_config.get("in") or "")
            validate_each_in_path(
                in_path, effect_path=effect_path, loop_names=loop_names
            )
            each_def = LoopEachDef(
                in_path=in_path,
                as_name=each_as_name or "item",
                truncate=bool(each_config.get("truncate")),
            )

    # Iteration bounds. Unset means no cap — see LoopDefinition.max_iterations.
    max_iterations_raw = effect.get("max_iterations")
    max_iterations: int | None = (
        int(max_iterations_raw) if max_iterations_raw is not None else None
    )
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
    if collect and validated_name is None:
        raise ValueError(
            f"Loop at '{effect_path}' sets 'collect: {collect}' but has no "
            "'name': collected values are written under the loop's own node, "
            "which an unnamed loop has none of. Give the loop a 'name' to fix "
            "this."
        )

    # Execution topology for each-loops
    flow = _normalize_flow(effect.get("flow") or "chain")

    # prime.<loop>.prev is only defined in chain flow (see LoopRuntime) — a
    # tree loop runs every pass in parallel, so there is no well-defined
    # previous pass to read. Checked against the raw body: a reference
    # several containers deep (an inner if/dynamic/loop) is just as wrong,
    # since the whole subtree runs inside the same parallel iteration.
    if (
        validated_name
        and flow == "tree"
        and each_def is not None
        and _tree_loop_prev_references(body_effects, validated_name)
    ):
        raise ValueError(
            f"Loop '{validated_name}' at '{effect_path}': "
            f"'prime.{validated_name}.prev' is not defined in flow: tree — "
            "tree passes run in parallel, so there is no previous pass to "
            "read. Use flow: chain (the default) if the body needs the "
            "previous pass, or remove the reference."
        )

    # Max parallel workers (only meaningful when flow="tree")
    max_concurrency_raw = effect.get("max_concurrency")
    max_concurrency: int | None = (
        int(max_concurrency_raw) if max_concurrency_raw is not None else None
    )

    labels_raw = effect.get("labels")
    labels = dict(labels_raw) if isinstance(labels_raw, dict) else None

    return LoopDefinition(
        name=validated_name,
        body=tuple(compiled_body),
        while_def=while_def,
        each_def=each_def,
        max_iterations=max_iterations,
        min_iterations=min_iterations,
        on_error=on_error,
        collect=collect,
        labels=labels,
        flow=flow,
        max_concurrency=max_concurrency,
    )


def _check_param_leaves(
    value: Any,
    *,
    name: str,
    effect_path: str,
    field: str,
    loop_names: frozenset[str],
) -> None:
    """Walk a tool effect's ``params`` for malformed templates and bad ``{from:}`` roots.

    A leaf matching ``{from: <path>}`` (optionally with ``default:``) is a
    by-reference param (#234): its path is checked against the same rooting
    rules as a ``use`` effect's own by-reference inputs, and — since it never
    renders — is not also walked as a template. Every other leaf is checked
    exactly as :func:`_check_templates` checks ``params`` today.
    """
    ref = param_reference(value)
    if ref is not None:
        validate_reference_path(
            ref.path,
            label=f"Tool effect '{name}' param '{field}'",
            effect_path=effect_path,
            loop_names=loop_names,
        )
        return
    if isinstance(value, str):
        reason = template_syntax_error(value)
        if reason is not None:
            raise ValueError(
                f"{effect_path}.{field}: malformed Mustache template: {reason}"
            )
    elif isinstance(value, dict):
        for key, item in value.items():
            _check_param_leaves(
                item,
                name=name,
                effect_path=effect_path,
                field=f"{field}.{key}",
                loop_names=loop_names,
            )
    elif isinstance(value, list):
        for index, item in enumerate(value):
            _check_param_leaves(
                item,
                name=name,
                effect_path=effect_path,
                field=f"{field}[{index}]",
                loop_names=loop_names,
            )


def _check_security_sensitive_param_leaf(
    value: Any, *, name: str, field: str
) -> None:
    """Reject a ``{from: <path>}`` leaf anywhere inside a security-sensitive
    param (today: ``allowed_commands``) at compile time.

    Mirrors the runtime guard in ``core.tool._reject_templated_security_params``:
    only a document's literal, unrendered list of strings is ever honoured for
    a plugin's own allowlist, so a by-reference value — which can carry
    runtime or model-generated content — is caught here too, before the
    orchestration ever runs.
    """
    if param_reference(value) is not None:
        raise ValueError(
            f"Tool effect '{name}' param '{field}': a by-reference '{{from: ...}}' "
            "value is not honoured for this security-sensitive setting; it must "
            "be a literal list of strings."
        )
    if isinstance(value, dict):
        for key, item in value.items():
            _check_security_sensitive_param_leaf(item, name=name, field=f"{field}.{key}")
    elif isinstance(value, list):
        for index, item in enumerate(value):
            _check_security_sensitive_param_leaf(item, name=name, field=f"{field}[{index}]")


def _compile_tool(
    effect: dict[str, Any],
    *,
    scope_path: str,
    effect_path: str,
    loop_names: frozenset[str] = frozenset(),
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

    params_json = effect.get("params_json")
    if params_json is not None and not isinstance(params_json, str):
        raise ValueError(
            f"Tool effect '{name}' at '{effect_path}' has invalid 'params_json': "
            "expected a string (a Mustache template that renders to JSON)."
        )

    prompt = effect.get("prompt")
    if prompt is not None and not isinstance(prompt, str):
        prompt = None

    _check_templates(prompt, effect_path=effect_path, field="prompt")
    _check_param_leaves(
        params, name=str(name), effect_path=effect_path, field="params", loop_names=loop_names
    )
    for sensitive_key in _SECURITY_SENSITIVE_PARAM_KEYS & params.keys():
        _check_security_sensitive_param_leaf(
            params[sensitive_key], name=str(name), field=f"params.{sensitive_key}"
        )
    _check_templates(params_json, effect_path=effect_path, field="params_json")

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

    retries = _compile_retries(effect)
    expect = _compile_expect(
        effect, effect_type="tool", effect_path=effect_path, loop_names=loop_names
    )
    group_raw = effect.get("group")
    group = group_raw.strip() if isinstance(group_raw, str) and group_raw.strip() else None

    _ = scope_path  # used by caller for deterministic addressing context
    return ToolDefinition(
        name=name,
        provider=provider.strip(),
        params=params,
        params_json=params_json,
        prompt=prompt,
        model=model,
        timeout_ms=timeout_ms,
        on_error=on_error,
        description=description,
        retries=retries,
        expect=expect,
        group=group,
    )


def _compile_use(
    effect: dict[str, Any],
    *,
    scope_path: str,
    effect_path: str,
    loop_names: frozenset[str] = frozenset(),
) -> UseDefinition:
    """Compile a use (sub-orchestration) effect."""
    import warnings

    name = effect.get("name")
    if not name:
        raise ValueError(f"Use effect at '{effect_path}' is missing 'name'.")

    ref = effect.get("ref")
    path = effect.get("path")
    orchestration = effect.get("orchestration")
    inline = effect.get("inline")

    has_ref = isinstance(ref, str) and ref.strip()
    has_path = isinstance(path, str) and path.strip()
    has_orch = isinstance(orchestration, str) and orchestration.strip()
    has_inline = isinstance(inline, str) and inline.strip()

    set_fields = [
        f for f, present in (
            ("ref", has_ref),
            ("path", has_path),
            ("orchestration", has_orch),
            ("inline", has_inline),
        ) if present
    ]

    if not set_fields:
        raise ValueError(
            f"Use effect '{name}' at '{effect_path}' requires exactly one of "
            "'ref' (curation library lookup), 'path' (filesystem), "
            "'orchestration' (deprecated), or 'inline' (Mustache template yielding YAML)."
        )
    if len(set_fields) > 1:
        raise ValueError(
            f"Use effect '{name}' at '{effect_path}' has multiple reference fields set "
            f"({', '.join(set_fields)}). Specify exactly one of ref/path/orchestration/inline."
        )

    if has_orch:
        warnings.warn(
            f"Use effect '{name}': the 'orchestration' field is deprecated. "
            "Use 'ref' for curation library lookup (e.g. 'utilities/critique') "
            "or 'path' for filesystem paths.",
            DeprecationWarning,
            stacklevel=2,
        )

    inputs = effect.get("inputs") or None
    if inputs is not None and not isinstance(inputs, dict):
        inputs = None

    _check_templates(inline, effect_path=effect_path, field="inline")
    for input_name, value in (inputs or {}).items():
        # Only string inputs render; a `{from: <path>}` reference does not.
        if isinstance(value, str):
            _check_templates(
                value, effect_path=effect_path, field=f"inputs.{input_name}"
            )

    # By-reference inputs (`name: {from: <path>}`) pass the resolved value
    # itself; their paths follow the same rooting rules as `each.in`, plus the
    # bindings of enclosing loops, so a bad root fails at `cof check` time.
    for input_name, value in (inputs or {}).items():
        from_path = reference_path(value)
        if from_path is not None:
            validate_reference_path(
                from_path,
                label=f"Use effect '{name}' input '{input_name}'",
                effect_path=effect_path,
                loop_names=loop_names,
            )

    # Canonical form is `name: {path: ...}`; the bare-string shorthand
    # `name: prime.x.value` normalizes to the same thing (see core.outputs).
    outputs = normalize_outputs(
        effect.get("outputs") or None, context=f"Use effect '{name}'"
    ) or None

    validate_flag = bool(effect.get("validate", True))

    on_error_raw = str(effect.get("on_error") or "fail").strip().lower()
    on_error: Literal["fail", "skip", "continue"] = (
        cast(Literal["fail", "skip", "continue"], on_error_raw)
        if on_error_raw in ("fail", "skip", "continue")
        else "fail"
    )

    description = effect.get("description")
    if description is not None and not isinstance(description, str):
        description = None

    retries = _compile_retries(effect)
    expect = _compile_expect(
        effect, effect_type="use", effect_path=effect_path, loop_names=loop_names
    )

    _ = scope_path
    return UseDefinition(
        name=name,
        ref=ref.strip() if has_ref else None,
        path=path.strip() if has_path else None,
        orchestration=orchestration.strip() if has_orch else None,
        inline=inline.strip() if has_inline else None,
        inputs=inputs,
        outputs=outputs,
        validate=validate_flag,
        on_error=on_error,
        description=description,
        retries=retries,
        expect=expect,
    )


def _compile_prompt(effect: dict[str, Any], *, effect_path: str) -> PromptDefinition:
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
    _check_templates(template, effect_path=effect_path, field="template")
    for index, message in enumerate(messages or ()):
        _check_templates(
            message.content, effect_path=effect_path, field=f"messages[{index}].content"
        )
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
        for index, asset in enumerate(assets):
            _check_templates(asset.ref, effect_path=effect_path, field=f"assets[{index}].ref")

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

    group_raw = effect.get("group")
    group = group_raw.strip() if isinstance(group_raw, str) and group_raw.strip() else None

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
        group=group,
    )
