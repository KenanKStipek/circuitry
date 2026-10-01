"""Tests for cof setup command."""

from __future__ import annotations

import json
import re
import stat
from pathlib import Path

from typer.testing import CliRunner

from circuitry.cli import setup as cli_setup
from circuitry.cli.app import app

runner = CliRunner()


def _mode(path: Path) -> int:
    return stat.S_IMODE(path.stat().st_mode)


def _extract_json(output: str) -> list:
    """Extract the JSON array from setup --json output (skips Rich panel header)."""
    # Find the first '[' which starts the JSON array
    match = re.search(r"\[", output)
    assert match, f"No JSON array found in output: {output[:200]}"
    return json.loads(output[match.start():])


def test_setup_json_runs_without_error() -> None:
    """--json mode is non-interactive and should always work."""
    result = runner.invoke(app, ["setup", "--json"])
    assert result.exit_code == 0
    data = _extract_json(result.output)
    assert isinstance(data, list)
    names = {entry["name"] for entry in data}
    assert "ollama" in names
    assert "ffmpeg" in names


def test_setup_json_has_expected_fields() -> None:
    result = runner.invoke(app, ["setup", "--json"])
    assert result.exit_code == 0
    data = _extract_json(result.output)
    for entry in data:
        assert "name" in entry
        assert "available" in entry
        assert isinstance(entry["available"], bool)
        assert "detail" in entry
        assert "models" in entry
        assert isinstance(entry["models"], list)


# ---------------------------------------------------------------------------
# #264 part 4 — config.json / .env written private, directory created private
# ---------------------------------------------------------------------------


def test_write_config_creates_private_directory_and_file() -> None:
    config_path = cli_setup._write_config({"default_model": "m"})

    assert config_path == cli_setup.GLOBAL_CONFIG_PATH
    assert _mode(config_path) == 0o600
    assert _mode(config_path.parent) == 0o700
    assert json.loads(config_path.read_text()) == {"default_model": "m"}


def test_write_config_tightens_mode_of_existing_file() -> None:
    cli_setup.GLOBAL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    cli_setup.GLOBAL_CONFIG_PATH.write_text("{}", encoding="utf-8")
    cli_setup.GLOBAL_CONFIG_PATH.chmod(0o644)

    cli_setup._write_config({"default_model": "m"})

    assert _mode(cli_setup.GLOBAL_CONFIG_PATH) == 0o600


def test_write_env_file_creates_private_directory_and_file(
    monkeypatch: object,
) -> None:
    from circuitry.cli.detect import BackendStatus, DetectionResult

    monkeypatch.setattr("typer.confirm", lambda *a, **k: True)  # type: ignore[union-attr]
    monkeypatch.setattr("typer.prompt", lambda *a, **k: "sk-test")  # type: ignore[union-attr]

    result = DetectionResult(backends=[BackendStatus(name="openai", available=False, detail="")])
    env_path = cli_setup._write_env_file(result)

    assert env_path is not None
    assert _mode(env_path) == 0o600
    assert _mode(env_path.parent) == 0o700
    assert "OPENAI_API_KEY=sk-test" in env_path.read_text()


def test_write_env_file_tightens_mode_of_existing_file(monkeypatch: object) -> None:
    from circuitry.cli.detect import BackendStatus, DetectionResult

    cli_setup.GLOBAL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    env_path = cli_setup.GLOBAL_CONFIG_DIR / ".env"
    env_path.write_text("EXISTING=1\n", encoding="utf-8")
    env_path.chmod(0o644)

    monkeypatch.setattr("typer.confirm", lambda *a, **k: True)  # type: ignore[union-attr]
    monkeypatch.setattr("typer.prompt", lambda *a, **k: "sk-test")  # type: ignore[union-attr]

    result = DetectionResult(backends=[BackendStatus(name="openai", available=False, detail="")])
    cli_setup._write_env_file(result)

    assert _mode(env_path) == 0o600
    assert "EXISTING=1" in env_path.read_text()
