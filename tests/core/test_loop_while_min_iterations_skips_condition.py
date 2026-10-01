"""A `while` loop must not evaluate its condition before a pass that
`min_iterations` already forces (#298): the condition's answer would be
discarded anyway, so running it only spends a model call or logs a warning
about state the forced pass hasn't written yet.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class EchoAdapter:
    name: str = "echo"
    calls: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append(prompt)
        return GenerateResult(text="no", raw={"model": model})


def test_forced_pass_reading_unset_state_logs_no_warning(caplog: pytest.LogCaptureFixture) -> None:
    """The exact #298 repro: a `mode: cel` condition reading state the first
    (forced) pass sets must not warn about an unset path before that pass
    has run, and the loop still runs the documented 3 passes."""
    orch = {
        "effects": [
            {"type": "tool", "name": "passmark", "provider": "json", "params": {"mode": "parse", "input": "5"}},
            {
                "type": "loop",
                "name": "attempts",
                "collect": "verdict",
                "min_iterations": 1,
                "max_iterations": 6,
                "while": {
                    "mode": "cel",
                    "expr": "state.prime.verdict.value < state.prime.passmark.value && state.iter.index + 1 < 3",
                },
                "body": [
                    {"type": "tool", "name": "verdict", "provider": "json", "params": {"mode": "parse", "input": "1"}},
                ],
            },
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict = {"input": {}}
    with caplog.at_level(logging.WARNING):
        DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
            store=Store(state)
        )

    unset_path_warnings = [
        r for r in caplog.records if "reads unset state path" in r.getMessage()
    ]
    assert not unset_path_warnings
    assert state["prime"]["attempts"]["value"]["iterations"] == 3


def test_forced_pass_makes_no_model_call_for_a_model_mode_condition() -> None:
    """A `mode: model` condition with `min_iterations: 1` must not call the
    adapter before the first (forced) pass. `max_iterations: 2` leaves room
    for the condition to be checked once, after that pass — the adapter
    always answers 'no', so the loop stops there with exactly one pass."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "min_iterations": 1,
                "max_iterations": 2,
                "while": {"mode": "model", "template": "Should we continue?"},
                "body": [{"type": "prompt", "name": "step", "template": "pass"}],
            },
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    state: dict = {"input": {}}
    adapter = EchoAdapter()
    DynamicRuntime(root, adapter=adapter, model="unit-test").execute(store=Store(state))

    condition_calls = [c for c in adapter.calls if "Should the loop continue?" in c]
    assert len(condition_calls) == 1
    assert state["prime"]["spin"]["value"]["iterations"] == 1
