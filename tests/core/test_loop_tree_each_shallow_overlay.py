"""`flow: tree` each-loop iterations get a cheap shallow overlay of the
run context, not a per-item `deepcopy(ctx)`. Regression suite for #268.

Before this, every parallel iteration paid for a full deep copy of the
entire run state, retained for the loop's whole duration — O(N x |state|)
in time and memory. A shallow overlay (`{**ctx, as: item, ...}`, the same
shape the chain path already builds) is safe because no body effect can
write back through a shared nested dict: every write lands in the
iteration's own isolated ``Store`` (never in ``ctx`` itself), and template
rendering / `params_json` always hand a tool a freshly rendered or
JSON-round-tripped value, never a live reference into ``ctx``.
"""

from __future__ import annotations

import json
from copy import deepcopy
from dataclasses import dataclass, field

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class EchoAdapter:
    name: str = "echo"
    prompts: list[str] = field(default_factory=list)

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        self.prompts.append(prompt)
        return GenerateResult(text=prompt, raw={"model": model})


def _tree_each_orch(body: list[dict]) -> dict:
    return {
        "effects": [
            {
                "type": "loop",
                "name": "work",
                "flow": "tree",
                "max_concurrency": 4,
                "each": {"in": "input.items", "as": "item"},
                "body": body,
            },
        ]
    }


def test_each_iteration_sees_only_its_own_item_with_shared_nested_object() -> None:
    """Multiple items sharing the SAME nested dict/list object by reference
    (the realistic shape a shallow, not deep, copy has to be safe for) do
    not bleed one iteration's binding into another's."""
    shared_tags = ["draft", "needs-review"]
    items = [{"id": i, "tags": shared_tags} for i in range(20)]
    state = {"input": {"items": deepcopy(items)}}
    # Make every item's "tags" the exact same list object, not just equal.
    for item in state["input"]["items"]:
        item["tags"] = shared_tags

    orch = _tree_each_orch(
        [{"type": "prompt", "name": "step", "template": "id={{item.id}} tags={{item.tags}}"}]
    )
    root = compile_orchestration(orch=orch, root_name="prime")
    adapter = EchoAdapter()
    DynamicRuntime(root, adapter=adapter, model="unit-test").execute(store=Store(state))

    node = state["prime"]["work"]
    assert node["value"]["iterations"] == 20
    seen_ids = sorted(
        int(node[f"iter_{i}"]["step"]["value"].split()[0].split("=")[1]) for i in range(20)
    )
    assert seen_ids == list(range(20))
    for i in range(20):
        assert node[f"iter_{i}"]["step"]["value"] == f"id={i} tags=['draft', 'needs-review']"

    # The shared object itself is untouched — nothing wrote back into it.
    assert shared_tags == ["draft", "needs-review"]
    assert state["input"]["items"][0]["tags"] is shared_tags


def test_source_collection_unchanged_after_tree_each_loop_runs() -> None:
    """The pre-loop collection (ctx's own nested structure) is byte-for-byte
    the same after a tree each loop reads through it — nothing a body effect
    does writes back through the shared, non-deep-copied context."""
    items = [{"id": i, "nested": {"count": i, "children": [i, i + 1]}} for i in range(15)]
    original = deepcopy(items)
    state = {"input": {"items": items}}

    orch = _tree_each_orch(
        [
            {
                "type": "tool",
                "name": "step",
                "provider": "json",
                "params": {"mode": "stringify", "input": "{{item.id}}"},
            }
        ]
    )
    root = compile_orchestration(orch=orch, root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=Store(state))

    assert state["input"]["items"] == original


def test_tree_each_loop_matches_chain_loop_output_for_same_input() -> None:
    """The overlay change must not change *what* the loop produces, only
    how cheaply it gets there."""
    items = [{"id": i} for i in range(10)]

    def _run(flow: str) -> dict:
        state = {"input": {"items": deepcopy(items)}}
        orch = {
            "effects": [
                {
                    "type": "loop",
                    "name": "work",
                    "flow": flow,
                    "collect": "step",
                    "each": {"in": "input.items", "as": "item"},
                    "body": [
                        {
                            "type": "tool",
                            "name": "step",
                            "provider": "json",
                            "params": {"mode": "stringify", "input": "{{item.id}}"},
                        }
                    ],
                },
            ]
        }
        root = compile_orchestration(orch=orch, root_name="prime")
        DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=Store(state))
        return state["prime"]["work"]["collected"]["value"]

    chain_result = sorted(int(json.loads(v)) for v in _run("chain"))
    tree_result = sorted(int(json.loads(v)) for v in _run("tree"))
    assert chain_result == tree_result == list(range(10))
