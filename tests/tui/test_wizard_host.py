"""The host half of the chat view: the loop, the verdict, and the two saves.

None of this needs a running app — that is the point of keeping it out of the
screen. What it does need to stay true to is the wizard's declared contract and
the curation manifest schema, both of which are asserted against the real
files rather than restated here.
"""

from __future__ import annotations

import json
from pathlib import Path

import jsonschema
import pytest
import yaml

from circuitry.tui.wizard_host import (
    CATEGORIES,
    TURN_PATHS,
    Conversation,
    DraftStatus,
    InvalidDraft,
    Seed,
    Turn,
    default_library_dir,
    dig,
    manifest_entry,
    run_turn,
    save_to_file,
    save_to_library,
    slugify,
    validate_draft,
    wizard_path,
)

from scripted_wizard import INVALID_DRAFT, VALID_DRAFT

MANIFEST_SCHEMA = Path("src/circuitry/schema/curation-manifest.schema.json")

SEED = Seed(name="Summarize And Translate", category="recipes", goal="Summarize, then translate.")


# ── the turn contract ────────────────────────────────────────────────────────


def test_turn_paths_are_the_paths_the_wizard_declares() -> None:
    """The one part of the wizard a host copies verbatim. Check, don't trust."""
    interface = yaml.safe_load(wizard_path().read_text(encoding="utf-8"))["interface"]
    declared = {name: spec["path"] for name, spec in interface["outputs"].items()}
    assert TURN_PATHS == declared


def test_dig_walks_and_misses_cleanly() -> None:
    state = {"prime": {"turn": {"decide": {"done": {"value": True}}}}}
    assert dig(state, "prime.turn.decide.done.value") is True
    assert dig(state, "prime.turn.decide.check.value.ok") is None
    assert dig(state, "nope") is None


def test_turn_from_state_unpacks_the_contract() -> None:
    state = {
        "prime": {
            "turn": {
                "decide": {
                    "respond": {"value": {"say": "Here you go."}},
                    "check": {"value": {"yaml": "effects: []", "ok": True, "errors": []}},
                    "done": {"value": True},
                }
            }
        }
    }
    turn = Turn.from_state(state)
    assert (turn.say, turn.yaml, turn.done, turn.valid) == ("Here you go.", "effects: []", True, True)


def test_turn_from_state_treats_a_question_turn_as_draftless() -> None:
    state = {
        "prime": {
            "turn": {
                "decide": {
                    "respond": {"value": {"say": "Which language?"}},
                    "check": {"value": None},
                    "done": {"value": False},
                }
            }
        }
    }
    turn = Turn.from_state(state)
    assert turn.yaml is None and turn.done is False and turn.valid is None


