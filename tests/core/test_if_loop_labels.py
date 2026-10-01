"""`labels` on `if` and `loop` effects are recorded on `meta.labels`, the
same as #305 did for `dynamic`. Regression suite for #306.

Before this, the schema accepted `labels` on `ConditionalEffect` and
`LoopEffect` (`cof check` passed it) but nothing read it: it was silently
dropped at run time, exactly the #251 class of bug.
"""

from __future__ import annotations

from dataclasses import dataclass

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class EchoAdapter:
    name: str = "echo"

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        return GenerateResult(text=prompt, raw={"model": model})


def test_loop_labels_recorded_on_meta() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "l",
                "labels": {"stage": "draft"},
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "{{item}}"},
                ],
            },
        ]
    }
    state = {"input": {"items": ["a"]}}
    root = compile_orchestration(orch=orch, root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=Store(state))

    assert state["prime"]["l"]["meta"]["labels"] == {"stage": "draft"}


def test_if_labels_recorded_on_meta() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "name": "c",
                "labels": {"stage": "draft"},
                "if": {"mode": "cel", "expr": "true"},
                "then": [
                    {"type": "prompt", "name": "step", "template": "yes"},
                ],
            },
        ]
    }
    state: dict = {"input": {}}
    root = compile_orchestration(orch=orch, root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=Store(state))

    assert state["prime"]["c"]["meta"]["labels"] == {"stage": "draft"}


def test_loop_without_labels_records_none() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "l",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "{{item}}"},
                ],
            },
        ]
    }
    state = {"input": {"items": ["a"]}}
    root = compile_orchestration(orch=orch, root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=Store(state))

    assert state["prime"]["l"]["meta"]["labels"] is None
