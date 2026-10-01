"""`termination.detail` on the each-loop bounds-check error path.
Regression suite for #304.

Guidebook ch.7 says of the ``error`` termination reason: "the loop failed;
``termination.detail`` and ``meta.error`` say why". That already held when a
pass itself raised (the ``except`` path in ``core/loop.py`` writes
``termination.detail``). It did not hold when an ``each`` collection is
longer than ``max_iterations`` under ``on_error: break``/``continue`` (#297):
the loop recorded ``termination: {reason: error}`` with no ``detail``, only
``meta.error``.
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

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        return GenerateResult(text=prompt, raw={"model": model})


def _bounds_orch(on_error: str) -> dict:
    return {
        "effects": [
            {
                "type": "loop",
                "name": "work",
                "max_iterations": 2,
                "on_error": on_error,
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "step", "template": "{{item}}"},
                ],
            },
        ]
    }


def test_bounds_check_break_writes_termination_detail() -> None:
    state = {"input": {"items": ["a", "b", "c"]}}
    root = compile_orchestration(orch=_bounds_orch("break"), root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=Store(state))

    node = state["prime"]["work"]
    termination = node["value"]["termination"]
    assert termination["reason"] == "error"
    assert "detail" in termination
    assert termination["detail"] == node["meta"]["error"]
    assert "3 items" in termination["detail"]
    assert "max_iterations is 2" in termination["detail"]


def test_bounds_check_continue_writes_termination_detail() -> None:
    state = {"input": {"items": ["a", "b", "c"]}}
    root = compile_orchestration(orch=_bounds_orch("continue"), root_name="prime")
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=Store(state))

    node = state["prime"]["work"]
    termination = node["value"]["termination"]
    assert termination["reason"] == "error"
    assert termination["detail"] == node["meta"]["error"]


def test_bounds_check_fail_still_raises_with_detail_via_outer_catch() -> None:
    """``on_error: fail`` (default) already worked before this fix — the
    bounds error raises and the function-level ``except`` writes
    ``termination.detail`` from ``str(e)``. This pins that it still does."""
    import pytest

    state = {"input": {"items": ["a", "b", "c"]}}
    root = compile_orchestration(orch=_bounds_orch("fail"), root_name="prime")
    with pytest.raises(Exception, match="max_iterations is 2"):
        DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=Store(state))

    node = state["prime"]["work"]
    termination = node["value"]["termination"]
    assert termination["reason"] == "error"
    assert "detail" in termination
