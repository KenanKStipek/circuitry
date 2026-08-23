"""`cof run --profile-from-state` — the public entry point for reconstructing
a run from `runtime.effective_settings.profile` alone (issue #132 / epic #11
AC3), rather than hand-assembling a `ProfileSettings` from internals.
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


def _write_orch(tmp_path: Path) -> Path:
    return _write(
        tmp_path / "recipe.yml",
        """
effects:
  - type: prompt
    name: summarize
    template: "summarize {{topic}}"
  - type: prompt
    name: extra
    template: "extra {{topic}}"
""".lstrip(),
    )


def test_profile_from_state_reproduces_the_run_without_the_profile_file(
    tmp_path: Path,
) -> None:
    orch = _write_orch(tmp_path)
    profile_path = _write(
        tmp_path / "profiles" / "repro.yml",
        "model: reproduced-model\neffects:\n  extra:\n    enabled: false\n",
    )
    first_out = tmp_path / "first.json"

    first = runner.invoke(
        app,
        [
            "run",
            str(orch),
            "--profile",
            "repro",
            "-e",
            "topic=widgets",
            "--dry-run",
            "--out",
            str(first_out),
        ],
    )
    assert first.exit_code == 0, first.stdout
    first_state = json.loads(first_out.read_text(encoding="utf-8"))
    assert first_state["runtime"]["effective_settings"]["model"] == "reproduced-model"
    assert first_state["prime"]["extra"]["meta"]["disabled"] is True

    # Reconstruction must not read the profile file back.
    profile_path.unlink()

    second_out = tmp_path / "second.json"
    second = runner.invoke(
        app,
        [
            "run",
            str(orch),
            "--profile-from-state",
            str(first_out),
            "-e",
            "topic=widgets",
            "--dry-run",
            "--out",
            str(second_out),
        ],
    )
    assert second.exit_code == 0, second.stdout
    second_state = json.loads(second_out.read_text(encoding="utf-8"))
    assert second_state["runtime"]["effective_settings"]["model"] == "reproduced-model"
    assert second_state["prime"]["extra"]["meta"]["disabled"] is True
    assert (
        second_state["runtime"]["effective_settings"]["profile"]
        == first_state["runtime"]["effective_settings"]["profile"]
    )


def test_profile_and_profile_from_state_are_mutually_exclusive(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path)
    _write(tmp_path / "profiles" / "fast.yml", "model: cheap\n")
    state_path = _write(tmp_path / "state.json", "{}")

    result = runner.invoke(
        app,
        [
            "run",
            str(orch),
            "--profile",
            "fast",
            "--profile-from-state",
            str(state_path),
            "--dry-run",
        ],
    )

    assert result.exit_code == 1
    assert "mutually exclusive" in result.stdout


def test_profile_from_state_refuses_a_state_with_no_recorded_profile(
    tmp_path: Path,
) -> None:
    orch = _write_orch(tmp_path)
    state_path = _write(tmp_path / "state.json", json.dumps({"runtime": {}}))

    result = runner.invoke(
        app,
        [
            "run",
            str(orch),
            "--profile-from-state",
            str(state_path),
            "--dry-run",
        ],
    )

    assert result.exit_code == 1
    assert "no runtime.effective_settings.profile" in result.stdout or (
        "carries no" in result.stdout
    )


def test_profile_from_state_refuses_a_redacted_record(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path)
    _write(
        tmp_path / "profiles" / "leaky.yml",
        'inputs:\n  api_key: "sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"\n',
    )
    out = tmp_path / "leaky.json"

    first = runner.invoke(
        app,
        [
            "run",
            str(orch),
            "--profile",
            "leaky",
            "-e",
            "topic=widgets",
            "--dry-run",
            "--out",
            str(out),
        ],
    )
    assert first.exit_code == 0, first.stdout

    result = runner.invoke(
        app,
        [
            "run",
            str(orch),
            "--profile-from-state",
            str(out),
            "-e",
            "topic=widgets",
            "--dry-run",
        ],
    )

    assert result.exit_code == 1
    assert "inputs.api_key" in result.stdout
