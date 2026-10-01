"""A repeated key is an error in every orchestration the runtime loads (#261).

``yaml.safe_load`` keeps the last of two identical keys without a word; the
loader behind files, library refs and ``use: inline`` children rejects the
mapping and names both lines instead.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from circuitry.cli.orchestration_loader import load_orchestration_file
from circuitry.cli.runtime_shim import validate
from circuitry.core.use import _validate_inline_yaml
from circuitry.core.yaml_load import DuplicateKeyError, load_yaml


def test_duplicate_top_level_key_names_both_lines() -> None:
    with pytest.raises(DuplicateKeyError) as exc:
        load_yaml("model: a\neffects: []\nmodel: b\n")
    message = str(exc.value)
    assert "duplicate key 'model' at line 3" in message
    assert "first defined at line 1" in message


def test_duplicate_key_inside_an_effect_is_rejected() -> None:
    text = (
        "effects:\n"
        "  - type: prompt\n"
        "    name: greet\n"
        "    template: one\n"
        "    template: two\n"
    )
    with pytest.raises(DuplicateKeyError, match="duplicate key 'template' at line 5"):
        load_yaml(text)


def test_merge_key_overrides_are_not_duplicates() -> None:
    text = "base: &b {k: 1, j: 2}\nderived:\n  <<: *b\n  k: 3\n"
    assert load_yaml(text)["derived"] == {"k": 3, "j": 2}


def test_unique_keys_load_like_safe_load() -> None:
    assert load_yaml("a: 1\nb: [x, {c: 2}]\n") == {"a": 1, "b": ["x", {"c": 2}]}


def test_orchestration_file_with_duplicate_key_fails_naming_the_file(tmp_path: Path) -> None:
    path = tmp_path / "dup.yml"
    path.write_text(
        "effects:\n"
        "  - {type: tool, name: a, provider: json, params: {input: '1'}}\n"
        "effects: []\n",
        encoding="utf-8",
    )
    with pytest.raises(DuplicateKeyError, match=r"dup\.yml: duplicate key 'effects'"):
        load_orchestration_file(path)

    result = validate(path)
    assert result["ok"] is False
    assert "duplicate key 'effects' at line 3" in result["errors"][0]


def test_inline_child_with_duplicate_key_fails_validation() -> None:
    ok, errors = _validate_inline_yaml(
        "effects:\n"
        "  - type: tool\n"
        "    name: a\n"
        "    name: b\n"
        "    provider: json\n"
        "    params: {input: '1'}\n"
    )
    assert ok is False
    assert "duplicate key 'name' at line 4" in errors[0]
