"""An error inside an unnamed loop must name the real effect, not a Python
class name (#269 comment item: "labelled prime.LoopDefinition[N] instead of
a state path").

An unnamed loop has no state node of its own (it's transparent — its body
writes directly into the parent's namespace), so the failing body effect's
own name is the only identifying information available.
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


def test_unnamed_loop_body_failure_names_the_effect_not_a_class() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                # No `name:` — this loop is unnamed/transparent.
                "each": {"in": "input.items", "as": "item"},
                "body": [{"type": "prompt", "name": "step", "template": "{{item}}"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    state = {"input": {"items": ["BOOM"]}}

    with pytest.raises(Exception) as exc_info:
        DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
            store=Store(state)
        )

    message = str(exc_info.value)
    assert "step" in message
    assert "LoopDefinition" not in message
    assert "ConditionalDefinition" not in message
