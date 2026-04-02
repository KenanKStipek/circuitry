"""Tests for the cof list CLI command."""

from __future__ import annotations

from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


def test_list_shows_orchestrations() -> None:
    result = runner.invoke(app, ["list"])
    assert result.exit_code == 0
    assert "hello" in result.output
    assert "article-summarizer" in result.output


def test_list_json_output() -> None:
    import json

    result = runner.invoke(app, ["list", "--json"])
    assert result.exit_code == 0
    data = json.loads(result.output)
    assert isinstance(data, list)
    names = [e["name"] for e in data]
    assert "hello" in names


def test_list_category_filter() -> None:
    result = runner.invoke(app, ["list", "--category", "example"])
    assert result.exit_code == 0
    assert "hello" in result.output
    # Templates should not appear when filtering by 'example'
    assert "template-prompt" not in result.output


def test_list_invalid_category() -> None:
    result = runner.invoke(app, ["list", "--category", "nonexistent"])
    assert result.exit_code == 1


def test_run_by_name_dry_run() -> None:
    result = runner.invoke(app, ["run", "hello", "--dry-run", "-e", "name=Test"])
    assert result.exit_code == 0


def test_run_nonexistent_name() -> None:
    result = runner.invoke(app, ["run", "does-not-exist"])
    assert result.exit_code == 1
    assert "not found" in result.output.lower()
