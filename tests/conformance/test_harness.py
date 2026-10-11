"""Unit tests for `harness.load_case`'s `case.json` validation and for
`harness._sandboxed_env`'s hermeticity."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from . import harness


def _write_case(tmp_path: Path, metadata: dict) -> Path:
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    (case_dir / "case.json").write_text(json.dumps(metadata), encoding="utf-8")
    return case_dir


def test_load_case_fills_in_defaults(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"spec_case": "C0", "expect": "success"})
    metadata = harness.load_case(case_dir)
    assert metadata["orchestration"] == "orchestration.yml"
    assert metadata["engines"] == ["python"]
    assert metadata["cli_args"] == []


def test_load_case_rejects_unknown_key(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "engine": ["python"]})
    with pytest.raises(ValueError, match="unknown key"):
        harness.load_case(case_dir)


def test_load_case_requires_expect(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"spec_case": "C0"})
    with pytest.raises(ValueError, match="missing the required 'expect' key"):
        harness.load_case(case_dir)


def test_load_case_rejects_invalid_expect(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "Success"})
    with pytest.raises(ValueError, match="'expect' must be one of"):
        harness.load_case(case_dir)


def test_load_case_rejects_invalid_error_compare(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "failure", "error_compare": "Exact"})
    with pytest.raises(ValueError, match="'error_compare' must be one of"):
        harness.load_case(case_dir)


def test_load_case_rejects_invalid_engines(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "engines": ["electicity"]})
    with pytest.raises(ValueError, match="'engines' must be"):
        harness.load_case(case_dir)


def test_load_case_rejects_empty_engines(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "engines": []})
    with pytest.raises(ValueError, match="'engines' must be"):
        harness.load_case(case_dir)


def test_load_case_rejects_non_bool_sort_warnings(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success", "sort_warnings": "true"})
    with pytest.raises(ValueError, match="'sort_warnings' must be a bool"):
        harness.load_case(case_dir)


def test_load_case_defaults_sort_warnings_to_false(tmp_path: Path) -> None:
    case_dir = _write_case(tmp_path, {"expect": "success"})
    metadata = harness.load_case(case_dir)
    assert metadata["sort_warnings"] is False


def test_warning_lines_keeps_only_prefixed_lines_in_order() -> None:
    stderr = (
        "WARNING: first\n"
        "some other line\n"
        "ERROR: second\n"
        "Warning: lowercase, not a logging line\n"
        "INFO: third\n"
    )
    assert harness.warning_lines(stderr) == [
        "WARNING: first",
        "ERROR: second",
        "INFO: third",
    ]


def test_warning_lines_sorts_when_asked() -> None:
    stderr = "WARNING: b\nWARNING: a\nWARNING: c\n"
    assert harness.warning_lines(stderr, sort=True) == [
        "WARNING: a",
        "WARNING: b",
        "WARNING: c",
    ]


def test_warning_lines_empty_stderr_is_an_empty_list() -> None:
    assert harness.warning_lines("") == []


def test_sandboxed_env_drops_credentials_and_circuitry_vars(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("OPENAI_API_KEY", "sk-leaked")
    monkeypatch.setenv("CIRCUITRY_MODEL", "x")
    monkeypatch.setenv("CIRCUITRY_CONFIG", "/nonexistent")
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    home_dir = tmp_path / "home"

    env = harness._sandboxed_env(case_dir, home_dir)

    assert "OPENAI_API_KEY" not in env
    assert "CIRCUITRY_MODEL" not in env
    assert "CIRCUITRY_CONFIG" not in env
    assert env["HOME"] == str(home_dir)
