"""``cli/app.py::_warn_resumed_input_changes`` (#584): the ordering and
missing-saved-value rules of the one ``WARNING:`` a `--resume` with
changed ``-e`` overrides prints, in isolation from a full run.
"""

from __future__ import annotations

import logging
from pathlib import Path

import pytest

from circuitry.cli.app import _warn_resumed_input_changes


def _write_doc(tmp_path: Path, content: str) -> Path:
    path = tmp_path / "doc.yml"
    path.write_text(content, encoding="utf-8")
    return path


def test_warns_declared_inputs_first_then_undeclared_sorted(
    tmp_path: Path, caplog: pytest.LogCaptureFixture
) -> None:
    orch = _write_doc(
        tmp_path,
        "interface:\n  inputs:\n    first: {type: string, default: a}\n"
        "    second: {type: string, default: a}\n"
        "effects: []\n",
    )
    with caplog.at_level(logging.WARNING, logger="circuitry.cli.app"):
        _warn_resumed_input_changes(
            orch_path=orch,
            saved_input={"first": "a", "second": "a", "zzz": "a"},
            overrides={"zzz": "b", "second": "b", "first": "b"},
        )
    [record] = caplog.records
    assert record.getMessage() == (
        "Resuming with changed input(s): first: 'a' -> 'b', second: 'a' -> 'b', "
        "zzz: 'a' -> 'b'. Other finished effects keep their saved results."
    )


def test_a_saved_state_with_no_input_mapping_counts_as_empty(
    tmp_path: Path, caplog: pytest.LogCaptureFixture
) -> None:
    orch = _write_doc(
        tmp_path,
        "interface:\n  inputs:\n    x: {type: string, default: a}\neffects: []\n",
    )
    with caplog.at_level(logging.WARNING, logger="circuitry.cli.app"):
        _warn_resumed_input_changes(
            orch_path=orch,
            saved_input={},
            overrides={"x": "b"},
        )
    [record] = caplog.records
    assert record.getMessage() == (
        "Resuming with changed input(s): x: None -> 'b'. "
        "Other finished effects keep their saved results."
    )


def test_unchanged_inputs_warn_nothing(
    tmp_path: Path, caplog: pytest.LogCaptureFixture
) -> None:
    orch = _write_doc(
        tmp_path,
        "interface:\n  inputs:\n    x: {type: string, default: a}\neffects: []\n",
    )
    with caplog.at_level(logging.WARNING, logger="circuitry.cli.app"):
        _warn_resumed_input_changes(
            orch_path=orch,
            saved_input={"x": "a"},
            overrides={"x": "a"},
        )
    assert caplog.records == []
