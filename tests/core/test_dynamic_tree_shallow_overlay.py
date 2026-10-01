"""`flow: tree` dynamics give their children a cheap shallow overlay of the
run context, not a per-run `deepcopy(ctx)`. Regression suite for #318.

Before this, every `flow: tree` dynamic paid for a full deep copy of the
entire run state, every time it ran -- O(|state|) per run, and O(N x |state|)
for one inside a loop that runs N passes. A shallow overlay (`dict(ctx)`) is
safe for the same reason #309 established it is safe for tree `each` loops:
no child effect can write back through a shared nested dict, since every
write lands in the child's own isolated ``Store`` (never in ``ctx`` itself),
and every value a child reads out of ``ctx`` and hands to a tool/prompt is
already a fresh copy by the time it leaves this process (template rendering
stringifies it; ``params_json`` round-trips it through ``json.loads``).
"""

from __future__ import annotations

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

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.prompts.append(prompt)
        return GenerateResult(text=prompt, raw={"model": model})


def test_tree_dynamic_sibling_never_sees_a_concurrent_writers_write() -> None:
    """One child writes a nested value under its own state path; a sibling
    templating the identical path reads the dynamic-start snapshot, never
    the concurrent write -- the same guarantee `deepcopy(ctx)` gave, now
    from a shallow overlay (#318)."""
    orch = {
        "flow": "tree",
        "effects": [
            {
                "type": "tool",
                "name": "writer",
                "provider": "json",
                "params": {"mode": "stringify", "input": {"shared": {"counter": 1}}},
            },
            {
                "type": "prompt",
                "name": "reader",
                "template": "writer_value={{prime.writer.value}}",
            },
        ],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    assert store.get("prime.writer.value") is not None
    # "reader" rendered against the dynamic-start snapshot, where "prime.writer"
    # did not exist yet (an unresolved path renders empty) -- never the
    # sibling's concurrent write.
    assert store.get("prime.reader.value") == "writer_value="


def test_tree_dynamic_children_share_a_nested_object_without_mutating_it() -> None:
    """Two children of a `flow: tree` dynamic read the SAME nested dict/list
    object by reference -- the shape a shallow overlay, not a deepcopy, has
    to be safe for. Neither read mutates it, and the pre-run structure is
    byte-for-byte unchanged afterward, mirroring #309's loop-each test for
    the dynamic case."""
    shared = {"count": 0, "children": [1, 2, 3]}
    state = {"input": {"shared": shared}}
    original = deepcopy(shared)

    orch = {
        "flow": "tree",
        "effects": [
            {"type": "prompt", "name": "a", "template": "count={{input.shared.count}}"},
            {
                "type": "prompt",
                "name": "b",
                "template": "children={{input.shared.children}}",
            },
        ],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store(state)
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    assert store.get("prime.a.value") == "count=0"
    assert store.get("prime.b.value") == "children=[1, 2, 3]"
    # The shared object itself is untouched -- nothing wrote back into it.
    assert state["input"]["shared"] == original
    assert state["input"]["shared"] is shared
