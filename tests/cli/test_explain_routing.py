"""``cof run --explain-routing`` — the per-effect line printed at dispatch.

Two layers: unit tests of :func:`circuitry.cli.explain_routing.explain_line`
against a bare node dict, and CLI-level tests over ``cof run --dry-run
--explain-routing`` — dry-run because the pre-dispatch meta block (and the
``on_effect_start`` fire it rides on) happens before the adapter is ever
called, so a dry run exercises the exact same window a live one would without
needing a reachable model.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
import yaml

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.cli.explain_routing import explain_line, make_explain_routing_observer

runner = CliRunner()


# -- unit tests: explain_line ---------------------------------------------------


def test_no_line_without_meta() -> None:
    assert explain_line("prime.task", {}) is None
    assert explain_line("prime.task", {"meta": None}) is None


def test_no_line_without_a_complexity_score() -> None:
    """Non-prompt effects, and prompts scored with scoring off, share this
    shape: a ``meta`` with no ``complexity`` key."""
    assert explain_line("prime.sum", {"meta": {"model": "m"}}) is None


def test_line_states_routing_off_with_no_band() -> None:
    node = {
        "meta": {
            "model": "llama3",
            "model_reason": "default",
            "complexity": {"score": 42.0, "max_score": 100.0},
        }
    }
    line = explain_line("prime.task", node)
    assert line is not None
    assert "prime.task" in line
    assert "score 42.0/100" in line
    assert "routing off" in line
    assert "model llama3" in line
    assert "why default" in line
    assert "band" not in line


def test_line_names_the_band_when_routing_resolved_one() -> None:
    node = {
        "meta": {
            "model": "small-model",
            "model_reason": "default",
            "complexity": {
                "score": 12.0,
                "max_score": 100.0,
                "band": {"name": "cheap", "model": "small-model"},
            },
        }
    }
    line = explain_line("prime.task", node)
    assert line is not None
    assert "band cheap" in line
    assert "routing off" not in line


def test_line_reports_an_explicit_model_choice() -> None:
    node = {
        "meta": {
            "model": "pinned-model",
            "model_reason": "explicit",
            "complexity": {"score": 5.0, "max_score": 100.0},
        }
    }
    line = explain_line("prime.task", node)
    assert line is not None
    assert "model pinned-model" in line
    assert "why explicit" in line


def test_observer_only_prints_lines_that_exist() -> None:
    printed: list[str] = []
    observer = make_explain_routing_observer(printed.append)

    observer("prime.sum", {"meta": {"model": "m"}})  # not a scored prompt
    observer(
        "prime.task",
        {"meta": {"model": "m", "complexity": {"score": 1.0}}},
    )

    assert len(printed) == 1
    assert "prime.task" in printed[0]


# -- CLI: `cof run --dry-run --explain-routing` ---------------------------------


def _write(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def _write_orch(tmp_path: Path, *, complexity: dict[str, Any]) -> Path:
    orch: dict[str, Any] = {
        "runtime": {"complexity": complexity},
        "effects": [
            {"type": "prompt", "name": "first", "template": "Hello {{name}}."},
            {
                "type": "prompt",
                "name": "second",
                "template": "World.",
                "model": "pinned-model",
            },
        ],
    }
    return _write(tmp_path / "orch.yml", yaml.dump(orch, sort_keys=False))


SCORING_AND_ROUTING = {
    "scoring": {"enabled": True},
    "routing": {"enabled": True, "bands": [{"name": "any", "model": "small-model"}]},
}
SCORING_ONLY = {"scoring": {"enabled": True}}
NEITHER = {"scoring": {"enabled": False}}


def _invoke(tmp_path: Path, orch: Path, *extra: str) -> Any:
    out = tmp_path / "state.json"
    # ``--tail`` opts out of the CliRunner's non-tty auto-detection (which
    # otherwise forces --quiet/--json on every invocation — see run_cmd's
    # "Auto-pipe detection"), so the explain-routing lines actually reach
    # captured stdout instead of being suppressed before we can assert on them.
    return runner.invoke(
        app,
        [
            "run",
            str(orch),
            "--dry-run",
            "--tail",
            "--adapter",
            "ollama",
            "--model",
            "run-default",
            "--out",
            str(out),
            "-e",
            "name=World",
            *extra,
        ],
    )


def test_prints_one_line_per_prompt_in_dispatch_order_with_routing_on(
    tmp_path: Path,
) -> None:
    orch = _write_orch(tmp_path, complexity=SCORING_AND_ROUTING)
    result = _invoke(tmp_path, orch, "--explain-routing")

    assert result.exit_code == 0, result.stdout
    lines = [line for line in result.stdout.splitlines() if "score" in line]
    assert len(lines) == 2
    # Declaration order: "first" (default model) dispatches before "second"
    # (its own pinned model).
    assert "first" in lines[0] and "band any" in lines[0]
    assert "run-default" in lines[0] and "why default" in lines[0]
    assert "second" in lines[1] and "band any" in lines[1]
    assert "pinned-model" in lines[1] and "why explicit" in lines[1]


def test_a_routed_effect_reads_why_router(tmp_path: Path) -> None:
    """The reason the ``"router"`` value was reserved in the first place.

    Every other CLI case here passes ``--model``, which pins the run default
    and makes the router defer — so without this one the flag would never be
    seen printing the line it was built for.
    """
    orch = _write_orch(tmp_path, complexity=SCORING_AND_ROUTING)
    out = tmp_path / "state.json"
    result = runner.invoke(
        app,
        [
            "run",
            str(orch),
            "--dry-run",
            "--tail",
            "--adapter",
            "ollama",
            "--out",
            str(out),
            "-e",
            "name=World",
            "--explain-routing",
        ],
    )

    assert result.exit_code == 0, result.stdout
    lines = [line for line in result.stdout.splitlines() if "score" in line]
    assert len(lines) == 2
    # "first" inherits the run default, so the router takes it; "second" pins
    # its own model, so the router defers and says so on the same line as the
    # band it declined to apply.
    assert "band any" in lines[0]
    assert "small-model" in lines[0] and "why router" in lines[0]
    assert "band any" in lines[1]
    assert "pinned-model" in lines[1] and "why explicit" in lines[1]


def test_states_routing_off_but_still_shows_the_score(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path, complexity=SCORING_ONLY)
    result = _invoke(tmp_path, orch, "--explain-routing")

    assert result.exit_code == 0, result.stdout
    lines = [line for line in result.stdout.splitlines() if "score" in line]
    assert len(lines) == 2
    assert all("routing off" in line for line in lines)
    assert all("band" not in line for line in lines)


def test_emits_nothing_when_scoring_is_disabled(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path, complexity=NEITHER)
    result = _invoke(tmp_path, orch, "--explain-routing")

    assert result.exit_code == 0, result.stdout
    assert "score" not in result.stdout
    assert "▸" not in result.stdout


def test_quiet_suppresses_it_even_when_requested(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path, complexity=SCORING_AND_ROUTING)
    result = _invoke(tmp_path, orch, "--explain-routing", "--quiet")

    assert result.exit_code == 0, result.stdout
    assert "▸" not in result.stdout
    assert "score" not in result.stdout


def test_without_the_flag_nothing_is_printed(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path, complexity=SCORING_AND_ROUTING)
    result = _invoke(tmp_path, orch)

    assert result.exit_code == 0, result.stdout
    assert "▸" not in result.stdout


def test_last_replays_the_flag(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    from circuitry.cli import app as app_module

    fake_dir = tmp_path / "config-home"
    fake_dir.mkdir(parents=True, exist_ok=True)
    monkeypatch.setattr(app_module, "GLOBAL_CONFIG_DIR", fake_dir)
    monkeypatch.setattr(app_module, "_LAST_RUN_PATH", fake_dir / "last-run.json")

    orch = _write_orch(tmp_path, complexity=SCORING_AND_ROUTING)
    _invoke(tmp_path, orch, "--explain-routing")

    stashed = json.loads((fake_dir / "last-run.json").read_text(encoding="utf-8"))
    assert stashed["explain_routing"] is True

    replayed = runner.invoke(app, ["run", "--last"])
    assert replayed.exit_code == 0, replayed.stdout
    assert "▸" in replayed.stdout
