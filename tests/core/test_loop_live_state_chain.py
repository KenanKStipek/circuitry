"""A chain loop publishes after each completed pass. Regression suite for #299.

Before this, a named loop's own node only reached ``on_write`` (the
``--live-state`` mirror, and any other state observer) through whatever a
body effect's own write happened to trigger, and tree flow republishes the
whole node after every pass (#291) while chain flow did not touch
``on_write`` between passes at all \u2014 an observer watching a long chain loop
(a retry loop, an ``each`` over shots) saw nothing move until the loop
finished, then every pass at once.
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

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        self.prompts.append(prompt)
        return GenerateResult(text=prompt, raw={"model": model})


def _snapshots_with(key: str, snapshots: list[dict]) -> list[dict]:
    return [s for s in snapshots if key in s.get("prime", {}).get("work", {})]


def test_each_chain_loop_publishes_after_every_completed_pass() -> None:
    snapshots: list[dict] = []
    store = Store(state={"input": {"items": ["a", "b", "c"]}}, on_write=lambda s: snapshots.append(deepcopy(s)))

    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "work",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "{{item}}"},
                ],
            },
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    # iter_0 shows up in some snapshot before iter_1 ever does.
    first_with_iter0 = next(
        i for i, s in enumerate(snapshots) if "iter_0" in s["prime"]["work"]
    )
    first_with_iter1 = next(
        i for i, s in enumerate(snapshots) if "iter_1" in s["prime"]["work"]
    )
    assert first_with_iter0 < first_with_iter1

    # And iter_0 is actually complete (not a partial write) by the time it
    # first appears — the publish happens right after the pass completes.
    assert snapshots[first_with_iter0]["prime"]["work"]["iter_0"]["step"]["value"] == "a"


def test_while_loop_publishes_after_every_completed_pass() -> None:
    snapshots: list[dict] = []
    store = Store(state={"input": {}}, on_write=lambda s: snapshots.append(deepcopy(s)))

    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "work",
                "min_iterations": 1,
                "max_iterations": 3,
                "while": {"mode": "cel", "expr": "state.iter.count < 3"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "pass {{_loop_index}}"},
                ],
            },
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    first_with_iter0 = next(
        i for i, s in enumerate(snapshots) if "iter_0" in s["prime"]["work"]
    )
    first_with_iter2 = next(
        i for i, s in enumerate(snapshots) if "iter_2" in s["prime"]["work"]
    )
    assert first_with_iter0 < first_with_iter2
