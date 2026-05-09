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

EXAMPLES_DIR = Path("src/circuitry/curation")
MANIFEST_PATH = EXAMPLES_DIR / "manifest.json"


def _curated_files() -> list[Path]:
    """All YAML files anywhere under the curation tree, sorted for stable order."""
    return sorted(p for p in EXAMPLES_DIR.rglob("*.yml") if p.is_file())


CURATED_EXAMPLES = [str(p.relative_to(EXAMPLES_DIR)) for p in _curated_files()]


def test_example_manifest_covers_curated_set() -> None:
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    listed = {entry["file"] for entry in manifest["entries"]}
    on_disk = set(CURATED_EXAMPLES)
    assert listed == on_disk, (
        f"Manifest/disk mismatch.\n"
        f"  in manifest only: {listed - on_disk}\n"
        f"  on disk only: {on_disk - listed}"
    )


@pytest.mark.parametrize("rel", CURATED_EXAMPLES)
def test_examples_validate(rel: str) -> None:
    result = validate_orchestration(orchestration_path=EXAMPLES_DIR / rel)
    assert result["ok"] is True, result.get("errors")
    assert result["errors"] == []


@pytest.mark.parametrize("rel", CURATED_EXAMPLES)
def test_examples_inspect(rel: str) -> None:
    summary = inspect_orchestration(orchestration_path=EXAMPLES_DIR / rel)
    assert summary["effects_count"] >= 1
    assert isinstance(summary["effect_names"], list)


@pytest.mark.parametrize("rel", CURATED_EXAMPLES)
def test_examples_dry_run_smoke(rel: str) -> None:
    result = run_orchestration(
        orchestration_path=EXAMPLES_DIR / rel,
        state={},
        dry_run=True,
        config=CircuitryConfig(default_adapter="ollama", default_model="phi3:mini"),
    )
    assert result.ok is True
    assert "runtime" in result.state
    assert "prime" in result.state
    assert result.state["runtime"]["last_run"]["completed_at"] is not None
