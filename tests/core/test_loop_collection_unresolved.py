"""`each.in` pointing at nothing terminates as `collection_unresolved`,
distinct from a genuinely empty list (`collection_exhausted`) — see
core.loop.LoopRuntime._resolve_collection and issue #86 stage 1.
"""

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


def _loop_orch(in_path: str) -> dict:
    return {
        "effects": [
            {
                "type": "loop",
                "name": "each_item",
                "each": {"in": in_path, "as": "item"},
                "body": [{"type": "prompt", "name": "step", "template": "{{item}}"}],
            }
        ]
    }


def test_missing_input_key_terminates_as_collection_unresolved() -> None:
    """input.items isn't in state at all: resolves to None, not [] — a
    misspelled or never-provided path must not masquerade as an empty run."""
    root = compile_orchestration(orch=_loop_orch("input.items"), root_name="prime")
    store = Store({"input": {}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    loop_value = store.get("prime.each_item.value")
    assert loop_value["iterations"] == 0
    assert loop_value["termination"]["reason"] == "collection_unresolved"

    meta = store.get("prime.each_item.meta")
    assert "resolved to None at segment 'items'" in meta["each_in_error"]


def test_wrong_type_at_target_terminates_as_collection_unresolved() -> None:
    """The path resolves, but to a non-list — also unresolved, not exhausted."""
    root = compile_orchestration(orch=_loop_orch("input.items"), root_name="prime")
    store = Store({"input": {"items": "not-a-list"}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    loop_value = store.get("prime.each_item.value")
    assert loop_value["iterations"] == 0
    assert loop_value["termination"]["reason"] == "collection_unresolved"

    meta = store.get("prime.each_item.meta")
    assert "instead of list" in meta["each_in_error"]


def test_actually_empty_list_terminates_as_collection_exhausted() -> None:
    """The counterpart: a real empty list is exhausted, not unresolved."""
    root = compile_orchestration(orch=_loop_orch("input.items"), root_name="prime")
    store = Store({"input": {"items": []}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    loop_value = store.get("prime.each_item.value")
    assert loop_value["iterations"] == 0
    assert loop_value["termination"]["reason"] == "collection_exhausted"

    meta = store.get("prime.each_item.meta")
    assert "each_in_error" not in meta
