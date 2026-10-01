"""``prime.<loop>.prev.<step>.value`` — the previous pass of a named loop,
readable from inside its own body. Regression suite for #243.

Before this, a named loop's body had no spelling for "the pass before this
one": ``{{prime.<step>.value}}`` is the current (not-yet-written) pass, and
``last``/``iter_<N>``/``collected`` only resolve after the loop completes.
The ``while`` condition already saw the previous pass (it is evaluated
*between* passes); the body did not — this closes that gap for both ``each``
and ``while``, in chain flow. Tree flow rejects the reference at compile
time: tree passes run in parallel, so there is no previous one.
"""

from __future__ import annotations

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


class EchoAdapter:
    name = "echo"

    def generate(self, *, model, prompt, timeout_seconds=120):
        return GenerateResult(text=prompt, raw={"model": model})


def _run(orch: dict, state: dict) -> dict:
    root = compile_orchestration(orch=orch, root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
        store=Store(state)
    )
    return state


def test_each_body_sees_prev_pass_absent_on_first() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "refine",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {
                        "type": "prompt",
                        "name": "step",
                        "template": "prev=[{{prime.refine.prev.step.value}}] item={{item}}",
                    },
                ],
            },
        ]
    }
    state = _run(orch, {"input": {"items": ["a", "b", "c"]}})

    node = state["prime"]["refine"]
    assert node["iter_0"]["step"]["value"] == "prev=[] item=a"
    assert node["iter_1"]["step"]["value"] == "prev=[prev=[] item=a] item=b"
    assert node["iter_2"]["step"]["value"] == "prev=[prev=[prev=[] item=a] item=b] item=c"


def test_each_body_prev_meta_also_resolves() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "refine",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "{{item}}"},
                    {
                        "type": "prompt",
                        "name": "probe",
                        "template": "adapter=[{{prime.refine.prev.step.meta.adapter}}]",
                    },
                ],
            },
        ]
    }
    state = _run(orch, {"input": {"items": ["a", "b"]}})

    node = state["prime"]["refine"]
    assert node["iter_0"]["probe"]["value"] == "adapter=[]"
    assert node["iter_1"]["probe"]["value"] == "adapter=[echo]"


def test_while_body_sees_prev_pass() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "polish",
                "min_iterations": 1,
                "max_iterations": 3,
                "while": {"mode": "cel", "expr": "state.iter.count < 3"},
                "body": [
                    {
                        "type": "prompt",
                        "name": "revise",
                        "template": "prev=[{{prime.polish.prev.revise.value}}] n={{_loop_index}}",
                    },
                ],
            },
        ]
    }
    state = _run(orch, {"input": {}})

    node = state["prime"]["polish"]
    assert node["iter_0"]["revise"]["value"] == "prev=[] n=0"
    assert node["iter_1"]["revise"]["value"] == "prev=[prev=[] n=0] n=1"
    assert node["iter_2"]["revise"]["value"] == "prev=[prev=[prev=[] n=0] n=1] n=2"


def test_cel_has_is_false_on_first_pass_true_after() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "refine",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {
                        "type": "if",
                        "if": {
                            "mode": "cel",
                            "expr": "has(state.prime.refine.prev)",
                        },
                        "then": [
                            {"type": "prompt", "name": "step", "template": "has-prev"},
                        ],
                        "else": [
                            {"type": "prompt", "name": "step", "template": "no-prev"},
                        ],
                    },
                ],
            },
        ]
    }
    state = _run(orch, {"input": {"items": ["a", "b"]}})

    node = state["prime"]["refine"]
    assert node["iter_0"]["step"]["value"] == "no-prev"
    assert node["iter_1"]["step"]["value"] == "has-prev"


def test_nested_named_loops_each_have_their_own_prev() -> None:
    """An inner loop's ``prev`` is its own, independent of the outer's.

    ``outer``'s own body is ``tag`` followed by the ``inner`` loop, so
    ``prime.outer.prev.tag`` is outer's prev (persists across outer passes);
    ``inner``'s own body is just ``step``, so ``prime.inner.prev.step`` is
    inner's prev — and a fresh ``LoopRuntime.execute()`` call runs for every
    outer pass, so it resets to absent at the start of each one.
    """
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "each": {"in": "input.outers", "as": "o"},
                "body": [
                    {
                        "type": "prompt",
                        "name": "tag",
                        "template": "outer_prev=[{{prime.outer.prev.tag.value}}] o={{o.id}}",
                    },
                    {
                        "type": "loop",
                        "name": "inner",
                        "each": {"in": "o.inners", "as": "i"},
                        "body": [
                            {
                                "type": "prompt",
                                "name": "step",
                                "template": "inner_prev=[{{prime.inner.prev.step.value}}] i={{i}}",
                            },
                        ],
                    },
                ],
            },
        ]
    }
    state = _run(
        orch,
        {
            "input": {
                "outers": [
                    {"id": "A", "inners": ["x", "y"]},
                    {"id": "B", "inners": ["z"]},
                ]
            }
        },
    )

    outer_node = state["prime"]["outer"]

    # outer's own prev: absent on its first pass, the first pass's tag on its second.
    assert outer_node["iter_0"]["tag"]["value"] == "outer_prev=[] o=A"
    assert outer_node["iter_1"]["tag"]["value"] == "outer_prev=[outer_prev=[] o=A] o=B"

    # inner's own prev: absent on inner's first pass EVERY outer pass —
    # it does not inherit anything from the previous outer pass's inner run.
    inner0 = outer_node["iter_0"]["inner"]
    assert inner0["iter_0"]["step"]["value"] == "inner_prev=[] i=x"
    assert inner0["iter_1"]["step"]["value"] == "inner_prev=[inner_prev=[] i=x] i=y"

    inner1 = outer_node["iter_1"]["inner"]
    assert inner1["iter_0"]["step"]["value"] == "inner_prev=[] i=z"


def test_tree_flow_rejects_prev_reference_at_compile_time() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "refine",
                "flow": "tree",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {
                        "type": "prompt",
                        "name": "step",
                        "template": "{{prime.refine.prev.step.value}}",
                    },
                ],
            },
        ]
    }
    with pytest.raises(ValueError, match="flow: tree"):
        compile_orchestration(orch=orch, root_name="prime")


def test_tree_flow_without_prev_reference_still_compiles() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "refine",
                "flow": "tree",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "{{item}}"},
                ],
            },
        ]
    }
    compile_orchestration(orch=orch, root_name="prime")