def test_run_turn_passes_the_host_state_and_adapter_through(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    seen: dict = {}

    class _Result:
        state: dict = {}

    def fake_run(**kwargs: object) -> object:
        seen.update(kwargs)
        return _Result()

    monkeypatch.setattr("circuitry.api.run_orchestration", fake_run)
    sentinel = object()
    run_turn({"goal": "g", "conversation": [], "draft": ""}, adapter=sentinel)  # type: ignore[arg-type]

    assert seen["state"] == {"goal": "g", "conversation": [], "draft": ""}
    assert seen["adapter"] is sentinel
    assert seen["orchestration_path"] == wizard_path()


# ── the seed form ────────────────────────────────────────────────────────────


@pytest.mark.parametrize(
    ("name", "expected"),
    [
        ("Summarize And Translate", "summarize_and_translate"),
        ("  spaced  out  ", "spaced_out"),
        ("weird!!chars", "weird_chars"),
        ("3 blind mice", "o_3_blind_mice"),
        ("!!!", ""),
    ],
)
def test_slugify_produces_manifest_legal_slugs(name: str, expected: str) -> None:
    assert slugify(name) == expected


def test_seed_reports_every_problem_at_once() -> None:
    problems = Seed(name="", category="nope", goal="").problems()
    assert len(problems) == 3
    assert SEED.problems() == ()


def test_seed_categories_are_the_ones_the_manifest_schema_allows() -> None:
    schema = json.loads(MANIFEST_SCHEMA.read_text(encoding="utf-8"))
    allowed = schema["$defs"]["Entry"]["properties"]["category"]["enum"]
    assert set(CATEGORIES) == set(allowed)


# ── the verdict ──────────────────────────────────────────────────────────────


def test_validate_draft_passes_a_real_orchestration() -> None:
    status = validate_draft(VALID_DRAFT)
    assert status.ok and status.errors == ()
    assert status.headline() == "✔ Valid"


def test_validate_draft_rejects_a_reserved_name() -> None:
    status = validate_draft(INVALID_DRAFT)
    assert not status.ok and status.errors
    assert status.headline().startswith("✘")


def test_validate_draft_rejects_an_empty_draft() -> None:
    assert not validate_draft("   ").ok


def test_validate_draft_agrees_with_cof_check(tmp_path: Path) -> None:
    """The pane's verdict has to be the verdict on the file it would write."""
    from circuitry.cli.runtime_shim import validate

    path = tmp_path / "draft.yml"
    path.write_text(VALID_DRAFT, encoding="utf-8")
    assert validate(path, skip_preflight=True)["ok"] is validate_draft(VALID_DRAFT).ok


# ── the conversation ─────────────────────────────────────────────────────────


def test_conversation_accumulates_transcript_and_draft() -> None:
    convo = Conversation(SEED)
    assert convo.state() == {"goal": SEED.goal, "conversation": [], "draft": ""}

    convo.record(Turn(say="Which language?"))
    convo.add_user("French.")
    convo.record(Turn(say="Built it.", yaml=VALID_DRAFT, done=True))

    state = convo.state()
    assert [m["role"] for m in state["conversation"]] == ["wizard", "user", "wizard"]
    assert state["draft"] == VALID_DRAFT
    assert convo.done and convo.can_save


def test_conversation_revalidates_rather_than_believing_the_wizard() -> None:
    """`valid: True` from the model does not make a draft saveable."""
    convo = Conversation(SEED)
    convo.record(Turn(say="Shipped it.", yaml=INVALID_DRAFT, done=True, valid=True))

    assert convo.status is not None and not convo.status.ok
    assert not convo.can_save
    assert not convo.done  # a draft that fails the gate cannot end the session


def test_conversation_without_a_draft_cannot_be_saved() -> None:
    convo = Conversation(SEED)
    convo.record(Turn(say="Which language?"))
    assert convo.status is None and not convo.can_save


# ── saving ───────────────────────────────────────────────────────────────────


def test_save_to_file_writes_a_trailing_newline(tmp_path: Path) -> None:
    path = save_to_file(VALID_DRAFT.rstrip("\n"), tmp_path / "nested" / "out.yml")
    assert path.read_text(encoding="utf-8") == VALID_DRAFT


def test_save_to_file_refuses_an_invalid_draft(tmp_path: Path) -> None:
    target = tmp_path / "out.yml"
    with pytest.raises(InvalidDraft):
        save_to_file(INVALID_DRAFT, target)
    assert not target.exists()


def test_save_to_library_refuses_an_invalid_draft(tmp_path: Path) -> None:
    with pytest.raises(InvalidDraft):
        save_to_library(INVALID_DRAFT, SEED, library_dir=tmp_path)
    assert not (tmp_path / "manifest.json").exists()


def test_save_to_library_writes_yaml_and_indexes_it(tmp_path: Path) -> None:
    saved = save_to_library(VALID_DRAFT, SEED, library_dir=tmp_path)

    assert saved.path == tmp_path / "recipes" / "summarize_and_translate.yml"
    assert saved.path.read_text(encoding="utf-8") == VALID_DRAFT
    assert saved.name == "recipes/summarize_and_translate"

    manifest = json.loads(saved.manifest_path.read_text(encoding="utf-8"))
    (entry,) = manifest["entries"]
    assert entry["file"] == "recipes/summarize_and_translate.yml"
    assert entry["intent"] == SEED.goal
    assert entry["primitives"] == ["prompt"]


def test_save_to_library_replaces_rather_than_duplicates(tmp_path: Path) -> None:
    save_to_library(VALID_DRAFT, SEED, library_dir=tmp_path)
    save_to_library(VALID_DRAFT, SEED, library_dir=tmp_path)

    manifest = json.loads((tmp_path / "manifest.json").read_text(encoding="utf-8"))
    assert len(manifest["entries"]) == 1


def test_save_to_library_keeps_other_entries(tmp_path: Path) -> None:
    (tmp_path / "manifest.json").write_text(
        json.dumps({"schema_version": "2.0.0", "entries": [{"name": "recipes/other"}]}),
        encoding="utf-8",
    )
    save_to_library(VALID_DRAFT, SEED, library_dir=tmp_path)

    manifest = json.loads((tmp_path / "manifest.json").read_text(encoding="utf-8"))
    assert [e["name"] for e in manifest["entries"]] == [
        "recipes/other",
        "recipes/summarize_and_translate",
    ]


def test_save_to_library_survives_a_corrupt_manifest(tmp_path: Path) -> None:
    (tmp_path / "manifest.json").write_text("{not json", encoding="utf-8")
    saved = save_to_library(VALID_DRAFT, SEED, library_dir=tmp_path)

    manifest = json.loads(saved.manifest_path.read_text(encoding="utf-8"))
    assert [e["name"] for e in manifest["entries"]] == ["recipes/summarize_and_translate"]


# ── the generated entry has to pass what the bundled library passes ──────────


def test_generated_manifest_passes_the_curation_schema(tmp_path: Path) -> None:
    saved = save_to_library(VALID_DRAFT, SEED, library_dir=tmp_path)
    manifest = json.loads(saved.manifest_path.read_text(encoding="utf-8"))

    schema = json.loads(MANIFEST_SCHEMA.read_text(encoding="utf-8"))
    errors = sorted(jsonschema.Draft7Validator(schema).iter_errors(manifest), key=str)
    assert not errors, "\n".join(e.message for e in errors)


def test_generated_manifest_passes_the_curation_metadata_rules(tmp_path: Path) -> None:
    """The same four invariants `tests/orchestrations/test_curation_metadata.py`
    asserts about the bundled library, applied to a generated one."""
    save_to_library(VALID_DRAFT, SEED, library_dir=tmp_path)
    manifest = json.loads((tmp_path / "manifest.json").read_text(encoding="utf-8"))

    on_disk = {str(p.relative_to(tmp_path)) for p in tmp_path.rglob("*.yml")}
    listed = {entry["file"] for entry in manifest["entries"]}
    assert on_disk == listed  # no orphans, no stale entries

    for entry in manifest["entries"]:
        file_path = Path(entry["file"])
        assert (tmp_path / file_path).exists()
        assert entry["category"] == entry["file"].split("/", 1)[0]
        assert entry["name"] == f"{file_path.parent.as_posix()}/{file_path.stem}"


def test_generated_entry_reads_the_interface_off_the_draft() -> None:
    draft = (
        "interface:\n"
        "  inputs:\n"
        "    article: {type: string, required: true}\n"
        "  outputs:\n"
        "    summary: {type: string, path: prime.summarize.value}\n"
        "effects:\n"
        "  - type: prompt\n"
        "    name: summarize\n"
        '    template: "Summarize {{article}}"\n'
    )
    entry = manifest_entry(draft, SEED)
    assert entry["inputs"] == {"article": {"type": "string", "required": True}}
    assert entry["outputs"]["summary"]["path"] == "prime.summarize.value"


def test_generated_entry_names_only_real_primitives() -> None:
    """A JSON Schema inside a prompt has `type:` keys that are not effects."""
    draft = (
        "effects:\n"
        "  - type: prompt\n"
        "    name: classify\n"
        "    prompt_type: object\n"
        "    schema:\n"
        "      type: object\n"
        "      properties:\n"
        "        label: {type: string}\n"
        '    template: "Classify it. Return ONLY JSON."\n'
        "  - type: tool\n"
        "    name: save\n"
        "    provider: json\n"
    )
    assert manifest_entry(draft, SEED)["primitives"] == ["prompt", "tool"]


# ── where the library is ─────────────────────────────────────────────────────


def test_default_library_dir_prefers_a_configured_folder_source(tmp_path: Path) -> None:
    from circuitry.cli.config import CircuitryConfig

    config = CircuitryConfig(
        runtime={
            "library": {
                "sources": [{"type": "curation"}, {"type": "folder", "path": str(tmp_path)}]
            }
        }
    )
    assert default_library_dir(config) == tmp_path


def test_default_library_dir_falls_back_when_a_config_is_broken() -> None:
    from circuitry.cli.config import CircuitryConfig

    broken = CircuitryConfig(runtime={"library": {"sources": [{"no": "type"}]}})
    assert default_library_dir(broken) == Path.home() / ".circuitry" / "library"


def test_draft_status_headline_counts_problems() -> None:
    assert DraftStatus(False, ("one",)).headline() == "✘ 1 problem"
    assert DraftStatus(False, ("one", "two")).headline() == "✘ 2 problems"
