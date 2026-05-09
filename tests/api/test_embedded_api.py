from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry import (
    CircuitryExecutionError,
    inspect_divergence_paths,
    inspect_orchestration,
    run_orchestration,
    validate_orchestration,
)
from circuitry.cli.app import app
from circuitry.cli.config import resolve_config

runner = CliRunner()


def _write(tmp_path: Path, name: str, content: str) -> Path:
    path = tmp_path / name
    path.write_text(content, encoding="utf-8")
    return path


def _shape_paths(value: Any, prefix: str = "") -> set[str]:
    paths: set[str] = set()
    if isinstance(value, dict):
        for key, child in value.items():
            current = f"{prefix}.{key}" if prefix else str(key)
            paths.add(current)
            paths.update(_shape_paths(child, current))
    elif isinstance(value, list):
        for idx, child in enumerate(value):
            current = f"{prefix}[{idx}]"
            paths.add(current)
            paths.update(_shape_paths(child, current))
    return paths


def test_cli_and_embedded_api_state_shape_match_for_same_orchestration(
    tmp_path: Path,
) -> None:
    orch = _write(
        tmp_path,
        "orch.yml",
        """
adapter: ollama
model: phi3:mini
effects:
  - type: prompt
    name: hello
    template: "hello"
""".strip()
        + "\n",
    )
    cli_out = tmp_path / "cli.json"

    cli_result = runner.invoke(
        app,
        ["run", str(orch), "--dry-run", "--out", str(cli_out)],
    )
    assert cli_result.exit_code == 0
    cli_state = json.loads(cli_out.read_text(encoding="utf-8"))
    # ``cof run`` builds its CircuitryConfig via the layered
    # ``resolve_config`` (sane defaults + global + project + env), but
    # ``load_config`` only reads the file. Use ``resolve_config`` here
    # so the embedded API call sees the same shape as the CLI did.
    cfg = resolve_config(explicit_path=None)

    api_result = run_orchestration(
        orchestration_path=orch,
        state={},
        dry_run=True,
        config=cfg,
    )
    assert api_result.ok is True

    # Timestamps differ, so compare structure/path shape and key metadata semantics.
    assert _shape_paths(cli_state) == _shape_paths(api_result.state)
    assert "prime.hello" in _shape_paths(api_result.state)
    assert (
        cli_state["runtime"]["effective_settings"]
        == api_result.state["runtime"]["effective_settings"]
    )


def test_embedded_api_raises_actionable_error_with_result_context(
    tmp_path: Path,
) -> None:
    orch = _write(
        tmp_path,
        "bad.yml",
        """
adapter: ollama
model: phi3:mini
effects:
  - type: prompt
    name: dup
    template: "a"
  - type: prompt
    name: dup
    template: "b"
""".strip()
        + "\n",
    )

    with pytest.raises(CircuitryExecutionError) as exc:
        run_orchestration(orchestration_path=orch, dry_run=True)

    err = str(exc.value)
    assert "Duplicate effect name 'dup'" in err
    assert exc.value.result.ok is False
    assert exc.value.result.state["runtime"]["last_run"]["completed_at"] is not None


def test_embedded_validate_and_inspect_match_expected_shape(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "ok.yml",
        """
effects:
  - type: prompt
    name: greet
    template: "hello"
""".strip()
        + "\n",
    )

    validation = validate_orchestration(orchestration_path=orch)
    assert validation["ok"] is True

    summary = inspect_orchestration(orchestration_path=orch)
    assert summary["effects_count"] == 1
    assert summary["effect_names"] == ["greet"]


def test_inspect_divergence_paths_extracts_failures_deterministically() -> None:
    state = {
        "prime": {
            "z": {"meta": {"error": "z failed"}},
            "a": {"meta": {"error": "a failed"}},
        }
    }
    records = inspect_divergence_paths(state=state)
    assert [r["path"] for r in records] == ["prime.a", "prime.z"]
