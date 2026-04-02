"""Tests for cof info and cof eject commands."""

from __future__ import annotations

import json
from pathlib import Path

from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.cli.registry import find_entry

runner = CliRunner()


# ── find_entry ───────────────────────────────────────────────────────────────


def test_find_entry_by_name() -> None:
    entry = find_entry("hello")
    assert entry is not None
    assert entry["name"] == "hello"


def test_find_entry_hyphenated() -> None:
    entry = find_entry("article-summarizer")
    assert entry is not None
    assert entry["file"] == "article_summarizer.yml"


def test_find_entry_not_found() -> None:
    assert find_entry("nonexistent") is None


# ── cof info ─────────────────────────────────────────────────────────────────


def test_info_shows_details() -> None:
    result = runner.invoke(app, ["info", "hello"])
    assert result.exit_code == 0
    assert "hello" in result.output
    assert "name" in result.output.lower()
    assert "example" in result.output.lower()


def test_info_json() -> None:
    result = runner.invoke(app, ["info", "hello", "--json"])
    assert result.exit_code == 0
    data = json.loads(result.output)
    assert data["name"] == "hello"
    assert "inputs" in data


def test_info_not_found() -> None:
    result = runner.invoke(app, ["info", "nonexistent"])
    assert result.exit_code == 1


# ── cof eject ────────────────────────────────────────────────────────────────


def test_eject_creates_file(tmp_path: Path) -> None:
    dest = tmp_path / "hello.yml"
    result = runner.invoke(app, ["eject", "hello", "--out", str(dest)])
    assert result.exit_code == 0
    assert dest.exists()
    content = dest.read_text()
    assert "effects:" in content


def test_eject_not_found() -> None:
    result = runner.invoke(app, ["eject", "nonexistent"])
    assert result.exit_code == 1
