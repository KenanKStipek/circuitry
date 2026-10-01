"""`cof check` warns when a field is schema-valid but has no effect where it
was set (#251 part 2): `min_iterations` on an `each` loop, and `if.threshold`
anywhere.
"""

from __future__ import annotations

from circuitry.core.lint import lint_orchestration


def test_min_iterations_on_each_loop_is_warned() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "l",
                "each": {"in": "input.items"},
                "min_iterations": 5,
                "body": [{"type": "prompt", "name": "s", "template": "x"}],
            },
        ]
    }
    warnings = lint_orchestration(orch)
    assert len(warnings) == 1
    assert "effects[0]" in warnings[0]
    assert "'min_iterations' has no effect on an 'each' loop" in warnings[0]


def test_min_iterations_on_while_loop_is_not_warned() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "l",
                "while": {"mode": "cel", "expr": "true"},
                "min_iterations": 1,
                "max_iterations": 2,
                "body": [{"type": "prompt", "name": "s", "template": "x"}],
            },
        ]
    }
    assert lint_orchestration(orch) == []


def test_if_threshold_is_always_warned() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "name": "c",
                "if": {"mode": "cel", "expr": "true"},
                "then": [{"type": "prompt", "name": "t", "template": "y"}],
                "threshold": 0.7,
            },
        ]
    }
    warnings = lint_orchestration(orch)
    assert len(warnings) == 1
    assert "'threshold' has no effect" in warnings[0]


def test_if_without_threshold_is_not_warned() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "name": "c",
                "if": {"mode": "cel", "expr": "true"},
                "then": [{"type": "prompt", "name": "t", "template": "y"}],
            },
        ]
    }
    assert lint_orchestration(orch) == []
