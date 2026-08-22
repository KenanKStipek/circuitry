"""Tests for multi-format orchestration loading (JSON, TOON) and serialization."""
from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.cli.orchestration_loader import (
    FORMAT_LABELS,
    ORCHESTRATION_SUFFIXES,
    load_orchestration_file,
    serialize_orchestration,
)
from circuitry.cli.runtime_shim import inspect_orchestration, validate


def _write(tmp_path: Path, name: str, content: str) -> Path:
    path = tmp_path / name
    path.write_text(content, encoding="utf-8")
    return path


VALID_ORCH: dict[str, Any] = {
    "effects": [
        {"type": "prompt", "name": "greet", "template": "Say hello", "format": "text"}
    ]
}

VALID_YAML = "effects:\n  - type: prompt\n    name: greet\n    template: Say hello\n    format: text\n"

VALID_JSON = json.dumps(VALID_ORCH, indent=2)


@pytest.fixture()
def valid_toon() -> str:
    encode = pytest.importorskip(
        "toon_format",
        reason="toon-format optional extra not installed",
    ).encode

    return encode(VALID_ORCH)


# ---------------------------------------------------------------------------
# ORCHESTRATION_SUFFIXES constant
# ---------------------------------------------------------------------------


def test_orchestration_suffixes_contains_all_formats() -> None:
    assert ".yml" in ORCHESTRATION_SUFFIXES
    assert ".yaml" in ORCHESTRATION_SUFFIXES
    assert ".json" in ORCHESTRATION_SUFFIXES
    assert ".toon" in ORCHESTRATION_SUFFIXES


def test_format_labels_covers_all_suffixes() -> None:
    for suffix in ORCHESTRATION_SUFFIXES:
        assert suffix in FORMAT_LABELS


# ---------------------------------------------------------------------------
# load_orchestration_file — happy path
# ---------------------------------------------------------------------------


def test_load_yaml(tmp_path: Path) -> None:
    path = _write(tmp_path, "orch.yml", VALID_YAML)
    data = load_orchestration_file(path)
    assert data["effects"][0]["name"] == "greet"


def test_load_json(tmp_path: Path) -> None:
    path = _write(tmp_path, "orch.json", VALID_JSON)
    data = load_orchestration_file(path)
    assert data["effects"][0]["name"] == "greet"


def test_load_toon(tmp_path: Path, valid_toon: str) -> None:
    path = _write(tmp_path, "orch.toon", valid_toon)
    data = load_orchestration_file(path)
    assert data["effects"][0]["name"] == "greet"


@pytest.mark.parametrize("ext,content_fn", [
    (".yml", lambda: VALID_YAML),
    (".json", lambda: VALID_JSON),
])
def test_load_parametrized(tmp_path: Path, ext: str, content_fn: Any) -> None:
    path = _write(tmp_path, f"orch{ext}", content_fn())
    data = load_orchestration_file(path)
    assert isinstance(data, dict)
    assert "effects" in data


def test_load_toon_parametrized(tmp_path: Path, valid_toon: str) -> None:
    path = _write(tmp_path, "orch.toon", valid_toon)
    data = load_orchestration_file(path)
    assert isinstance(data, dict)
    assert "effects" in data


# ---------------------------------------------------------------------------
# load_orchestration_file — error cases
# ---------------------------------------------------------------------------


def test_unsupported_extension_lists_all_supported(tmp_path: Path) -> None:
    path = _write(tmp_path, "orch.xml", "<root/>")
    with pytest.raises(ValueError, match=r"Unsupported orchestration format.*\.xml"):
        load_orchestration_file(path)


def test_json_non_dict_root(tmp_path: Path) -> None:
    path = _write(tmp_path, "orch.json", "[1, 2, 3]")
    with pytest.raises(ValueError, match="Orchestration JSON must be a mapping"):
        load_orchestration_file(path)


