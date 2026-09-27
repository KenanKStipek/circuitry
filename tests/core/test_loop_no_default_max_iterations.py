"""Issue #223: loops have no built-in iteration limit.

`max_iterations` used to default to 100 whether or not the orchestration set
it — an `each` loop over more than 100 items, or a `while` loop that took
more than 100 passes to converge, silently stopped short and left the run
green. `max_iterations` is now `None` (no cap) unless the orchestration sets
it: an `each` loop runs every item in its collection, and a `while` loop runs
until its condition is false.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import pytest

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


@dataclass
class ConditionCountingAdapter:
    """Says `yes` to the loop's own continuation prompt for the first
    ``continue_for`` checks, then `no` — distinguishing the condition prompt
    (which the runtime marks with "Should the loop continue?") from the
    body's own prompt so counting the checks doesn't also count passes.
    """

    continue_for: int
    name: str = "counting"
    checks: int = field(default=0, init=False)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        if "Should the loop continue?" in prompt:
            self.checks += 1
            return GenerateResult(text="yes" if self.checks <= self.continue_for else "no", raw={})
        return GenerateResult(text="body", raw={})


def _each_orch(*, flow: str) -> dict:
    return {
        "effects": [
            {
                "type": "loop",
                "name": "raster",
                "each": {"in": "input.items", "as": "item"},
                "flow": flow,
                "body": [{"type": "prompt", "name": "step", "template": "{{item}}"}],
            }
        ]
    }


@pytest.mark.parametrize("flow", ["chain", "tree"])
def test_each_over_1000_items_with_no_max_iterations_runs_every_item(flow: str) -> None:
    root = compile_orchestration(orch=_each_orch(flow=flow), root_name="prime")
    store = Store({"input": {"items": list(range(1000))}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    value = store.get("prime.raster.value")
    assert value["iterations"] == 1000
    assert value["termination"]["reason"] == "collection_exhausted"
    assert "unvisited" not in value["termination"]

    meta = store.get("prime.raster.meta")
    assert "max_iterations" not in meta


def test_while_with_no_max_iterations_runs_until_condition_is_false() -> None:
    """A condition that only converges after 150 passes must not be cut short
    by a built-in cap — there isn't one unless the orchestration sets it."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "while": {"mode": "model", "template": "Continue?"},
                "body": [{"type": "prompt", "name": "step", "template": "STEP"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {}})

    DynamicRuntime(
        root, adapter=ConditionCountingAdapter(continue_for=150), model="unit-test"
    ).execute(store=store)

    value = store.get("prime.spin.value")
    assert value["iterations"] == 150
    assert value["termination"]["reason"] == "condition_false"

    meta = store.get("prime.spin.meta")
    assert "max_iterations" not in meta
