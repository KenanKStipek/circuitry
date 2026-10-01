"""`cof --version` (#269 item 3): only the `version` subcommand worked before."""

from __future__ import annotations

import re

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()

#: Rich force-colors its own help rendering under CI=true regardless of
#: CliRunner's color=False, so a substring check on raw output needs ANSI
#: stripped first.
_ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")


def test_version_flag_prints_version_and_exits_zero() -> None:
    result = runner.invoke(app, ["--version"])
    assert result.exit_code == 0, result.output
    assert result.output.startswith("Circuitry ")


def test_version_subcommand_still_works() -> None:
    result = runner.invoke(app, ["version"])
    assert result.exit_code == 0, result.output
    assert result.output.startswith("Circuitry ")


def test_version_flag_and_subcommand_agree() -> None:
    flag_result = runner.invoke(app, ["--version"])
    subcommand_result = runner.invoke(app, ["version"])
    assert flag_result.output == subcommand_result.output


def test_help_mentions_version_flag() -> None:
    result = runner.invoke(app, ["--help"])
    assert result.exit_code == 0
    assert "--version" in _ANSI_RE.sub("", result.output)
