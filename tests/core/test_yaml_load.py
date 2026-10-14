"""A repeated key is an error in every orchestration the runtime loads (#261).

``yaml.safe_load`` keeps the last of two identical keys without a word; the
loader behind files, library refs and ``use: inline`` children rejects the
mapping and names both lines instead.
"""

from __future__ import annotations

from pathlib import Path

import pytest
import yaml

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


def test_merge_key_inside_a_merged_mapping_is_not_a_duplicate() -> None:
    """#390: a merge source that overrides its own merged key is not a dup."""
    text = (
        "base: &base {retries: 1, timeout: 5}\n"
        "templates:\n"
        "  defaults: &defaults\n"
        "    <<: *base\n"
        "    retries: 2\n"
        "effect:\n"
        "  <<: *defaults\n"
    )
    assert load_yaml(text) == yaml.safe_load(text)


def test_merge_source_nested_three_levels_deep_matches_safe_load() -> None:
    """#390: the merge source's own ``<<`` must sit deeper than the
    mapping that merges it in -- a top-level sibling never hits the
    breadth-first construction order the bug depends on.
    """
    text = (
        "base: &base {r: 1, t: 5}\n"
        "a:\n"
        "  b:\n"
        "    c: &c\n"
        "      <<: *base\n"
        "      r: 2\n"
        "top:\n"
        "  <<: *c\n"
    )
    assert load_yaml(text) == yaml.safe_load(text)


def test_merge_of_a_list_of_mappings_matches_safe_load() -> None:
    """#390: ``mid``'s own list-of-merges sits nested under ``t:``, deeper
    than the top-level ``top:`` that merges it in.
    """
    text = (
        "a: &a {x: 1, y: 2}\n"
        "b: &b {y: 20, z: 3}\n"
        "t:\n"
        "  mid: &mid\n"
        "    <<: [*a, *b]\n"
        "    y: 200\n"
        "top:\n"
        "  <<: *mid\n"
    )
    assert load_yaml(text) == yaml.safe_load(text)


def test_merge_chain_with_overrides_at_each_depth_matches_safe_load() -> None:
    """#390: each merge source sits at a different depth -- ``l2`` under
    ``x:``, ``l3`` under ``x: {y: ...}``, ``l4`` back at the top level.
    """
    text = (
        "l1: &l1 {r: 1, t: 5}\n"
        "x:\n"
        "  l2: &l2\n"
        "    <<: *l1\n"
        "    r: 2\n"
        "  y:\n"
        "    l3: &l3\n"
        "      <<: *l2\n"
        "      t: 50\n"
        "l4:\n"
        "  <<: *l3\n"
        "  r: 3\n"
    )
    assert load_yaml(text) == yaml.safe_load(text)


def test_real_duplicate_alongside_a_merge_key_is_still_rejected() -> None:
    text = "base: &base {x: 1}\neffect:\n  <<: *base\n  y: 2\n  y: 3\n"
    with pytest.raises(DuplicateKeyError, match="duplicate key 'y' at line 5"):
        load_yaml(text)


def test_real_duplicate_inside_a_nested_merge_source_fails_the_run(tmp_path: Path) -> None:
    """#390: a real duplicate inside a nested merge source still fails,
    through the full ``run()`` path, not just ``load_yaml`` directly.
    """
    from circuitry.cli.config import CircuitryConfig
    from circuitry.cli.runtime_shim import RunRequest, run

    path = tmp_path / "dup.yml"
    path.write_text(
        "base: &base {retries: 1}\n"
        "templates:\n"
        "  defaults: &defaults\n"
        "    <<: *base\n"
        "    retries: 2\n"
        "    retries: 3\n"
        "effect:\n"
        "  <<: *defaults\n",
        encoding="utf-8",
    )

    result = run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
            skip_preflight=True,
        )
    )
    assert result.ok is False
    assert (
        "duplicate key 'retries' at line 6, column 5 "
        "(first defined at line 5, column 5); "
        "YAML would silently keep only the last one" in (result.error or "")
    )
    assert "prime" not in result.state


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
