from __future__ import annotations

import json
from pathlib import Path

import pytest

from circuitry.cli.runtime_shim import inspect_orchestration

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


def _write(tmp_path: Path, name: str, content: str) -> Path:
    path = tmp_path / name
    path.write_text(content, encoding="utf-8")
    return path


def test_inspect_uses_effects_and_populates_alias_fields(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "orch.yml",
        """
effects:
  - type: prompt
    name: first
    template: "a"
  - type: prompt
    name: second
    template: "b"
""".strip()
        + "\n",
    )

    summary = inspect_orchestration(orch)
    assert summary["effects_count"] == 2
    assert summary["effect_names"] == ["first", "second"]
    assert summary["steps_count"] == 2
    assert summary["step_names"] == ["first", "second"]


def test_run_writes_state_file_on_failure_when_out_is_set(tmp_path: Path) -> None:
    # Duplicate sibling names fail validation/compile.
    orch = _write(
        tmp_path,
        "bad.yml",
        """
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
    out = tmp_path / "state.json"

    result = runner.invoke(app, ["run", str(orch), "--out", str(out)])

    assert result.exit_code == 1
    assert out.exists()

    state = json.loads(out.read_text(encoding="utf-8"))
    assert "runtime" in state
    assert "last_run" in state["runtime"]
    assert state["runtime"]["last_run"]["completed_at"] is not None
    assert "Duplicate effect name 'dup'" in result.stdout
    assert "State written:" in result.stdout
