from __future__ import annotations

from dataclasses import dataclass

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.diagnostics import find_divergence_paths
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass(frozen=True)
class FailingAdapter:
    name: str = "failing"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        raise RuntimeError("adapter offline")


def test_find_divergence_paths_returns_sorted_failure_records() -> None:
    state = {
        "prime": {
            "b": {"meta": {"error": "b failed", "created_at": "1"}},
            "a": {"meta": {"error": "a failed", "created_at": "2"}},
        }
    }

    records = find_divergence_paths(state)

    assert [r["path"] for r in records] == ["prime.a", "prime.b"]
    assert [r["error"] for r in records] == ["a failed", "b failed"]


def test_find_divergence_paths_captures_nested_runtime_breadcrumbs() -> None:
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "outer",
                "effects": [
                    {
                        "type": "dynamic",
                        "name": "inner",
                        "effects": [
                            {
                                "type": "prompt",
                                "name": "task",
                                "template": "go",
                            }
                        ],
                    }
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    store = Store({})
    with pytest.raises(RuntimeError):
        DynamicRuntime(root, adapter=FailingAdapter(), model="test").execute(store=store)

    records = find_divergence_paths(store.state)
    paths = {r["path"] for r in records}

    assert "prime.outer.inner.task" in paths
    assert "prime.outer.inner" in paths
    assert "prime.outer" in paths
    assert "prime" in paths

    root_error = next(r["error"] for r in records if r["path"] == "prime")
    assert "prime.outer" in root_error
