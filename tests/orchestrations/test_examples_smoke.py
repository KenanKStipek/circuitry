from __future__ import annotations

import json
from pathlib import Path

import pytest

from circuitry.api import (
    inspect_orchestration,
    run_orchestration,
    validate_orchestration,
)
from circuitry.cli.config import CircuitryConfig

EXAMPLES_DIR = Path("orchestrations")
MANIFEST_PATH = EXAMPLES_DIR / "manifest.json"
CURATED_EXAMPLES = [
    "hello.yml",
    "dynamic_hello.yml",
    "conditional_example.yml",
    "loop_example.yml",
    "typed_prompt_example.yml",
    "reflector_v1.yml",
    "multi_primitive_story.yml",
]


def test_example_manifest_covers_curated_set() -> None:
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    listed = {entry["file"] for entry in manifest["orchestrations"]}
    assert listed == set(CURATED_EXAMPLES)


@pytest.mark.parametrize("name", CURATED_EXAMPLES)
def test_examples_validate(name: str) -> None:
    result = validate_orchestration(orchestration_path=EXAMPLES_DIR / name)
    assert result["ok"] is True
    assert result["errors"] == []


@pytest.mark.parametrize("name", CURATED_EXAMPLES)
def test_examples_inspect(name: str) -> None:
    summary = inspect_orchestration(orchestration_path=EXAMPLES_DIR / name)
    assert summary["effects_count"] >= 1
    assert isinstance(summary["effect_names"], list)


@pytest.mark.parametrize("name", CURATED_EXAMPLES)
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
