"""`cof wizard` — the headless CLI entry point for the same wizard the TUI
Chat view drives.

These tests exercise argument handling, the re-validation gate, and the two
ways a draft can leave (``--out``, ``--library``) with a fake turn runner —
no LLM, no textual. Artefact equivalence with the TUI chat view is asserted
separately, over the real orchestration, in
``tests/tui/test_chat_view.py::test_cli_wizard_matches_the_chat_views_artifact``.
"""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import patch

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.cli.config import CircuitryConfig
from circuitry.tui.wizard_host import Turn

runner = CliRunner()

#: A minimal but genuinely schema-valid draft — the same gate `cof check` runs
#: has to accept it, since `save_to_file`/`save_to_library` re-validate.
VALID_DRAFT = 'effects:\n  - type: prompt\n    name: greet\n    template: "Hello"\n'

#: Structurally invalid — `iter_0` is a reserved loop-segment name.
INVALID_DRAFT = 'effects:\n  - type: prompt\n    name: iter_0\n    template: "no"\n'


def _fake_turns(*turns: Turn):
    """A `run_turn`-shaped fake that hands back ``turns`` in order."""
    remaining = iter(turns)

    def _run(state, *, config=None, verbose=False):
        return next(remaining)

    return _run


def test_wizard_writes_a_valid_draft_to_out(tmp_path: Path) -> None:
    out = tmp_path / "greeting.yml"
    fake = _fake_turns(Turn(say="Built it.", yaml=VALID_DRAFT, done=True))
    with patch("circuitry.cli.app.run_turn", fake), patch(
        "circuitry.cli.app.resolve_config", return_value=CircuitryConfig()
    ):
        result = runner.invoke(app, ["wizard", "--goal", "Greet someone", "--out", str(out)])

    assert result.exit_code == 0, result.output
    assert out.read_text(encoding="utf-8") == VALID_DRAFT


def test_wizard_prints_the_draft_to_stdout_by_default() -> None:
    fake = _fake_turns(Turn(say="Built it.", yaml=VALID_DRAFT, done=True))
    with patch("circuitry.cli.app.run_turn", fake), patch(
        "circuitry.cli.app.resolve_config", return_value=CircuitryConfig()
    ):
        result = runner.invoke(app, ["wizard", "--goal", "Greet someone"])

    assert result.exit_code == 0, result.output
    assert VALID_DRAFT.strip() in result.output


def test_wizard_refuses_to_write_an_invalid_final_draft(tmp_path: Path) -> None:
    out = tmp_path / "bad.yml"
    fake = _fake_turns(Turn(say="Shipped it.", yaml=INVALID_DRAFT, done=True, valid=True))
    with patch("circuitry.cli.app.run_turn", fake), patch(
        "circuitry.cli.app.resolve_config", return_value=CircuitryConfig()
    ):
        result = runner.invoke(app, ["wizard", "--goal", "Greet someone", "--out", str(out)])

    assert result.exit_code == 1
    assert not out.exists()
    assert "did not pass validation" in result.output


def test_wizard_reports_when_no_draft_is_ever_produced() -> None:
    fake = _fake_turns(Turn(say="What should it output?"))
    with patch("circuitry.cli.app.run_turn", fake), patch(
        "circuitry.cli.app.resolve_config", return_value=CircuitryConfig()
    ):
        # No --reply and empty stdin: the conversation runs dry after one turn.
        result = runner.invoke(app, ["wizard", "--goal", "Greet someone"], input="")

    assert result.exit_code == 1
    assert "No orchestration was produced" in result.output


def test_wizard_asks_for_a_valid_seed_before_running_a_turn() -> None:
    result = runner.invoke(app, ["wizard", "--goal", "  ", "--category", "not-a-category"])
    assert result.exit_code == 1
    assert "Category must be one of" in result.output


