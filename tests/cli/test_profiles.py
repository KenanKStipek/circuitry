from __future__ import annotations

from pathlib import Path

import pytest

from circuitry.cli.profiles import (
    ProfileNotFoundError,
    ProfileValidationError,
    collect_orchestration_condition_paths,
    collect_orchestration_effect_paths,
    discover_profile_path,
    load_profile,
)


def _write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def _orch(tmp_path: Path) -> Path:
    orch_path = tmp_path / "recipe.yml"
    _write(
        orch_path,
        """
effects:
  - type: prompt
    name: summarize
    template: "s"
  - type: dynamic
    name: sub
    effects:
      - type: prompt
        name: deep_analysis
        template: "d"
""".strip()
        + "\n",
    )
    return orch_path


def test_collect_orchestration_effect_paths_covers_nested_and_transparent_scopes() -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "summarize", "template": "s"},
            {
                "type": "dynamic",
                "name": "sub",
                "effects": [{"type": "prompt", "name": "child", "template": "c"}],
            },
            {
                "type": "conditional",
                "if": {"mode": "cel", "expr": "true"},
                "then": [{"type": "prompt", "name": "transparent_child", "template": "t"}],
            },
            {
                "type": "conditional",
                "name": "named_cond",
                "if": {"mode": "cel", "expr": "true"},
                "then": [{"type": "prompt", "name": "branch_child", "template": "b"}],
            },
        ]
    }
    paths = collect_orchestration_effect_paths(orch)
    assert paths == {
        "summarize",
        "sub",
        "sub.child",
        "transparent_child",
        "named_cond",
        "named_cond.branch_child",
    }


def test_discover_profile_path_orchestration_scope_wins_over_project(tmp_path: Path) -> None:
    orch_path = _orch(tmp_path)
    orch_profile = orch_path.parent / "profiles" / "fast.yml"
    project_profile = tmp_path / "cwd" / "profiles" / "fast.yml"
    _write(orch_profile, "model: from-orch-scope\n")
    _write(project_profile, "model: from-project-scope\n")

    resolved = discover_profile_path(
        name="fast", orchestration_path=orch_path, cwd=tmp_path / "cwd"
    )
    assert resolved == orch_profile.resolve()


def test_discover_profile_path_falls_back_to_project_level(tmp_path: Path) -> None:
    orch_path = _orch(tmp_path)
    cwd = tmp_path / "cwd"
    project_profile = cwd / "profiles" / "fast.yml"
    _write(project_profile, "model: from-project-scope\n")

    resolved = discover_profile_path(name="fast", orchestration_path=orch_path, cwd=cwd)
    assert resolved == project_profile.resolve()


def test_discover_profile_path_not_found_lists_searched_locations(tmp_path: Path) -> None:
    orch_path = _orch(tmp_path)
    with pytest.raises(ProfileNotFoundError) as exc_info:
        discover_profile_path(name="missing", orchestration_path=orch_path, cwd=tmp_path)
    assert "missing" in str(exc_info.value)
    assert "profiles" in str(exc_info.value)


def test_load_profile_happy_path_parses_all_fields(tmp_path: Path) -> None:
    orch_path = _orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "fast.yml",
        """
adapter: ollama
model: llama3.2
inputs:
  topic: "widgets"
effects:
  summarize:
    model: cheap
    provider: cyberdiner
  sub.deep_analysis:
    model: good-fast
  my_reflector:
    enabled: false
persistence:
  backend: jsonl-file
  path: runs.jsonl
""".strip()
        + "\n",
    )

    profile = load_profile(name="fast", orchestration_path=orch_path, orch={
        "effects": [
            {"type": "prompt", "name": "summarize", "template": "s"},
            {
                "type": "dynamic",
                "name": "sub",
                "effects": [{"type": "prompt", "name": "deep_analysis", "template": "d"}],
            },
            {"type": "reflector", "name": "my_reflector", "effects": []},
        ]
    })

    assert profile.name == "fast"
    assert profile.adapter == "ollama"
    assert profile.model == "llama3.2"
    assert profile.inputs == {"topic": "widgets"}
    assert profile.effects["summarize"] == {"model": "cheap", "provider": "cyberdiner"}
    assert profile.effects["sub.deep_analysis"] == {"model": "good-fast"}
    assert profile.effects["my_reflector"] == {"enabled": False}
    assert profile.persistence == {"backend": "jsonl-file", "path": "runs.jsonl"}


