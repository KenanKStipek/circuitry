"""Profile editing rules: the effect tree, the draft, and what it writes.

Textual-free — these are the rules a profile obeys, tested without booting
an app. Widget behaviour lives in ``test_profile_view.py``.

The load-bearing test here is ``test_saved_profile_round_trips``: what the
editor writes has to be a plain profile document, read back by the *engine's*
loader with nothing TUI-shaped about it.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.cli.profiles import (
    ProfileValidationError,
    load_profile,
)
from circuitry.tui.profile_edit import (
    BACKENDS,
    DEFAULT_PROFILE_NAME,
    EffectOverride,
    PersistenceDraft,
    ProfileDraft,
    backend_by_name,
    build_effect_tree,
    condition_refusal,
    discover_profiles,
    load_draft,
    profile_dir_for,
    valid_profile_name,
)

#: A fixture orchestration with one of everything a profile can address:
#: a plain effect, a reflector with a child, a named conditional (whose
#: condition is off limits) and a named loop.
FIXTURE: dict[str, Any] = {
    "name": "fixture",
    "adapter": "echo",
    "model": "echo-1",
    "interface": {
        "inputs": {
            "topic": {"type": "string", "required": True, "description": "Subject."},
            "depth": {"type": "number", "default": 2},
        }
    },
    "effects": [
        {"type": "prompt", "name": "summarize", "template": "{{topic}}"},
        {
            "type": "reflector",
            "name": "planner",
            "effects": [
                {"type": "prompt", "name": "draft", "template": "{{topic}}"},
            ],
        },
        {
            "type": "conditional",
            "name": "gate",
            "if": {"mode": "cel", "expr": "true"},
            "then": [{"type": "prompt", "name": "deep", "template": "x"}],
        },
        {
            "type": "loop",
            "name": "refine",
            "while": {"mode": "cel", "expr": "false"},
            "body": [{"type": "prompt", "name": "pass", "template": "y"}],
        },
    ],
}


@pytest.fixture
def orchestration(tmp_path: Path) -> Path:
    path = tmp_path / "fixture.yml"
    path.write_text(yaml.safe_dump(FIXTURE), encoding="utf-8")
    return path


# -- the effect tree ----------------------------------------------------------


def test_tree_lists_every_overridable_path_in_declaration_order() -> None:
    tree = build_effect_tree(FIXTURE)
    assert [node.path for node in tree if node.overridable] == [
        "summarize",
        "planner",
        "planner.draft",
        "gate",
        "gate.deep",
        "refine",
        "refine.pass",
    ]


def test_tree_paths_agree_with_the_engine() -> None:
    """The editor and the profile validator must address the same effects."""
    from circuitry.cli.profiles import collect_orchestration_effect_paths

    tree = build_effect_tree(FIXTURE)
    assert {node.path for node in tree if node.overridable} == (
        collect_orchestration_effect_paths(FIXTURE)
    )


def test_tree_nests_children_under_their_container() -> None:
    depths = {node.path: node.depth for node in build_effect_tree(FIXTURE)}
    assert depths["summarize"] == 0
    assert depths["planner"] == 0
    assert depths["planner.draft"] == 1
    assert depths["gate.deep"] == 1


def test_reflectors_are_flagged() -> None:
    planner = next(n for n in build_effect_tree(FIXTURE) if n.path == "planner")
    assert planner.is_reflector
    assert "reflector" in planner.row_label()


def test_condition_rows_render_but_are_not_overridable() -> None:
    conditions = [n for n in build_effect_tree(FIXTURE) if n.kind == "condition"]
    assert [n.path for n in conditions] == ["gate.if", "refine.while"]
    assert not any(n.overridable for n in conditions)
    assert "not overridable" in conditions[0].row_label()


def test_anonymous_containers_are_transparent_but_visible() -> None:
    orch = {
        "effects": [
            {
                "type": "conditional",
                "if": {"mode": "cel", "expr": "true"},
                "then": [{"type": "prompt", "name": "inner", "template": "x"}],
            }
        ]
    }
    tree = build_effect_tree(orch)
    assert [(n.path, n.kind) for n in tree] == [("", "group"), ("inner", "effect")]
    # No path segment of its own — "inner", not "<anonymous>.inner".
    assert tree[1].depth == 1


# -- the draft ----------------------------------------------------------------


def test_empty_draft_writes_nothing_and_is_not_dirty() -> None:
    draft = ProfileDraft()
    assert draft.to_mapping() == {}
    assert not draft.dirty


def test_setting_an_override_makes_it_dirty() -> None:
    draft = ProfileDraft(name="fast")
    draft.set_model("summarize", "tier-1")
    assert draft.dirty
    draft.mark_clean()
    assert not draft.dirty


def test_enabled_true_is_dropped_because_it_is_the_default() -> None:
    draft = ProfileDraft()
    draft.set_enabled("summarize", True)
    assert draft.to_mapping() == {}
    draft.set_enabled("summarize", False)
    assert draft.to_mapping() == {"effects": {"summarize": {"enabled": False}}}
    assert not draft.is_enabled("summarize")


def test_clearing_every_field_removes_the_effect_entry() -> None:
    draft = ProfileDraft()
    draft.set_model("summarize", "tier-1")
    draft.set_model("summarize", None)
    assert "summarize" not in draft.effects


def test_duplicate_copies_content_under_a_new_name() -> None:
    draft = ProfileDraft(name="fast")
    draft.set_model("summarize", "tier-1")
    draft.mark_clean()
    copy = draft.duplicate("thorough")
    assert copy.name == "thorough"
    assert copy.effects == draft.effects
    assert copy.dirty  # never saved under this name


@pytest.mark.parametrize(
    ("name", "ok"),
    [
        ("fast", True),
        ("my.profile-2", True),
        ("", False),
        ("../escape", False),
        ("has space", False),
    ],
)
def test_profile_names_have_to_be_filename_safe(name: str, ok: bool) -> None:
    assert valid_profile_name(name) is ok


# -- refusals -----------------------------------------------------------------


def test_condition_refusal_is_the_validators_own_message(orchestration: Path) -> None:
    """AC: the editor's refusal is the validator's, word for word."""
    directory = profile_dir_for(orchestration)
    directory.mkdir(parents=True)
    (directory / "bad.yml").write_text(
        yaml.safe_dump({"effects": {"gate.if": {"enabled": False}}}), encoding="utf-8"
    )

    with pytest.raises(ProfileValidationError) as caught:
        load_profile(name="bad", orchestration_path=orchestration, orch=FIXTURE)

    assert condition_refusal("gate.if", profile_name="bad") == str(caught.value)


def test_problems_flags_a_condition_override() -> None:
    draft = ProfileDraft(name="bad")
    draft.effects["refine.while"] = EffectOverride(enabled=False)
    problems = draft.problems(FIXTURE)
    assert problems == [condition_refusal("refine.while", profile_name="bad")]


def test_problems_flags_a_backend_missing_its_required_key() -> None:
    draft = ProfileDraft(name="fast", persistence=PersistenceDraft(backend="sqlite"))
    assert "needs path" in draft.problems(FIXTURE)[0]


def test_problems_flags_an_unusable_name() -> None:
    assert "not a usable profile name" in ProfileDraft(name="../x").problems(FIXTURE)[0]


# -- orphans ------------------------------------------------------------------


def test_orphan_overrides_are_listed_and_droppable() -> None:
    """AC: unknown/renamed effects are surfaced with a one-key cleanup."""
    tree = build_effect_tree(FIXTURE)
    draft = ProfileDraft(name="stale")
    draft.set_model("summarize", "tier-1")
    draft.set_model("summarise", "tier-1")  # renamed away
    draft.set_enabled("gone.child", False)

    assert draft.orphans(tree) == ["gone.child", "summarise"]
    assert draft.drop_orphans(tree) == ["gone.child", "summarise"]
    assert draft.orphans(tree) == []
    assert set(draft.effects) == {"summarize"}


def test_a_profile_with_orphans_still_opens(orchestration: Path) -> None:
    """The loader would refuse it; the editor has to show it to fix it."""
    directory = profile_dir_for(orchestration)
    directory.mkdir(parents=True)
    (directory / "stale.yml").write_text(
        yaml.safe_dump({"effects": {"summarise": {"model": "tier-1"}}}),
        encoding="utf-8",
    )

    with pytest.raises(ProfileValidationError):
        load_profile(name="stale", orchestration_path=orchestration, orch=FIXTURE)

    draft = load_draft("stale", orchestration_path=orchestration)
    assert draft.orphans(build_effect_tree(FIXTURE)) == ["summarise"]


# -- persistence --------------------------------------------------------------


def test_every_documented_backend_is_offered() -> None:
    assert {spec.name for spec in BACKENDS} == {
        "jsonl-file",
        "sqlite",
        "mongodb",
        "postgres",
    }


def test_backend_blank_values_are_left_out_of_the_file() -> None:
    draft = ProfileDraft(
        name="fast",
        persistence=PersistenceDraft(
            backend="sqlite", values={"path": "runs.db", "table": "  "}
        ),
    )
    assert draft.to_mapping()["persistence"] == {"backend": "sqlite", "path": "runs.db"}


def test_backend_lookup() -> None:
    assert backend_by_name("sqlite") is not None
    assert backend_by_name("nope") is None


# -- discovery and io ---------------------------------------------------------


def test_save_lands_where_the_cli_looks(orchestration: Path) -> None:
    draft = ProfileDraft(name="fast")
    draft.set_model("summarize", "tier-1")
    path = draft.save(profile_dir_for(orchestration))
    assert path == orchestration.parent / "profiles" / "fast.yml"
    assert not draft.dirty


def test_discovery_prefers_orchestration_scoped_profiles(
    tmp_path: Path, orchestration: Path
) -> None:
    orch_scoped = profile_dir_for(orchestration)
    orch_scoped.mkdir(parents=True)
    (orch_scoped / "fast.yml").write_text("{}\n", encoding="utf-8")

    project = tmp_path / "project" / "profiles"
    project.mkdir(parents=True)
    (project / "fast.yml").write_text("{}\n", encoding="utf-8")
    (project / "other.yml").write_text("{}\n", encoding="utf-8")

    found = dict(discover_profiles(orchestration, cwd=tmp_path / "project"))
    assert found["fast"] == orch_scoped / "fast.yml"
    assert found["other"] == project / "other.yml"


def test_default_name_is_a_usable_filename() -> None:
    assert valid_profile_name(DEFAULT_PROFILE_NAME)


# -- the round trip -----------------------------------------------------------


def test_saved_profile_round_trips_through_the_real_loader(orchestration: Path) -> None:
    """AC: a TUI-saved profile is byte-compatible with the engine."""
    draft = ProfileDraft(name="thorough")
    draft.adapter = "echo"
    draft.model = "tier-4"
    draft.inputs = {"topic": "circuit design", "depth": 5}
    draft.set_model("summarize", "tier-1")
    draft.set_provider("summarize", "cyberdiner")
    draft.set_enabled("planner", False)
    draft.persistence = PersistenceDraft(
        backend="jsonl-file", values={"path": "runs.jsonl"}
    )
    path = draft.save(profile_dir_for(orchestration))

    loaded = load_profile(
        name="thorough", orchestration_path=orchestration, orch=FIXTURE
    )

    assert loaded.path == path
    assert loaded.adapter == "echo"
    assert loaded.model == "tier-4"
    assert loaded.inputs == {"topic": "circuit design", "depth": 5}
    assert loaded.effects == {
        "summarize": {"model": "tier-1", "provider": "cyberdiner"},
        "planner": {"enabled": False},
    }
    assert loaded.persistence == {"backend": "jsonl-file", "path": "runs.jsonl"}
    # And the document itself is ordinary YAML, not a Python-tagged dump.
    assert "!!python" not in path.read_text(encoding="utf-8")


def test_reopening_a_saved_profile_is_a_fixed_point(orchestration: Path) -> None:
    draft = ProfileDraft(name="fast")
    draft.model = "tier-1"
    draft.set_enabled("planner", False)
    draft.persistence = PersistenceDraft(backend="sqlite", values={"path": "runs.db"})
    draft.save(profile_dir_for(orchestration))

    reopened = load_draft("fast", orchestration_path=orchestration)
    assert not reopened.dirty
    assert reopened.to_yaml() == draft.to_yaml()


def test_an_unknown_persistence_key_survives_a_round_trip(orchestration: Path) -> None:
    """`db_path` is a legal sqlite alias; the editor must not eat it."""
    directory = profile_dir_for(orchestration)
    directory.mkdir(parents=True)
    (directory / "aliased.yml").write_text(
        yaml.safe_dump({"persistence": {"backend": "sqlite", "db_path": "runs.db"}}),
        encoding="utf-8",
    )
    draft = load_draft("aliased", orchestration_path=orchestration)
    assert draft.to_mapping()["persistence"] == {
        "backend": "sqlite",
        "db_path": "runs.db",
    }