def test_toon_non_dict_root(tmp_path: Path) -> None:
    encode = pytest.importorskip(
        "toon_format", reason="toon-format optional extra not installed"
    ).encode

    toon_content = encode([1, 2, 3])
    path = _write(tmp_path, "orch.toon", toon_content)
    with pytest.raises(ValueError, match="Orchestration TOON must be a mapping"):
        load_orchestration_file(path)


def test_malformed_json(tmp_path: Path) -> None:
    path = _write(tmp_path, "orch.json", "{not valid json")
    # json.loads raises JSONDecodeError, a ValueError subclass.
    with pytest.raises(ValueError):
        load_orchestration_file(path)


# ---------------------------------------------------------------------------
# serialize_orchestration
# ---------------------------------------------------------------------------


def test_serialize_yaml() -> None:
    result = serialize_orchestration(VALID_ORCH, "yaml")
    parsed = yaml.safe_load(result)
    assert parsed["effects"][0]["name"] == "greet"


def test_serialize_json() -> None:
    result = serialize_orchestration(VALID_ORCH, "json")
    parsed = json.loads(result)
    assert parsed["effects"][0]["name"] == "greet"


def test_serialize_toon() -> None:
    decode = pytest.importorskip(
        "toon_format", reason="toon-format optional extra not installed"
    ).decode

    result = serialize_orchestration(VALID_ORCH, "toon")
    parsed = decode(result)
    assert parsed["effects"][0]["name"] == "greet"


def test_serialize_unsupported_format() -> None:
    with pytest.raises(ValueError, match="Unsupported output format"):
        serialize_orchestration(VALID_ORCH, "xml")


def test_serialize_roundtrip_json(tmp_path: Path) -> None:
    serialized = serialize_orchestration(VALID_ORCH, "json")
    path = _write(tmp_path, "rt.json", serialized)
    loaded = load_orchestration_file(path)
    assert loaded == VALID_ORCH


def test_serialize_roundtrip_toon(tmp_path: Path) -> None:
    pytest.importorskip(
        "toon_format", reason="toon-format optional extra not installed"
    )
    serialized = serialize_orchestration(VALID_ORCH, "toon")
    path = _write(tmp_path, "rt.toon", serialized)
    loaded = load_orchestration_file(path)
    assert loaded == VALID_ORCH


# ---------------------------------------------------------------------------
# validate() across formats
# ---------------------------------------------------------------------------


def test_validate_json_orchestration(tmp_path: Path) -> None:
    path = _write(tmp_path, "orch.json", VALID_JSON)
    result = validate(path)
    assert result["ok"] is True
    assert result["errors"] == []


def test_validate_toon_orchestration(tmp_path: Path, valid_toon: str) -> None:
    path = _write(tmp_path, "orch.toon", valid_toon)
    result = validate(path)
    assert result["ok"] is True
    assert result["errors"] == []


def test_validate_invalid_json_schema(tmp_path: Path) -> None:
    bad = json.dumps({"effects": [{"type": "bogus", "name": "x"}]})
    path = _write(tmp_path, "bad.json", bad)
    result = validate(path)
    assert result["ok"] is False
    assert len(result["errors"]) > 0


# ---------------------------------------------------------------------------
# inspect_orchestration() across formats
# ---------------------------------------------------------------------------


def test_inspect_json(tmp_path: Path) -> None:
    path = _write(tmp_path, "orch.json", VALID_JSON)
    summary = inspect_orchestration(path)
    assert summary["effects_count"] == 1
    assert summary["effect_names"] == ["greet"]
    assert summary["format"] == "json"


def test_inspect_toon(tmp_path: Path, valid_toon: str) -> None:
    path = _write(tmp_path, "orch.toon", valid_toon)
    summary = inspect_orchestration(path)
    assert summary["effects_count"] == 1
    assert summary["effect_names"] == ["greet"]
    assert summary["format"] == "toon"
