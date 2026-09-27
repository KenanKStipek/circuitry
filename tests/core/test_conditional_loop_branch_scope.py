"""A step inside an `if` branch in a loop body must be able to read the
branch's earlier steps — templates and CEL alike.

Regression for #224. ``ConditionalRuntime.execute`` used to hand every branch
step the *same* ``ctx`` it was given, and inside a loop body that ``ctx`` is a
snapshot ``_scope_ctx`` (loop.py) built before the ``if`` ran — so a branch
step's own writes never reached the branch's later steps. At the top level
this happened to work anyway, because the ``ctx`` a top-level container hands
its children is a live reference to ``store.state``, not a copy.

Mirrors the reported YAML (a loop body: a tool, then an ``if`` whose branch
runs two tools where the second reads the first) with the dependency-free
``json`` tool provider in place of ``awk``, so the regression suite stays
hermetic and does not depend on a binary being on ``PATH``.
"""

from __future__ import annotations

from typing import Any

import pytest

from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


def _run(orch: dict[str, Any], state: dict[str, Any]) -> dict[str, Any]:
    root = compile_orchestration(orch=orch)
    DynamicRuntime(root, adapter=None, model="unit-test").execute(store=Store(state))
    return state


def _json_str(name: str, literal: str) -> dict[str, Any]:
    """A tool step whose ``.value`` is the plain string *literal* — a value
    source with no model call, no subprocess, and no adapter dependency."""
    return {
        "type": "tool",
        "name": name,
        "provider": "json",
        "params": {"mode": "parse", "input": f'"{literal}"'},
    }


def _combine(name: str, *refs: str) -> dict[str, Any]:
    """A tool step whose ``.value`` is a JSON object of the given refs, e.g.
    ``_combine("second", "prime.first.value", "prime.inner1.value")`` renders
    to ``{"prime.first.value": "C", "prime.inner1.value": "D"}``."""
    return {
        "type": "tool",
        "name": name,
        "provider": "json",
        "params": {
            "mode": "stringify",
            "input": {ref: f"{{{{{ref}}}}}" for ref in refs},
        },
    }


def _loop_with_branch(*, flow: str, cel_expr: str) -> dict[str, Any]:
    """The reported shape: loop body = [first, if(then=[inner1, second])]."""
    return {
        "effects": [
            {
                "type": "loop",
                "name": "lp",
                "flow": flow,
                "each": {"in": "input.items", "as": "it"},
                "collect": "second",
                "body": [
                    _json_str("first", "C"),
                    {
                        "type": "if",
                        "if": {"mode": "cel", "expr": cel_expr},
                        "then": [
                            _json_str("inner1", "D"),
                            _combine("second", "prime.first.value", "prime.inner1.value"),
                        ],
                        "else": [
                            _json_str("inner1", "D"),
                            _combine("second", "prime.first.value", "prime.inner1.value"),
                        ],
                    },
                ],
            }
        ]
    }


@pytest.mark.parametrize("flow", ["chain", "tree"])
@pytest.mark.parametrize(
    "cel_expr", ["true", "false"], ids=["then-branch", "else-branch"]
)
def test_branch_step_reads_its_sibling_in_a_loop_body(
    flow: str, cel_expr: str
) -> None:
    state = _run(_loop_with_branch(flow=flow, cel_expr=cel_expr), {"input": {"items": [1]}})

    second_value = state["prime"]["lp"]["iter_0"]["second"]["value"]
    assert '"prime.first.value": "C"' in second_value
    assert '"prime.inner1.value": "D"' in second_value


def test_cel_condition_inside_the_branch_sees_the_branch_earlier_step() -> None:
    """A conditional nested *inside* the branch must see the branch's own
    prior step too, not just the outer branch's steps seeing each other."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "lp",
                "each": {"in": "input.items", "as": "it"},
                "body": [
                    _json_str("first", "C"),
                    {
                        "type": "if",
                        "if": {"mode": "cel", "expr": "true"},
                        "then": [
                            _json_str("inner1", "D"),
                            {
                                "type": "if",
                                "name": "nested",
                                "if": {
                                    "mode": "cel",
                                    "expr": (
                                        "has(state.prime.inner1) && "
                                        "state.prime.inner1.value == 'D'"
                                    ),
                                },
                                "then": [_json_str("nested_hit", "yes")],
                                "else": [_json_str("nested_hit", "no")],
                            },
                        ],
                    },
                ],
            }
        ]
    }

    state = _run(orch, {"input": {"items": [1]}})

    iter_0 = state["prime"]["lp"]["iter_0"]
    nested = iter_0["nested"]
    assert nested["value"]["branch"] == "then"
    assert nested["nested_hit"]["value"] == "yes"
