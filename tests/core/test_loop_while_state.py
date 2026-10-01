"""`while`-loop state hygiene: `state.iter.index`/`state.iter.count` resolve
in a `while` condition (#242), and a `while` loop never leaks
`_loop_index`/`iter` into the context it was given (#260 part 1).

Before #242, a `while` condition's CEL expression could reference
`state.iter.index` (``cof check`` allowed it, since the loop's own `iter`
binding is in scope for its while condition the same as for its body), but
the value was never resolvable *on its own* — only as a side effect of a
separate bug. Before #260 part 1, a `while` loop wrote `ctx["_loop_index"]`/
`ctx["iter"]` directly onto the dict it was given, right before running
each pass, rather than building a per-pass overlay the way `each` already
does. At the root, `ctx` *is* the run state, so that mutation leaked into
saved state and into whatever ran after the loop; nested inside another
loop's body, the inner `while`'s index clobbered the outer pass's own
`_loop_index`/`iter` for every later step in that pass. But because the
mutation happened *before* running a pass and was never reverted, it had a
side effect every owner orchestration ended up depending on: by the time
the *next* check read `ctx["iter"]["index"]`, it saw the previous pass's
index, one check behind — so a condition written as `state.iter.index + 1
< N` read as "run at most N passes", and that is the meaning #260 keeps
while removing the leak: in a `while` condition, `iter.index` is the index
of the last *finished* pass (`-1` before the first pass, `N - 1` once N
passes have run), and `iter.count` is the same count without the `-1`
offset (`0` before the first pass, `N` once N passes have run) — the
uncompensated spelling for new conditions, where `state.iter.count < N`
reads the same as `state.iter.index + 1 < N`. The body's own `iter.index`
(and `_loop_index`) is unaffected by any of this: inside the body, it is
always the pass currently running, exactly as `each` already binds it.
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


def test_while_condition_index_is_the_last_finished_pass_and_count_is_how_many_ran() -> None:
    """`state.iter.index` is the index of the *last finished* pass — `-1`
    on the check before the first pass, `N - 1` once N passes have run —
    not the index of the pass about to run. This is the de-facto meaning
    the context leak #260 fixes used to produce (the leaking mutation wrote
    the about-to-run index onto `ctx` *before* running that pass's body, so
    the next check read it as the previous pass's index); existing
    conditions written against that leak, such as `state.iter.index + 1 <
    N`, must keep working once the leak is gone. `state.iter.count` is the
    uncompensated equivalent for new conditions: `N` once N passes have run,
    `0` before the first."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "min_iterations": 1,
                "max_iterations": 3,
                "while": {"mode": "cel", "expr": "true && state.iter.index + 1 < 3"},
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


def test_while_condition_owner_idiom_inside_an_enclosing_loop_each_outer_pass_runs_three() -> None:
    """The same `state.iter.index + 1 < N` idiom nested inside another
    loop's body: each outer pass's own inner `while` loop must see its own
    count from scratch, not leak across outer passes or into the outer
    loop's own `_loop_index`/`iter` (#260)."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "each": {"in": "input.items", "as": "x"},
                "body": [
                    {
                        "type": "loop",
                        "name": "spin",
                        "min_iterations": 1,
                        "max_iterations": 3,
                        "while": {
                            "mode": "cel",
                            "expr": "true && state.iter.index + 1 < 3",
                        },
                        "body": [
                            {
                                "type": "prompt",
                                "name": "step",
                                "template": "{{x}} pass {{_loop_index}}",
                            }
                        ],
                    }
                ],
            },
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    state = {"input": {"items": ["a", "b"]}}
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
        store=Store(state)
    )
    assert state["prime"]["outer"]["iter_0"]["spin"]["value"]["iterations"] == 3
    assert state["prime"]["outer"]["iter_1"]["spin"]["value"]["iterations"] == 3


def test_while_condition_iter_count_caps_the_loop_without_the_plus_one() -> None:
    """`state.iter.count` needs no `+ 1` compensation: `state.iter.count < 2`
    runs exactly 2 passes."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "while": {"mode": "cel", "expr": "state.iter.count < 2"},
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
    assert value["iterations"] == 2
    assert value["termination"]["reason"] == "condition_false"


def test_while_condition_count_zero_before_first_pass_stops_a_loop_that_requires_one() -> None:
    """`state.iter.count` is `0` on the very first check — a condition that
    requires at least one finished pass never runs one."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "while": {"mode": "cel", "expr": "state.iter.count < 0"},
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
