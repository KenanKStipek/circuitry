"""``collect`` under a failed pass (issue #239).

Before this fix, ``collected`` was assembled by walking ``iter_0 ..
iter_{iteration_count-1}``, where ``iteration_count`` only counts passes
that ran to completion. A failed pass under ``on_error: continue`` (or
``break`` in a ``tree`` loop, where every pass is already running) threw
that off two ways at once: the failed pass's own `iter_<N>` node — with a
``None`` value and an error in ``meta`` — fell inside the walked range and
was collected as a stray ``null``, while the walk's upper bound, now one
short, dropped the last genuinely completed pass instead.

The contract under test: ``collected`` holds the values of the passes that
produced one, in pass order, never loses a completed pass, and a failed
pass is left out entirely (not a ``null`` placeholder) — the loop's
``meta.failed_passes`` names which pass indices were skipped.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class EchoAdapter:
    """Echoes every rendered prompt back; a prompt containing BOOM raises."""

    name: str = "echo"
    prompts: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.prompts.append(prompt)
        if "BOOM" in prompt:
            raise RuntimeError("scripted iteration failure")
        return GenerateResult(text=prompt, raw={"model": model})


def _orch(*, flow: str, on_error: str) -> dict:
    return {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "flow": flow,
                "on_error": on_error,
                "collect": "step",
                "each": {"in": "input.items", "as": "item"},
                "body": [{"type": "prompt", "name": "step", "template": "{{item}}"}],
            }
        ]
    }


def _run(orch: dict, items: list[str]) -> dict:
    root = compile_orchestration(orch=orch, root_name="prime")
    state = {"input": {"items": items}}
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
        store=Store(state)
    )
    return state


@pytest.mark.parametrize(
    ("items", "expected_collected", "expected_failed"),
    [
        (["BOOM", "b", "c"], ["b", "c"], [0]),
        (["a", "BOOM", "c"], ["a", "c"], [1]),
        (["a", "b", "BOOM"], ["a", "b"], [2]),
    ],
)
def test_chain_continue_excludes_the_failed_pass(
    items: list[str], expected_collected: list[str], expected_failed: list[int]
) -> None:
    state = _run(_orch(flow="chain", on_error="continue"), items)
    node = state["prime"]["outer"]
    assert node["collected"]["value"] == expected_collected
    assert node["meta"]["failed_passes"] == expected_failed


@pytest.mark.parametrize(
    ("items", "expected_collected", "expected_failed"),
    [
        (["BOOM", "b", "c"], ["b", "c"], [0]),
        (["a", "BOOM", "c"], ["a", "c"], [1]),
        (["a", "b", "BOOM"], ["a", "b"], [2]),
    ],
)
def test_tree_continue_excludes_the_failed_pass(
    items: list[str], expected_collected: list[str], expected_failed: list[int]
) -> None:
    state = _run(_orch(flow="tree", on_error="continue"), items)
    node = state["prime"]["outer"]
    assert node["collected"]["value"] == expected_collected
    assert node["meta"]["failed_passes"] == expected_failed


@pytest.mark.parametrize(
    ("items", "expected_collected", "expected_failed"),
    [
        (["BOOM", "b", "c"], ["b", "c"], [0]),
        (["a", "BOOM", "c"], ["a", "c"], [1]),
        (["a", "b", "BOOM"], ["a", "b"], [2]),
    ],
)
def test_tree_break_excludes_the_failed_pass(
    items: list[str], expected_collected: list[str], expected_failed: list[int]
) -> None:
    """In a ``tree`` loop every pass is already running when one fails, so
    ``break`` lets the others finish and drops only the failed one — same
    contract as ``continue`` here."""
    state = _run(_orch(flow="tree", on_error="break"), items)
    node = state["prime"]["outer"]
    assert node["collected"]["value"] == expected_collected
    assert node["meta"]["failed_passes"] == expected_failed


def test_chain_break_stops_at_the_failed_pass_and_excludes_it() -> None:
    """A ``chain`` loop under ``break`` never runs the passes after the
    failure, so this is the one case the pre-fix walk already got right —
    pinned here so the fix doesn't regress it."""
    state = _run(_orch(flow="chain", on_error="break"), ["a", "BOOM", "c"])
    node = state["prime"]["outer"]
    assert node["collected"]["value"] == ["a"]
    assert node["meta"]["failed_passes"] == [1]
    assert "iter_2" not in node


def test_no_failures_leaves_meta_without_failed_passes_key() -> None:
    state = _run(_orch(flow="chain", on_error="continue"), ["a", "b"])
    node = state["prime"]["outer"]
    assert node["collected"]["value"] == ["a", "b"]
    assert "failed_passes" not in node["meta"]
