from __future__ import annotations

from dataclasses import dataclass

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass(frozen=True)
class EchoAdapter:
    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=prompt, raw={"model": model, "prompt": prompt})


def test_conditional_records_executed_effects_metadata() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "name": "gate",
                "if": {"mode": "cel", "expr": "state.input.ok == True"},
                "then": [{"type": "prompt", "name": "then_step", "template": "yes"}],
                "else": [{"type": "prompt", "name": "else_step", "template": "no"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"ok": True}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    value = store.get("prime.gate.value")
    assert isinstance(value, dict)
    assert value["branch"] == "then"
    assert isinstance(value["effects"], list)
    assert len(value["effects"]) == 1
    assert value["effects"][0]["name"] == "then_step"
    assert value["effects"][0]["type"] == "PromptDefinition"


def test_loop_records_effects_by_iteration_with_body_summary() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "repeat",
                "each": {"in": "input.items", "as": "item"},
                "body": [{"type": "prompt", "name": "render", "template": "{{item}}"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": ["a", "b", "c"]}})

    DynamicRuntime(
        root,
        adapter=EchoAdapter(),
        model="unit-test",
        dry_run=True,
    ).execute(store=store)

    loop_value = store.get("prime.repeat.value")
    assert isinstance(loop_value, dict)
    assert loop_value["iterations"] == 3
    assert loop_value["termination"]["reason"] == "collection_exhausted"

    effects_by_iteration = loop_value["effects_by_iteration"]
    assert isinstance(effects_by_iteration, list)
    assert len(effects_by_iteration) == 3
    for item in effects_by_iteration:
        assert item["count"] == 1
        assert item["executed_effects"][0]["name"] == "render"
        assert item["executed_effects"][0]["type"] == "PromptDefinition"
