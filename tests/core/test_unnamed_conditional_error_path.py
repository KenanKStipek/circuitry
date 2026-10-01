"""An error inside an unnamed conditional must name the failing branch
effect, not surface bare (#258/#269 review follow-up). ``DynamicRuntime`` and
``LoopRuntime`` both prefix a bare exception with the failing child's own
name when the container itself has no name of its own; ``ConditionalRuntime``
must do the same.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class EchoAdapter:
    name: str = "echo"
    prompts: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.prompts.append(prompt)
        if "BOOM" in prompt:
            raise RuntimeError("scripted failure")
        return GenerateResult(text=prompt, raw={"model": model})


def test_unnamed_conditional_branch_failure_names_the_effect() -> None:
    orch = {
        "effects": [
            {
                "type": "conditional",
                # No `name:` — this conditional is unnamed/transparent.
                "if": {"mode": "cel", "expr": "true"},
                "then": [{"type": "prompt", "name": "step", "template": "BOOM"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict = {}

    with pytest.raises(Exception) as exc_info:
        DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
            store=Store(state)
        )

    message = str(exc_info.value)
    assert "step" in message
    assert "ConditionalDefinition" not in message
