from __future__ import annotations

from typing import Any, Union

from .dynamic import DynamicDefinition
from .prompt import PromptDefinition
from .reflector import ReflectorDefinition


StepDef = Union[DynamicDefinition, PromptDefinition, ReflectorDefinition]


def compile_orchestration(
    *, orch: dict[str, Any], root_name: str = "prime"
) -> DynamicDefinition:
    steps = orch.get("steps") or []
    if not isinstance(steps, list):
        raise ValueError("Orchestration 'steps' must be a list.")

    compiled_steps: list[StepDef] = [
        _compile_step(s) for s in steps if isinstance(s, dict)
    ]

    return DynamicDefinition(
        name=root_name,
        steps=compiled_steps,
        strategy="chain",
    )


def _compile_step(step: dict[str, Any]) -> StepDef:
    step_type = (step.get("type") or "").strip().lower()
    name = step.get("name")

    if step_type == "prompt":
        if not name:
            raise ValueError("Prompt step is missing 'name'.")
        template = step.get("template")
        if not isinstance(template, str) or not template.strip():
            raise ValueError(f"Prompt '{name}' missing non-empty 'template'.")
        return PromptDefinition(name=name, template=template)

    if step_type == "dynamic":
        if not name:
            raise ValueError("Dynamic step is missing 'name'.")

        strategy = (step.get("strategy") or "chain").strip().lower()
        if strategy != "chain":
            strategy = "chain"

        child_steps = step.get("steps") or []
        if not isinstance(child_steps, list):
            raise ValueError(f"Dynamic '{name}' steps must be a list.")

        compiled_children = [
            _compile_step(s) for s in child_steps if isinstance(s, dict)
        ]

        return DynamicDefinition(
            name=name,
            steps=compiled_children,
            strategy=strategy,
        )

    if step_type == "reflector":
        if not name:
            raise ValueError("Reflector step is missing 'name'.")

        strategy = (step.get("strategy") or "chain").strip().lower()
        if strategy != "chain":
            strategy = "chain"

        inner_steps = step.get("steps") or []
        if not isinstance(inner_steps, list):
            raise ValueError(f"Reflector '{name}' steps must be a list.")

        compiled_inner = [_compile_step(s) for s in inner_steps if isinstance(s, dict)]

        inner_dynamic = DynamicDefinition(
            name="inner",
            steps=compiled_inner,
            strategy=strategy,
        )

        return ReflectorDefinition(
            name=name,
            inner=inner_dynamic,
        )

    raise ValueError(f"Unsupported step type: {step_type!r}")
