"""`each` loops over a bounded collection know their length up front, so a
collection bigger than `max_iterations` is never a runaway the way an
unbounded `while` can be — hitting the cap used to truncate silently and
leave the run green. See issue #209.

`while` keeps the cap as its safety floor, but the loop node must say
`max_iterations_reached` (not converge silently) when the cap — not the
condition — ended it.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.loop import LoopBoundsError
from circuitry.core.store import Store


@dataclass(frozen=True)
class EchoAdapter:
    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=prompt, raw={"model": model, "prompt": prompt})


@dataclass(frozen=True)
class AlwaysYesAdapter:
    """Never converges — every while-condition check answers `yes`."""

    name: str = "always-yes"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text="yes", raw={})


@dataclass(frozen=True)
class FailOnAdapter:
    """Raises for one specific prompt text, echoes everything else."""

    fail_on: str
    name: str = "fail-on"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        if prompt == self.fail_on:
            raise RuntimeError(f"boom on {prompt!r}")
        return GenerateResult(text=prompt, raw={"model": model, "prompt": prompt})


@dataclass
class RepliesAdapter:
    """Returns queued replies in order, falling back to `yes` once exhausted."""

    name: str = "replies"
    replies: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        text = self.replies.pop(0) if self.replies else "yes"
        return GenerateResult(text=text, raw={})


def _each_orch(
    *,
    max_iterations: int = 3,
    truncate: bool = False,
    flow: str = "chain",
    on_error: str = "fail",
) -> dict:
    each: dict = {"in": "input.items", "as": "item"}
    if truncate:
        each["truncate"] = True
    return {
        "effects": [
            {
                "type": "loop",
                "name": "raster",
                "each": each,
                "flow": flow,
                "max_iterations": max_iterations,
                "on_error": on_error,
                "body": [{"type": "prompt", "name": "step", "template": "{{item}}"}],
            }
        ]
    }


@pytest.mark.parametrize("flow", ["chain", "tree"])
def test_each_over_max_iterations_fails_at_loop_start_by_default(flow: str) -> None:
    """5 items, cap of 3, no `truncate`: errors loudly, naming both numbers."""
    root = compile_orchestration(orch=_each_orch(max_iterations=3, flow=flow), root_name="prime")
    store = Store({"input": {"items": list(range(5))}})

    with pytest.raises(RuntimeError) as excinfo:
        DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    cause = excinfo.value.__cause__
    assert isinstance(cause, LoopBoundsError)
    assert "'raster'" in str(cause)
    assert "5 items" in str(cause)
    assert "max_iterations is 3" in str(cause)

    meta = store.get("prime.raster.meta")
    assert "5 items" in meta["error"]

    value = store.get("prime.raster.value")
    assert value["termination"]["reason"] == "error"
    assert value["iterations"] == 0


def test_each_at_exactly_max_iterations_does_not_error() -> None:
    """Boundary: collection length == max_iterations is fine, not a bounds error."""
    root = compile_orchestration(orch=_each_orch(max_iterations=5), root_name="prime")
    store = Store({"input": {"items": list(range(5))}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    value = store.get("prime.raster.value")
    assert value["iterations"] == 5
    assert value["termination"]["reason"] == "collection_exhausted"
    assert "unvisited" not in value["termination"]


@pytest.mark.parametrize("flow", ["chain", "tree"])
def test_each_truncate_true_processes_first_n_and_records_unvisited(flow: str) -> None:
    """The opt-in: process the first `max_iterations` items, and say so."""
    root = compile_orchestration(
        orch=_each_orch(max_iterations=3, truncate=True, flow=flow), root_name="prime"
    )
    store = Store({"input": {"items": list(range(5))}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    value = store.get("prime.raster.value")
    assert value["iterations"] == 3
    assert value["termination"]["reason"] == "max_iterations_reached"
    assert value["termination"]["unvisited"] == 2


def test_each_tree_on_error_continue_with_failure_is_collection_exhausted() -> None:
    """A failed iteration under on_error: continue must not read as the cap
    ending the loop — the collection was still fully visited, one pass just
    errored (see issue #209 tree-flow regression).
    """
    root = compile_orchestration(
        orch=_each_orch(max_iterations=5, flow="tree", on_error="continue"),
        root_name="prime",
    )
    store = Store({"input": {"items": list(range(5))}})

    DynamicRuntime(
        root, adapter=FailOnAdapter(fail_on="2"), model="unit-test"
    ).execute(store=store)

    value = store.get("prime.raster.value")
    assert value["iterations"] == 4
    assert value["termination"]["reason"] == "collection_exhausted"
    assert "unvisited" not in value["termination"]


def test_each_tree_on_error_continue_with_failure_and_truncate_records_unvisited() -> None:
    """Truncation and a failed iteration are independent: the failure must not
    steal the `max_iterations_reached` + `unvisited` signal that `truncate`
    earned.
    """
    root = compile_orchestration(
        orch=_each_orch(
            max_iterations=3, truncate=True, flow="tree", on_error="continue"
        ),
        root_name="prime",
    )
    store = Store({"input": {"items": list(range(5))}})

    DynamicRuntime(
        root, adapter=FailOnAdapter(fail_on="1"), model="unit-test"
    ).execute(store=store)

    value = store.get("prime.raster.value")
    assert value["iterations"] == 2
    assert value["termination"]["reason"] == "max_iterations_reached"
    assert value["termination"]["unvisited"] == 2


def _while_orch(*, max_iterations: int) -> dict:
    return {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "max_iterations": max_iterations,
                "while": {"mode": "model", "template": "Continue?"},
                "body": [{"type": "prompt", "name": "step", "template": "STEP"}],
            }
        ]
    }


def test_while_hitting_cap_records_max_iterations_reached() -> None:
    """A condition that never says stop must not read as `condition_false`."""
    root = compile_orchestration(orch=_while_orch(max_iterations=3), root_name="prime")
    store = Store({"input": {}})

    DynamicRuntime(root, adapter=AlwaysYesAdapter(), model="unit-test").execute(store=store)

    value = store.get("prime.spin.value")
    assert value["iterations"] == 3
    assert value["termination"]["reason"] == "max_iterations_reached"


def test_while_hitting_cap_warns_under_verbose(capsys: pytest.CaptureFixture[str]) -> None:
    root = compile_orchestration(orch=_while_orch(max_iterations=2), root_name="prime")
    store = Store({"input": {}})

    DynamicRuntime(
        root, adapter=AlwaysYesAdapter(), model="unit-test", verbose=True
    ).execute(store=store)

    out = capsys.readouterr().out.replace("\n", " ")
    assert "'spin'" in out
    assert "stopped after 2 iterations" in out
    assert "max_iterations (2) reached" in out
    assert "without the while-condition becoming false" in out


def test_while_condition_false_before_cap_is_not_max_iterations_reached() -> None:
    """The converging case must keep its own reason, not get relabeled.

    Uses a CEL condition on the body's own output — a `mode: model` condition
    would consume the same adapter replies queue as the body prompt, which
    conflates "what the condition saw" with "what the body produced".
    """
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "max_iterations": 10,
                "min_iterations": 1,
                "while": {"mode": "cel", "expr": "state.prime.step.value != 'stop'"},
                "body": [{"type": "prompt", "name": "step", "template": "STEP"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {}})

    DynamicRuntime(
        root, adapter=RepliesAdapter(replies=["go", "stop"]), model="unit-test"
    ).execute(store=store)

    value = store.get("prime.spin.value")
    assert value["iterations"] == 2
    assert value["termination"]["reason"] == "condition_false"
