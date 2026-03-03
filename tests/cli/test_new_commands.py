from __future__ import annotations

import json
from pathlib import Path

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


def _write(tmp_path: Path, name: str, content: str) -> Path:
    path = tmp_path / name
    path.write_text(content, encoding="utf-8")
    return path


VALID_ORCH = """\
effects:
  - type: prompt
    name: greet
    template: "Hello {{name}}"
"""

INVALID_ORCH = """\
adapter: ollama
model: llama3
"""


# ---------------------------------------------------------------------------
# cof check
# ---------------------------------------------------------------------------


def test_check_valid_orchestration(tmp_path: Path):
    orch = _write(tmp_path, "valid.yml", VALID_ORCH)
    result = runner.invoke(app, ["check", str(orch)])
    assert result.exit_code == 0
    assert "Valid" in result.stdout


def test_check_invalid_orchestration(tmp_path: Path):
    orch = _write(tmp_path, "bad.yml", INVALID_ORCH)
    result = runner.invoke(app, ["check", str(orch)])
    assert result.exit_code == 1
    assert "Invalid" in result.stdout


def test_check_json_output(tmp_path: Path):
    orch = _write(tmp_path, "valid.yml", VALID_ORCH)
    result = runner.invoke(app, ["check", str(orch), "--json"])
    assert result.exit_code == 0
    parsed = json.loads(result.stdout)
    assert parsed["ok"] is True


# ---------------------------------------------------------------------------
# cof validate (same as check)
# ---------------------------------------------------------------------------


def test_validate_matches_check(tmp_path: Path):
    orch = _write(tmp_path, "valid.yml", VALID_ORCH)
    check_result = runner.invoke(app, ["check", str(orch), "--json"])
    validate_result = runner.invoke(app, ["validate", str(orch), "--json"])
    assert check_result.exit_code == validate_result.exit_code
    assert json.loads(check_result.stdout)["ok"] == json.loads(validate_result.stdout)["ok"]


# ---------------------------------------------------------------------------
# cof version
# ---------------------------------------------------------------------------


def test_version_prints_string():
    result = runner.invoke(app, ["version"])
    assert result.exit_code == 0
    assert "circuitry" in result.stdout.lower()


# ---------------------------------------------------------------------------
# cof init
# ---------------------------------------------------------------------------


def test_init_creates_files(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.chdir(tmp_path)
    result = runner.invoke(app, ["init"], input="ollama\nhttp://localhost:11434\nllama3.1:8b\n")
    assert result.exit_code == 0

    config_path = tmp_path / "circuitry.config.json"
    hello_path = tmp_path / "hello.yml"
    assert config_path.exists()
    assert hello_path.exists()

    config_data = json.loads(config_path.read_text(encoding="utf-8"))
    assert config_data["default_model"] == "llama3.1:8b"
    assert config_data["default_adapter"] == "ollama"


def test_init_aborts_if_config_exists(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.chdir(tmp_path)
    (tmp_path / "circuitry.config.json").write_text("{}", encoding="utf-8")
    result = runner.invoke(app, ["init"])
    assert result.exit_code == 1
    assert "already exists" in result.stdout


# ---------------------------------------------------------------------------
# cof run --last (with no previous run)
# ---------------------------------------------------------------------------


def test_run_last_fails_without_previous(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    from circuitry.cli import app as app_module

    # Point _LAST_RUN_PATH to a non-existent file
    monkeypatch.setattr(app_module, "_LAST_RUN_PATH", tmp_path / "no-last.json")
    result = runner.invoke(app, ["run", "--last"])
    assert result.exit_code != 0
