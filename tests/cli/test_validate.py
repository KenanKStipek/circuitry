from __future__ import annotations

from pathlib import Path

from circuitry.cli.runtime_shim import validate


def _write(tmp_path: Path, name: str, content: str) -> Path:
    path = tmp_path / name
    path.write_text(content, encoding="utf-8")
    return path


def test_validate_ok_for_valid_orchestration(tmp_path: Path) -> None:
    path = _write(
        tmp_path,
        "valid.yml",
        """
effects:
  - type: prompt
    name: greet
    template: "Hello"
""".strip()
        + "\n",
    )
    result = validate(path)
    assert result["ok"] is True
    assert result["errors"] == []


def test_validate_fails_for_empty_file(tmp_path: Path) -> None:
    path = _write(tmp_path, "empty.yml", "")
    result = validate(path)
    assert result["ok"] is False
    assert result["errors"] == ["Orchestration file is empty."]


def test_validate_fails_for_duplicate_sibling_names(tmp_path: Path) -> None:
    path = _write(
        tmp_path,
        "dup.yml",
        """
effects:
  - type: prompt
    name: greet
    template: "a"
  - type: prompt
    name: greet
    template: "b"
""".strip()
        + "\n",
    )
    result = validate(path)
    assert result["ok"] is False
    assert len(result["errors"]) == 1
    assert "Duplicate effect name 'greet'" in result["errors"][0]
