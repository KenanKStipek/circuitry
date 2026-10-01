"""Tests for the cof list CLI command."""

from __future__ import annotations

from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


def test_list_shows_orchestrations() -> None:
    result = runner.invoke(app, ["list"])
    assert result.exit_code == 0
    # Default-width terminal truncates long names with an ellipsis, so check
    # short, unambiguous substrings present in any column.
    assert "hello" in result.output
    assert "recipes" in result.output


def test_list_json_output() -> None:
    import json

    result = runner.invoke(app, ["list", "--json"])
    assert result.exit_code == 0
    data = json.loads(result.output)
    assert isinstance(data, list)
    names = [e["name"] for e in data]
    assert "learn/hello" in names


def test_list_category_filter() -> None:
    result = runner.invoke(app, ["list", "--category", "learn"])
    assert result.exit_code == 0
    assert "hello" in result.output
    # Recipes should not appear when filtering by 'learn'.
    assert "article_summarizer" not in result.output


def test_list_invalid_category() -> None:
    result = runner.invoke(app, ["list", "--category", "nonexistent"])
    assert result.exit_code == 1


def test_list_category_help_names_real_categories() -> None:
    """The help text used to name five categories (example, utility,
    creative, tooling, template) that match none of the real ones
    (#265 part 5)."""
    result = runner.invoke(app, ["list", "--help"])
    assert result.exit_code == 0
    for fake in ("example", "creative", "tooling", "template"):
        assert fake not in result.output
    assert "agents" in result.output
    assert "learn" in result.output


def test_list_category_utilities_matches_the_real_category() -> None:
    result = runner.invoke(app, ["list", "--category", "utilities"])
    assert result.exit_code == 0


def test_run_by_name_dry_run() -> None:
    result = runner.invoke(app, ["run", "learn/hello", "--dry-run", "-e", "name=Test"])
    assert result.exit_code == 0


def test_run_nonexistent_name() -> None:
    result = runner.invoke(app, ["run", "does-not-exist"])
    assert result.exit_code == 1
    assert "not found" in result.output.lower()