def test_load_profile_rejects_unknown_top_level_key(tmp_path: Path) -> None:
    orch_path = _orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "fast.yml",
        "not_a_real_key: true\n",
    )
    with pytest.raises(ProfileValidationError) as exc_info:
        load_profile(
            name="fast",
            orchestration_path=orch_path,
            orch={"effects": []},
        )
    assert "schema validation" in str(exc_info.value)


def test_load_profile_rejects_unknown_effect_path_and_names_valid_ones(
    tmp_path: Path,
) -> None:
    orch_path = _orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "fast.yml",
        "effects:\n  does_not_exist:\n    model: cheap\n",
    )
    orch = {
        "effects": [
            {"type": "prompt", "name": "summarize", "template": "s"},
            {
                "type": "dynamic",
                "name": "sub",
                "effects": [{"type": "prompt", "name": "deep_analysis", "template": "d"}],
            },
        ]
    }
    with pytest.raises(ProfileValidationError) as exc_info:
        load_profile(name="fast", orchestration_path=orch_path, orch=orch)

    message = str(exc_info.value)
    assert "does_not_exist" in message
    assert "summarize" in message
    assert "sub.deep_analysis" in message


def test_load_profile_rejects_non_mapping_effect_override(tmp_path: Path) -> None:
    orch_path = _orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "fast.yml",
        "effects:\n  summarize: []\n",
    )
    orch = {"effects": [{"type": "prompt", "name": "summarize", "template": "s"}]}
    with pytest.raises(ProfileValidationError):
        load_profile(name="fast", orchestration_path=orch_path, orch=orch)


# ── condition paths are not disableable (issue #29) ──────────────────────────


def _conditional_orch() -> dict:
    return {
        "effects": [
            {
                "type": "conditional",
                "name": "named_cond",
                "if": {"mode": "cel", "expr": "true"},
                "then": [{"type": "prompt", "name": "branch_child", "template": "b"}],
            },
            {
                "type": "loop",
                "name": "named_loop",
                "while": {"mode": "cel", "expr": "true"},
                "body": [{"type": "prompt", "name": "body_child", "template": "l"}],
            },
            {
                "type": "conditional",
                "if": {"mode": "cel", "expr": "true"},
                "then": [{"type": "prompt", "name": "transparent", "template": "t"}],
            },
        ]
    }


def test_collect_orchestration_condition_paths_covers_conditionals_and_loops() -> None:
    # Anonymous containers contribute no path segment, so they have no
    # addressable condition either.
    assert collect_orchestration_condition_paths(_conditional_orch()) == {
        "named_cond.if",
        "named_cond.condition",
        "named_loop.while",
        "named_loop.condition",
    }


@pytest.mark.parametrize(
    "condition_path",
    ["named_cond.if", "named_cond.condition", "named_loop.while"],
)
def test_load_profile_rejects_disabling_a_condition_with_actionable_error(
    tmp_path: Path, condition_path: str
) -> None:
    orch_path = _orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "fast.yml",
        f"effects:\n  {condition_path}:\n    enabled: false\n",
    )
    with pytest.raises(ProfileValidationError) as exc_info:
        load_profile(
            name="fast", orchestration_path=orch_path, orch=_conditional_orch()
        )

    message = str(exc_info.value)
    assert condition_path in message
    # Actionable: says why, and names the container to disable instead.
    assert "cannot be disabled" in message
    assert condition_path.rsplit(".", 1)[0] in message


def test_load_profile_still_allows_disabling_the_container_itself(
    tmp_path: Path,
) -> None:
    orch_path = _orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "fast.yml",
        "effects:\n  named_cond:\n    enabled: false\n",
    )
    profile = load_profile(
        name="fast", orchestration_path=orch_path, orch=_conditional_orch()
    )
    assert profile.effects["named_cond"] == {"enabled": False}
