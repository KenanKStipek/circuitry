"""``cof run``'s own console line for a ``report`` effect: one ``▸ <message>``
line, printed unless ``--quiet``/``--json`` (#605)."""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest
import yaml

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


def _write_orch(tmp_path: Path) -> Path:
    orch: dict[str, Any] = {
        "effects": [
            {"type": "report", "message": "working on {{input.topic}}"},
            {
                "type": "report",
                "name": "summary",
                "level": "warning",
                "message": "found an issue",
            },
        ],
    }
    path = tmp_path / "orch.yml"
    path.write_text(yaml.dump(orch, sort_keys=False), encoding="utf-8")
    return path


def _invoke(tmp_path: Path, orch: Path, *extra: str) -> Any:
    out = tmp_path / "state.json"
    return runner.invoke(
        app,
        [
            "run",
            str(orch),
            "--dry-run",
            "--tail",
            "--out",
            str(out),
            "-e",
            "topic=circuitry",
            *extra,
        ],
    )


def test_prints_one_line_per_report(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path)
    result = _invoke(tmp_path, orch)

    assert result.exit_code == 0, result.stdout
    lines = [line for line in result.stdout.splitlines() if line.startswith("▸")]
    assert lines == [
        "▸ working on circuitry",
        "▸ found an issue",
    ]


def test_suppressed_under_quiet(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path)
    result = _invoke(tmp_path, orch, "--quiet")

    assert result.exit_code == 0, result.stdout
    assert "▸" not in result.stdout


def test_suppressed_under_json(tmp_path: Path) -> None:
    orch = _write_orch(tmp_path)
    out = tmp_path / "state.json"
    # --json is mutually exclusive with --tail (the auto-quiet bypass
    # every other case here uses) -- not needed anyway: --json already
    # suppresses the line on its own, same as the non-tty default this
    # runner already gets without --tail.
    result = runner.invoke(
        app,
        [
            "run",
            str(orch),
            "--dry-run",
            "--json",
            "--out",
            str(out),
            "-e",
            "topic=circuitry",
        ],
    )

    assert result.exit_code == 0, result.stdout
    assert "▸" not in result.stdout
