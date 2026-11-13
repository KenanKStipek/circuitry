"""`cof run`'s label: the document's own top-level `label:` template, or
`--label` (#752) — the CLI entry point. `core/test_label.py` covers the
clean/render functions directly; this covers wiring: precedence, where it
lands (`--out`'s `runtime.last_run.label`, `--events`' `run_start`), resume,
and `use` children never contributing one.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


def _write(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def _last_run(out: Path) -> dict:
    return json.loads(out.read_text(encoding="utf-8"))["runtime"]["last_run"]


def _tool_step(name: str = "t") -> str:
    return (
        f"  - type: tool\n    name: {name}\n    provider: json\n"
        "    params:\n      input: '1'\n"
    )


def test_label_rendered_from_an_input(tmp_path: Path) -> None:
    orch = _write(
        tmp_path / "doc.yml",
        "label: 'ticket {{input.ticket}}'\n"
        "interface:\n  inputs:\n    ticket: {type: string, default: '42'}\n"
        "effects:\n" + _tool_step(),
    )
    out = tmp_path / "out.json"
    result = runner.invoke(app, ["run", str(orch), "--out", str(out), "--quiet"])
    assert result.exit_code == 0, result.output
    assert _last_run(out)["label"] == "ticket 42"


def test_no_label_key_when_document_has_none(tmp_path: Path) -> None:
    orch = _write(tmp_path / "doc.yml", "effects:\n" + _tool_step())
    out = tmp_path / "out.json"
    result = runner.invoke(app, ["run", str(orch), "--out", str(out), "--quiet"])
    assert result.exit_code == 0, result.output
    assert "label" not in _last_run(out)


def test_missing_input_renders_empty_no_label_no_warning(tmp_path: Path) -> None:
    orch = _write(
        tmp_path / "doc.yml",
        "label: '{{input.ticket}}'\neffects:\n" + _tool_step(),
    )
    out = tmp_path / "out.json"
    result = runner.invoke(app, ["run", str(orch), "--out", str(out), "--quiet"])
    assert result.exit_code == 0, result.output
    assert "label" not in _last_run(out)
    assert "Warning:" not in result.output


def test_cli_label_wins_over_document_label(tmp_path: Path) -> None:
    orch = _write(
        tmp_path / "doc.yml",
        "label: 'from the document'\neffects:\n" + _tool_step(),
    )
    out = tmp_path / "out.json"
    result = runner.invoke(
        app, ["run", str(orch), "--out", str(out), "--label", "from the flag", "--quiet"]
    )
    assert result.exit_code == 0, result.output
    assert _last_run(out)["label"] == "from the flag"


def test_cli_label_literal_text_not_rendered(tmp_path: Path) -> None:
    orch = _write(tmp_path / "doc.yml", "effects:\n" + _tool_step())
    out = tmp_path / "out.json"
    result = runner.invoke(
        app,
        ["run", str(orch), "--out", str(out), "--label", "{{input.ticket}}", "--quiet"],
    )
    assert result.exit_code == 0, result.output
    assert _last_run(out)["label"] == "{{input.ticket}}"


def test_malformed_label_template_warns_once_and_run_succeeds(tmp_path: Path) -> None:
    orch = _write(
        tmp_path / "doc.yml",
        "label: '{{#open'\neffects:\n" + _tool_step(),
    )
    out = tmp_path / "out.json"
    result = runner.invoke(app, ["run", str(orch), "--out", str(out)])
    assert result.exit_code == 0, result.output
    assert "label" not in _last_run(out)
    assert result.output.count("Warning:") == 1
    assert "malformed Mustache template" in result.output
    assert "the run continues without a label" in result.output


def test_control_characters_and_overlong_label_are_cleaned(tmp_path: Path) -> None:
    orch = _write(
        tmp_path / "doc.yml",
        "label: \"{{input.raw}}\"\n"
        "interface:\n  inputs:\n    raw: {type: string, required: true}\n"
        "effects:\n" + _tool_step(),
    )
    out = tmp_path / "out.json"
    raw = "a\x1b[31mb\x07c" + "x" * 70
    result = runner.invoke(
        app, ["run", str(orch), "--out", str(out), "-e", f"raw={raw}", "--quiet"]
    )
    assert result.exit_code == 0, result.output
    label = _last_run(out)["label"]
    assert label == ("abc" + "x" * 70)[:64]
    assert len(label) == 64


def test_use_childs_own_label_is_ignored(tmp_path: Path) -> None:
    _write(
        tmp_path / "child.yml",
        "label: 'child label should never surface'\neffects:\n" + _tool_step("ct"),
    )
    orch = _write(
        tmp_path / "parent.yml",
        "label: 'parent label'\neffects:\n"
        "  - type: use\n    name: child_use\n    path: child.yml\n",
    )
    out = tmp_path / "out.json"
    result = runner.invoke(app, ["run", str(orch), "--out", str(out), "--quiet"])
    assert result.exit_code == 0, result.output
    assert _last_run(out)["label"] == "parent label"


def test_events_run_start_carries_the_label_right_after_orchestration(tmp_path: Path) -> None:
    orch = _write(
        tmp_path / "doc.yml",
        "label: 'ticket 42'\neffects:\n" + _tool_step(),
    )
    out = tmp_path / "out.json"
    events = tmp_path / "events.jsonl"
    result = runner.invoke(
        app, ["run", str(orch), "--out", str(out), "--events", str(events), "--quiet"]
    )
    assert result.exit_code == 0, result.output
    run_start = json.loads(events.read_text(encoding="utf-8").splitlines()[0])
    assert run_start["ev"] == "run_start"
    assert run_start["label"] == "ticket 42"
    keys = list(run_start.keys())
    assert keys.index("label") == keys.index("orchestration") + 1


def test_events_run_start_omits_label_when_unlabelled(tmp_path: Path) -> None:
    orch = _write(tmp_path / "doc.yml", "effects:\n" + _tool_step())
    out = tmp_path / "out.json"
    events = tmp_path / "events.jsonl"
    result = runner.invoke(
        app, ["run", str(orch), "--out", str(out), "--events", str(events), "--quiet"]
    )
    assert result.exit_code == 0, result.output
    run_start = json.loads(events.read_text(encoding="utf-8").splitlines()[0])
    assert "label" not in run_start


def test_live_state_carries_the_label(tmp_path: Path) -> None:
    orch = _write(
        tmp_path / "doc.yml",
        "label: 'ticket 42'\neffects:\n" + _tool_step(),
    )
    out = tmp_path / "out.json"
    live_state = tmp_path / "live.json"
    result = runner.invoke(
        app,
        ["run", str(orch), "--out", str(out), "--live-state", str(live_state), "--quiet"],
    )
    assert result.exit_code == 0, result.output
    live = json.loads(live_state.read_text(encoding="utf-8"))
    assert live["runtime"]["last_run"]["label"] == "ticket 42"


def test_resume_without_label_re_renders_from_the_same_inputs(tmp_path: Path) -> None:
    orch = _write(
        tmp_path / "doc.yml",
        "label: 'ticket {{input.ticket}}'\n"
        "interface:\n  inputs:\n    ticket: {type: string, default: '1'}\n"
        "effects:\n" + _tool_step(),
    )
    out1 = tmp_path / "out1.json"
    result = runner.invoke(
        app, ["run", str(orch), "--out", str(out1), "-e", "ticket=99", "--quiet"]
    )
    assert result.exit_code == 0, result.output
    assert _last_run(out1)["label"] == "ticket 99"

    out2 = tmp_path / "out2.json"
    result = runner.invoke(
        app,
        [
            "run", str(orch), "--resume", "last", "--state", str(out1),
            "--out", str(out2), "--quiet",
        ],
    )
    assert result.exit_code == 0, result.output
    assert _last_run(out2)["label"] == "ticket 99"


def test_resume_with_label_flag_wins(tmp_path: Path) -> None:
    orch = _write(
        tmp_path / "doc.yml",
        "label: 'ticket {{input.ticket}}'\n"
        "interface:\n  inputs:\n    ticket: {type: string, default: '1'}\n"
        "effects:\n" + _tool_step(),
    )
    out1 = tmp_path / "out1.json"
    result = runner.invoke(
        app, ["run", str(orch), "--out", str(out1), "-e", "ticket=99", "--quiet"]
    )
    assert result.exit_code == 0, result.output

    out2 = tmp_path / "out2.json"
    result = runner.invoke(
        app,
        [
            "run", str(orch), "--resume", "last", "--state", str(out1),
            "--out", str(out2), "--label", "resumed override", "--quiet",
        ],
    )
    assert result.exit_code == 0, result.output
    assert _last_run(out2)["label"] == "resumed override"


def test_last_replays_the_label(tmp_path: Path) -> None:
    """`--last` replays every stashed argument, `--label` included — even
    the stashed `--out` wins over a fresh invocation's own, the same as
    every other replayed flag, so this writes back to `out1` again."""
    orch = _write(
        tmp_path / "doc.yml",
        "effects:\n" + _tool_step(),
    )
    out1 = tmp_path / "out1.json"
    result = runner.invoke(
        app,
        ["run", str(orch), "--out", str(out1), "--label", "stashed label", "--quiet"],
    )
    assert result.exit_code == 0, result.output
    assert _last_run(out1)["label"] == "stashed label"

    result = runner.invoke(app, ["run", "--last", "--quiet"])
    assert result.exit_code == 0, result.output
    assert _last_run(out1)["label"] == "stashed label"
