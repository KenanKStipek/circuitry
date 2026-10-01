"""A repeated key is an error in a `.json` orchestration too (#303).

`json.loads` keeps the last of two identical keys without a word, the same
gap #294/#261 closed for YAML. Every `.json` orchestration the runtime
loads — a file, a library ref, a `use` child loaded by `path:` — goes
through :func:`load_json` instead, which rejects the object and names the
key and its path.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from circuitry.cli.orchestration_loader import load_orchestration_file
from circuitry.cli.runtime_shim import validate
from circuitry.core.json_load import DuplicateKeyError, load_json


def test_duplicate_top_level_key_names_the_key() -> None:
    with pytest.raises(DuplicateKeyError, match="duplicate key 'effects' in top level"):
        load_json(
            '{"effects": [{"type": "tool", "name": "a", "provider": "json", '
            '"params": {"input": "1"}}], '
            '"effects": [{"type": "tool", "name": "b", "provider": "json", '
            '"params": {"input": "2"}}]}'
        )


def test_duplicate_key_nested_inside_an_effect_names_its_path() -> None:
    with pytest.raises(
        DuplicateKeyError, match=r"duplicate key 'input' in effects\[0\]\.params"
    ):
        load_json(
            '{"effects": [{"type": "tool", "name": "a", "provider": "json", '
            '"params": {"input": "1", "input": "2"}}]}'
        )


def test_unique_keys_load_like_json_loads() -> None:
    assert load_json('{"a": 1, "b": [1, {}, [], {"c": 2}]}') == {
        "a": 1,
        "b": [1, {}, [], {"c": 2}],
    }


def test_orchestration_file_with_duplicate_key_fails_naming_the_file(tmp_path: Path) -> None:
    path = tmp_path / "dup.json"
    path.write_text(
        '{"effects": [{"type": "tool", "name": "a", "provider": "json", '
        '"params": {"mode": "parse", "input": "1"}}], '
        '"effects": []}',
        encoding="utf-8",
    )
    with pytest.raises(DuplicateKeyError, match=r"dup\.json: duplicate key 'effects'"):
        load_orchestration_file(path)

    result = validate(path)
    assert result["ok"] is False
    assert "duplicate key 'effects' in top level" in result["errors"][0]


def test_run_on_a_duplicate_key_json_file_fails_the_same_way(tmp_path: Path) -> None:
    path = tmp_path / "dup.json"
    path.write_text(
        '{"effects": [{"type": "tool", "name": "a", "provider": "json", '
        '"params": {"mode": "parse", "input": "1"}}], '
        '"effects": []}',
        encoding="utf-8",
    )
    from circuitry.cli.config import CircuitryConfig
    from circuitry.cli.runtime_shim import RunRequest, run

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
    assert "duplicate key 'effects' in top level" in (result.error or "")
    assert "prime" not in result.state


def test_use_path_child_json_with_duplicate_key_fails_to_load(tmp_path: Path) -> None:
    (tmp_path / "child.json").write_text(
        '{"effects": [{"type": "tool", "name": "a", "provider": "json", '
        '"params": {"mode": "parse", "input": "1"}, "name": "b"}]}',
        encoding="utf-8",
    )
    parent = tmp_path / "parent.yml"
    parent.write_text(
        "effects:\n"
        "  - type: use\n"
        "    name: child\n"
        "    path: child.json\n"
        "    on_error: continue\n"
        "    validate: false\n",
        encoding="utf-8",
    )
    from dataclasses import dataclass

    from circuitry.adapters.base import GenerateResult
    from circuitry.cli.config import CircuitryConfig
    from circuitry.cli.runtime_shim import RunRequest, run

    @dataclass(frozen=True)
    class StubAdapter:
        name: str = "stub"

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            raise AssertionError("no prompt effect should run")

    result = run(
        RunRequest(
            orchestration_path=parent,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
            skip_preflight=True,
            adapter=StubAdapter(),
        )
    )
    assert result.ok is True, result.error
    error = result.state["prime"]["child"]["meta"]["error"]
    assert "child.json: duplicate key 'name' in effects[0]" in error


# A `use ref:` child can't actually be `.json`: every `LibrarySource` that
# discovers entries from a directory (`FolderSource._orchestration_files`,
# and `CurationSource`'s own filesystem fallback) is hardcoded to
# `FOLDER_SUFFIXES = (".yml", ".yaml")` (`cli/library_sources.py`) — a
# `.json` file is never enumerated as a resolvable entry, `path:`/inline are
# the only ways a `.json` document reaches `use`.
