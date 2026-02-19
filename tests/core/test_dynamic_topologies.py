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


def test_compile_normalizes_flow_aliases_root_and_nested() -> None:
    orch = {
        "flow": "tot",
        "effects": [
            {
                "type": "dynamic",
                "name": "inner",
                "flow": "chain_of_thought",
                "effects": [
                    {"type": "prompt", "name": "step", "template": "hello"},
                ],
            }
        ],
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    assert root.flow == "tree"
    inner = root.effects[0]
    assert getattr(inner, "flow", None) == "chain"


def test_dynamic_metadata_records_canonical_flow() -> None:
    orch = {
        "flow": "tree_of_thought",
        "effects": [
            {"type": "prompt", "name": "a", "template": "a"},
        ],
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    store = Store({})
    runtime = DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test")
    runtime.execute(store=store)

    assert store.get("prime.meta.flow") == "tree"
    assert store.get("prime.meta.completed_at") is not None


def test_chain_and_tree_have_different_sibling_context_semantics() -> None:
    # second prompt depends on first prompt's runtime output.
    base_effects = [
        {"type": "prompt", "name": "first", "template": "alpha"},
        {"type": "prompt", "name": "second", "template": "{{prime.first.value}}"},
    ]

    chain_root = compile_orchestration(
        orch={"flow": "chain", "effects": base_effects}, root_name="prime"
    )
    tree_root = compile_orchestration(
        orch={"flow": "tree", "effects": base_effects}, root_name="prime"
    )

    chain_store = Store({})
    tree_store = Store({})
    adapter = EchoAdapter()

    DynamicRuntime(chain_root, adapter=adapter, model="unit-test").execute(
        store=chain_store
    )
    DynamicRuntime(tree_root, adapter=adapter, model="unit-test").execute(
        store=tree_store
    )

    assert chain_store.get("prime.first.value") == "alpha"
    assert tree_store.get("prime.first.value") == "alpha"

    # Chain sees prior sibling writes. Tree evaluates siblings against root snapshot.
    assert chain_store.get("prime.second.value") == "alpha"
    assert tree_store.get("prime.second.value") is None
