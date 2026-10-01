"""Tree loop `on_error: fail` must report the lowest-index failure, with its
iteration number, not whichever pass happens to finish first (#269 item 7).

Before this fix, ``raise next(iter(errors.values()))`` re-raised the first
exception *inserted into the errors dict*, which follows ``as_completed``'s
completion order — the fastest failing thread, not the lowest ``each``
index — and the raised message never named which iteration failed at all.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from typing import Any

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class EchoAdapter:
    """Echoes a prompt back; one containing BOOM raises. SLOW sleeps first,
    so a slow-but-lowest-index failure doesn't win by completing first."""

    name: str = "echo"
    prompts: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.prompts.append(prompt)
        if "SLOW" in prompt:
            time.sleep(0.2)
        if "BOOM" in prompt:
            raise RuntimeError(f"scripted failure: {prompt}")
        return GenerateResult(text=prompt, raw={"model": model})


def _tree_fail_orch() -> dict[str, Any]:
    return {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "flow": "tree",
                "on_error": "fail",
                "each": {"in": "input.items", "as": "item"},
                "body": [{"type": "prompt", "name": "step", "template": "{{item}}"}],
            }
        ]
    }


def test_tree_fail_raises_the_lowest_index_failure_not_the_fastest() -> None:
    # Index 0 fails slowly; indices 1 and 2 fail immediately and would win
    # a "first to complete" race.
    items = ["SLOW BOOM index0", "BOOM index1", "BOOM index2"]
    root = compile_orchestration(orch=_tree_fail_orch(), root_name="prime")
    state: dict[str, Any] = {"input": {"items": items}}

    with pytest.raises(Exception) as exc_info:
        DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
            store=Store(state)
        )

    message = str(exc_info.value)
    assert "index0" in message
    assert "index1" not in message
    assert "index2" not in message


def test_tree_fail_names_the_iteration_index_in_the_message() -> None:
    items = ["a", "BOOM index1", "c"]
    root = compile_orchestration(orch=_tree_fail_orch(), root_name="prime")
    state: dict[str, Any] = {"input": {"items": items}}

    with pytest.raises(Exception) as exc_info:
        DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
            store=Store(state)
        )

    message = str(exc_info.value)
    assert "[1]" in message or "index 1" in message or "iteration 1" in message
