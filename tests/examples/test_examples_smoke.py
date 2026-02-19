from __future__ import annotations

from pathlib import Path

import pytest

from circuitry.api import run_orchestration, validate_orchestration
from circuitry.cli.config import CircuitryConfig

EXAMPLES_DIR = Path("examples")
REPRESENTATIVE_EXAMPLES = [
    "hello.yml",
    "dynamic_hello.yml",
    "conditional_example.yml",
    "loop_example.yml",
]


@pytest.mark.parametrize("name", REPRESENTATIVE_EXAMPLES)
def test_examples_validate(name: str) -> None:
    result = validate_orchestration(orchestration_path=EXAMPLES_DIR / name)
    assert result["ok"] is True
    assert result["errors"] == []


@pytest.mark.parametrize("name", REPRESENTATIVE_EXAMPLES)
def test_examples_dry_run_smoke(name: str) -> None:
    result = run_orchestration(
        orchestration_path=EXAMPLES_DIR / name,
        state={},
        dry_run=True,
        config=CircuitryConfig(default_adapter="ollama", default_model="phi3:mini"),
    )
    assert result.ok is True
    assert "runtime" in result.state
    assert "prime" in result.state
    assert result.state["runtime"]["last_run"]["completed_at"] is not None
