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


def test_collect_ignores_stale_passes_when_a_named_loops_node_is_reused_by_an_outer_unnamed_loop() -> None:
    """A named loop's node is a dict on the enclosing store, keyed by the
    loop's name. When the loop sits in the body of an *unnamed* outer loop,
    every outer pass writes into that same enclosing store, so the named
    inner loop's node — and whatever ``iter_<N>`` keys are already on it —
    is reused across outer passes.

    Before this fix, ``_collect_values`` scanned every ``iter_<N>`` key still
    on the node, so a first outer pass with 3 inner passes left ``iter_2``
    sitting on the node for a second, shorter (2-pass) outer pass to
    mistakenly pick up alongside its own.
    """
    orch = {
        "effects": [
            {
                "type": "loop",
                # Outer: unnamed, so "inner"'s node is the same dict across
                # both outer passes.
                "each": {"in": "input.groups", "as": "group"},
                "body": [
                    {
                        "type": "loop",
                        "name": "inner",
                        "collect": "step",
                        "each": {"in": "group", "as": "item"},
                        "body": [
                            {"type": "prompt", "name": "step", "template": "{{item}}"}
                        ],
                    }
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    state = {"input": {"groups": [["a", "b", "c"], ["x", "y"]]}}
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
        store=Store(state)
    )
    # The second, shorter outer pass's "inner" collect must hold only its
    # own two passes — not the first pass's leftover third one.
    assert state["prime"]["inner"]["collected"]["value"] == ["x", "y"]


def test_collect_ignores_iter_keys_seeded_onto_the_node_before_the_run() -> None:
    """The same reuse hazard from a different source: a node pre-seeded with
    an earlier run's ``iter_<N>`` keys (e.g. ``--state``/persistence resume)
    must not have those stale passes collected alongside a fresh run's own.
    """
    orch = _orch(flow="chain", on_error="continue")
    root = compile_orchestration(orch=orch, root_name="prime")
    state = {
        "input": {"items": ["x", "y"]},
        "prime": {
            "outer": {
                "value": None,
                "meta": {},
                # Stale pass from an earlier run of this same document.
                "iter_5": {"step": {"value": "STALE", "meta": {}}},
            }
        },
    }
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
        store=Store(state)
    )
    node = state["prime"]["outer"]
    assert node["collected"]["value"] == ["x", "y"]
    assert "STALE" not in node["collected"]["value"]


def test_while_continue_collect_excludes_the_failed_pass() -> None:
    """The collect contract (#239) under a ``while`` loop with
    ``on_error: continue`` — only tested so far on ``each`` loops."""

    @dataclass
    class FailMiddleAdapter:
        """Fails the one pass whose rendered prompt is 'pass 1'."""

        name: str = "fail-middle"

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            if prompt == "pass 1":
                raise RuntimeError("scripted failure")
            return GenerateResult(text=prompt, raw={"model": model})

    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "collect": "step",
                "on_error": "continue",
                "max_iterations": 3,
                "while": {"mode": "cel", "expr": "state.iter.index < 3"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "pass {{_loop_index}}"}
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict = {"input": {}}
    DynamicRuntime(root, adapter=FailMiddleAdapter(), model="unit-test").execute(
        store=Store(state)
    )
    node = state["prime"]["spin"]
    assert node["collected"]["value"] == ["pass 0", "pass 2"]
    assert node["meta"]["failed_passes"] == [1]
