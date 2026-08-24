"""End-to-end composition: a child orchestration written entirely against
`input.*`, driven from a parent `use.inputs` mapping — the full path from
parent `input` through the isolated child `input` namespace, compiled and
run through the real compiler/runtime (not a hand-built UseDefinition).

Unit-level isolation (child state not leaking parent keys) is already
covered by test_use.py::test_use_runtime_state_isolation; this test's job
is the compile-time contract: the child's own `each.in`/template refs are
all root-relative against its own `input` namespace, fed entirely by the
parent's `use.inputs` mapping.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

import yaml

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


def test_child_driven_entirely_by_input_namespace_from_parent_use_inputs(
    tmp_path: Path,
) -> None:
    child_orch = {
        "interface": {"inputs": {"who": {"type": "string", "required": True}}},
        "effects": [
            {"type": "prompt", "name": "greet", "template": "Hello {{input.who}}"},
            {
                "type": "loop",
                "name": "each_tag",
                "each": {"in": "input.tags", "as": "tag"},
                "body": [{"type": "prompt", "name": "step", "template": "{{tag}}"}],
            },
        ],
    }
    child_path = tmp_path / "child.yml"
    child_path.write_text(yaml.dump(child_orch), encoding="utf-8")

    parent_orch = {
        "effects": [
            {
                "type": "use",
                "name": "sub",
                "path": str(child_path),
                "inputs": {"who": "{{input.name}}", "tags": ["a", "b"]},
            }
        ]
    }
    root = compile_orchestration(orch=parent_orch, root_name="prime")
    store = Store({"input": {"name": "Alice"}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    # Full-namespace mode (no `outputs:` on the use effect, no child
    # interface.outputs): every child effect surfaces at prime.sub.<name>.
    assert store.get("prime.sub.greet.value") == "Hello Alice"
    loop_value = store.get("prime.sub.each_tag.value")
    assert loop_value["iterations"] == 2
    assert loop_value["termination"]["reason"] == "collection_exhausted"
