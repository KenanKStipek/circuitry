"""CLI wiring: bare ``cof`` gating and the ``cof tui`` subcommand."""

from __future__ import annotations

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry import tui
from circuitry.cli.app import app

runner = CliRunner()


def test_bare_cof_without_tty_is_unchanged(monkeypatch: pytest.MonkeyPatch) -> None:
    """Piped/redirected invocation keeps Click's ``Missing command.`` usage error."""
    monkeypatch.setattr(tui, "should_launch_tui", lambda: False)
    called: list[int] = []
    monkeypatch.setattr(tui, "run_tui", lambda: called.append(1))

    result = runner.invoke(app, [])

    assert result.exit_code == 2
    assert "Missing command." in result.output
    assert called == []


def test_bare_cof_on_tty_launches_the_app(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(tui, "should_launch_tui", lambda: True)
    called: list[int] = []
    monkeypatch.setattr(tui, "run_tui", lambda: called.append(1))

    result = runner.invoke(app, [])

    assert result.exit_code == 0, result.output
    assert called == [1]


def test_subcommands_never_launch_the_app(monkeypatch: pytest.MonkeyPatch) -> None:
    """The gate must not fire when a subcommand was requested, TTY or not."""
    monkeypatch.setattr(tui, "should_launch_tui", lambda: True)
    called: list[int] = []
    monkeypatch.setattr(tui, "run_tui", lambda: called.append(1))

    result = runner.invoke(app, ["version"])

    assert result.exit_code == 0, result.output
    assert "Circuitry" in result.output
    assert called == []


def test_tui_command_forces_the_app_without_a_tty(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(tui, "should_launch_tui", lambda: False)
    called: list[int] = []
    monkeypatch.setattr(tui, "run_tui", lambda: called.append(1))

    result = runner.invoke(app, ["tui"])

    assert result.exit_code == 0, result.output
    assert called == [1]


def test_tui_command_without_extra_prints_install_hint(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(tui, "textual_available", lambda: False)

    def _should_not_run() -> None:  # pragma: no cover - guard
        raise AssertionError("run_tui called without the extra installed")

    monkeypatch.setattr(tui, "run_tui", _should_not_run)

    result = runner.invoke(app, ["tui"])

    assert result.exit_code == 1
    assert "circuitry-cof[tui]" in result.output


def test_tui_command_is_listed_in_help() -> None:
    result = runner.invoke(app, ["--help"])

    assert result.exit_code == 0, result.output
    assert "tui" in result.output