def test_wizard_reads_multi_turn_replies_from_a_file(tmp_path: Path) -> None:
    seen_states: list[dict] = []
    turns = iter(
        [
            Turn(say="Which language?"),
            Turn(say="Built it.", yaml=VALID_DRAFT, done=True),
        ]
    )

    def fake_run_turn(state, *, config=None, verbose=False):
        seen_states.append(state)
        return next(turns)

    replies = tmp_path / "answers.txt"
    replies.write_text("French.\n", encoding="utf-8")
    out = tmp_path / "out.yml"

    with patch("circuitry.cli.app.run_turn", fake_run_turn), patch(
        "circuitry.cli.app.resolve_config", return_value=CircuitryConfig()
    ):
        result = runner.invoke(
            app,
            [
                "wizard",
                "--goal",
                "Greet someone",
                "--reply",
                str(replies),
                "--out",
                str(out),
            ],
        )

    assert result.exit_code == 0, result.output
    assert out.read_text(encoding="utf-8") == VALID_DRAFT
    assert seen_states[1]["conversation"] == [
        {"role": "wizard", "content": "Which language?"},
        {"role": "user", "content": "French."},
    ]


def test_wizard_reads_replies_from_redirected_stdin_with_no_tty(tmp_path: Path) -> None:
    """No --reply, stdin piped: CliRunner never provides a TTY either way."""
    turns = iter(
        [
            Turn(say="Which language?"),
            Turn(say="Built it.", yaml=VALID_DRAFT, done=True),
        ]
    )
    fake = lambda state, *, config=None, verbose=False: next(turns)  # noqa: E731
    out = tmp_path / "out.yml"

    with patch("circuitry.cli.app.run_turn", fake), patch(
        "circuitry.cli.app.resolve_config", return_value=CircuitryConfig()
    ):
        result = runner.invoke(
            app,
            ["wizard", "--goal", "Greet someone", "--out", str(out)],
            input="French.\n",
        )

    assert result.exit_code == 0, result.output
    assert out.read_text(encoding="utf-8") == VALID_DRAFT


def test_wizard_saves_to_the_library(tmp_path: Path) -> None:
    fake = _fake_turns(Turn(say="Built it.", yaml=VALID_DRAFT, done=True))
    config = CircuitryConfig(
        runtime={"library": {"sources": [{"type": "folder", "path": str(tmp_path)}]}}
    )
    with patch("circuitry.cli.app.run_turn", fake), patch(
        "circuitry.cli.app.resolve_config", return_value=config
    ):
        result = runner.invoke(
            app,
            [
                "wizard",
                "--goal",
                "Greet someone",
                "--name",
                "Greeter",
                "--category",
                "recipes",
                "--library",
            ],
        )

    assert result.exit_code == 0, result.output
    saved = tmp_path / "recipes" / "greeter.yml"
    assert saved.read_text(encoding="utf-8") == VALID_DRAFT
    manifest = json.loads((tmp_path / "manifest.json").read_text(encoding="utf-8"))
    assert manifest["entries"][0]["name"] == "recipes/greeter"


def test_wizard_library_save_refuses_an_invalid_draft(tmp_path: Path) -> None:
    fake = _fake_turns(Turn(say="Shipped it.", yaml=INVALID_DRAFT, done=True, valid=True))
    config = CircuitryConfig(
        runtime={"library": {"sources": [{"type": "folder", "path": str(tmp_path)}]}}
    )
    with patch("circuitry.cli.app.run_turn", fake), patch(
        "circuitry.cli.app.resolve_config", return_value=config
    ):
        result = runner.invoke(
            app, ["wizard", "--goal", "Greet someone", "--library"]
        )

    assert result.exit_code == 1
    assert not (tmp_path / "manifest.json").exists()


def test_wizard_help_mentions_it_is_not_gen() -> None:
    result = runner.invoke(app, ["wizard", "--help"])
    assert result.exit_code == 0
    assert "cof gen" in result.output


def test_gen_help_mentions_wizard() -> None:
    result = runner.invoke(app, ["gen", "--help"])
    assert result.exit_code == 0
    assert "wizard" in result.output
