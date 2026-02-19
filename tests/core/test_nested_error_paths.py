from __future__ import annotations

from dataclasses import dataclass

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass(frozen=True)
class FailingAdapter:
    name: str = "failing"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        raise RuntimeError("adapter boom")


def test_nested_dynamic_failures_include_hierarchical_effect_paths() -> None:
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "outer",
                "effects": [
                    {
                        "type": "dynamic",
                        "name": "inner",
                        "effects": [
                            {"type": "prompt", "name": "leaf", "template": "hello"},
                        ],
                    }
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})

    with pytest.raises(RuntimeError) as exc:
        DynamicRuntime(root, adapter=FailingAdapter(), model="unit-test").execute(
            store=store
        )

    message = str(exc.value)
    assert "prime.outer" in message
    assert "outer.inner" in message
    assert "inner.leaf" in message
    assert "adapter boom" in message
