"""Tests for cof setup command."""

from __future__ import annotations

import json
import re

from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


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
