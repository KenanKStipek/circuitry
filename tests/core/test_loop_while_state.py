"""`while`-loop state hygiene: `state.iter.index` resolves (#242), and a
`while` loop never leaks `_loop_index`/`iter` into the context it was given
(#260 part 1).

Before #242, a `while` condition's CEL expression could reference
`state.iter.index` (``cof check`` allowed it, since the loop's own `iter`
binding is in scope for its while condition the same as for its body) but
the value was never resolvable: the loop only set `ctx["iter"]` once a pass
was about to run, after the condition for that pass had already been
evaluated, so the condition always saw the *previous* pass's index, one
check behind.

Before #260 part 1, a `while` loop set `ctx["_loop_index"]`/`ctx["iter"]`
directly on the dict it was given rather than building a per-pass overlay
the way `each` already does. At the root, `ctx` *is* the run state, so the
loop's own last index leaked into saved state and into whatever ran after
it; nested inside another loop's body, the inner `while`'s index clobbered
the outer pass's own `_loop_index`/`iter` for every later step in that pass.
"""

from __future__ import annotations

from dataclasses import dataclass

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class EchoAdapter:
    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=prompt, raw={"model": model})


def test_while_condition_sees_completed_pass_count_as_iter_index() -> None:
    """`state.iter.index` is 0 on the check before the first pass, and N
    after N passes have completed — so a CEL condition can cap the loop by
    pass count without `max_iterations`."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "while": {"mode": "cel", "expr": "state.iter.index < 3"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "pass {{_loop_index}}"}
                ],
            },
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict = {"input": {}}
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
        store=Store(state)
    )
    value = state["prime"]["spin"]["value"]
    assert value["iterations"] == 3
    assert value["termination"]["reason"] == "condition_false"


def test_while_condition_with_min_iterations_zero_and_always_false_runs_zero_passes() -> None:
    """0 completed passes on the very first check: `state.iter.index < 0` is
    immediately false, so the loop never runs a pass."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "while": {"mode": "cel", "expr": "state.iter.index < 0"},
                "body": [{"type": "prompt", "name": "step", "template": "pass"}],
            },
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict = {"input": {}}
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
        store=Store(state)
    )
    assert state["prime"]["spin"]["value"]["iterations"] == 0


def test_root_while_loop_leaves_no_loop_index_or_iter_in_root_state() -> None:
    """A root `while` loop must not write `_loop_index`/`iter` onto the run
    state itself, and a later effect must not see the loop's last pass."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "max_iterations": 2,
                "while": {"mode": "cel", "expr": "true"},
                "body": [{"type": "prompt", "name": "tick", "template": "{{_loop_index}}"}],
            },
            {
                "type": "prompt",
                "name": "after",
                "template": "idx={{_loop_index}} iter={{iter.index}}",
            },
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict = {"input": {}}
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
        store=Store(state)
    )
    assert "_loop_index" not in state
    assert "iter" not in state
    assert state["prime"]["after"]["value"] == "idx= iter="


def test_nested_while_loop_does_not_clobber_the_outer_loops_own_index() -> None:
    """A `while` loop nested as the first body step of an `each` loop must
    not leave later steps in that same pass seeing the inner loop's final
    index instead of the outer loop's own."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "each": {"in": "input.items", "as": "b"},
                "body": [
                    {
                        "type": "loop",
                        "name": "inner",
                        "max_iterations": 4,
                        "while": {"mode": "cel", "expr": "true"},
                        "body": [
                            {"type": "prompt", "name": "spin_step", "template": "spin"}
                        ],
                    },
                    {
                        "type": "prompt",
                        "name": "which",
                        "template": "{{b}}#{{_loop_index}}",
                    },
                ],
            },
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict = {"input": {"items": ["a", "b"]}}
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
        store=Store(state)
    )
    assert state["prime"]["outer"]["iter_0"]["which"]["value"] == "a#0"
    assert state["prime"]["outer"]["iter_1"]["which"]["value"] == "b#1"
