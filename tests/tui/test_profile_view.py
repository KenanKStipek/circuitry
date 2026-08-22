"""Pilot tests for the Profile view: pick, override, toggle, save, hand off.

The pilot builds "fast" and "thorough" against a fixture orchestration and
then runs one of them through the real ``runtime_shim.run`` — so a green
test means the whole path (widgets → draft → file → engine loader → applied
overrides) actually works, not just that the widgets render.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pytest
import yaml

pytest.importorskip("textual")

from textual.pilot import Pilot
from textual.widgets import Input, Select, Static, Switch

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest
from circuitry.cli.runtime_shim import run as shim_run
from circuitry.tui.app import CircuitryApp
from circuitry.tui.launch import OrchestrationChoice
from circuitry.tui.profile_edit import (
    NO_OVERRIDE,
    ProfileDraft,
    condition_refusal,
    profile_dir_for,
)
from circuitry.tui.profile_view import (
    NEW_PROFILE,
    NO_PERSISTENCE,
    ConfirmDiscard,
    ProfileScreen,
)
from circuitry.tui.screens import VIEWS

PROFILE_SPEC = next(spec for spec in VIEWS if spec.slug == "profiles")

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
            "effects": [{"type": "prompt", "name": "draft", "template": "{{topic}}"}],
        },
        {
            "type": "conditional",
            "name": "gate",
            "if": {"mode": "cel", "expr": "true"},
            "then": [{"type": "prompt", "name": "deep", "template": "x"}],
        },
    ],
}

BROKEN: dict[str, Any] = {
    "name": "broken",
    "effects": [{"type": "prompt", "template": "nameless prompts do not compile"}],
}


@dataclass(frozen=True)
class EchoAdapter:
    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=f"{model}:{prompt}", raw={"model": model})


def _write(tmp_path: Path, orch: dict[str, Any], name: str = "fixture.yml") -> Path:
    path = tmp_path / name
    path.write_text(yaml.dump(orch, sort_keys=False), encoding="utf-8")
    return path


def _choice(path: Path) -> OrchestrationChoice:
    return OrchestrationChoice(key=str(path), label=path.name, path=path, source="local")


def _screen(path: Path, *, cwd: Path | None = None, **kwargs: Any) -> ProfileScreen:
    return ProfileScreen(
        PROFILE_SPEC,
        config=CircuitryConfig(),
        choices=[_choice(path)],
        cwd=cwd if cwd is not None else path.parent,
        **kwargs,
    )


async def _open(pilot: Pilot[Any], screen: ProfileScreen) -> ProfileScreen:
    await pilot.app.push_screen(screen)
    await pilot.pause()
    screen.query_one("#profile-orchestration", Select).value = screen._choices[0].key
    await pilot.pause()
    await pilot.pause()
    return screen


def _row_index(screen: ProfileScreen, path: str) -> int:
    return next(i for i, node in enumerate(screen._rows) if node.path == path)


async def _set(pilot: Pilot[Any], widget: Any, value: Any) -> None:
    widget.value = value
    await pilot.pause()
    await pilot.pause()


# -- the tree ------------------------------------------------------------------


def test_selecting_an_orchestration_renders_its_effect_tree(
    run_app: Any, tmp_path: Path
) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[list[str], str]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        return [node.path for node in screen._rows], screen.status_text

    paths, status = run_app(scenario)
    assert paths == [
        "summarize",
        "planner",
        "planner.draft",
        "gate",
        "gate.if",
        "gate.deep",
    ]
    assert "5 overridable effects" in status


def test_an_orchestration_that_does_not_compile_says_so(
    run_app: Any, tmp_path: Path
) -> None:
    """The tree comes from the validate-only path, so a bad file is caught."""

    async def scenario(pilot: Pilot[Any]) -> tuple[str, list[Any]]:
        screen = await _open(pilot, _screen(_write(tmp_path, BROKEN)))
        return screen.status_text, screen._rows

    status, rows = run_app(scenario)
    assert "does not compile" in status
    assert rows == []


def test_every_overridable_row_gets_pickers_and_a_toggle(
    run_app: Any, tmp_path: Path
) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[int, int]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        index = _row_index(screen, "summarize")
        screen.query_one(f"#fx-provider-{index}", Select)
        screen.query_one(f"#fx-model-{index}", Select)
        screen.query_one(f"#fx-custom-{index}", Input)
        screen.query_one(f"#fx-enabled-{index}", Switch)
        return len(list(screen.query(Switch).results(Switch))), len(screen._rows)

    switches, rows = run_app(scenario)
    # One per row, condition rows included — they render, they just refuse.
    assert switches == rows


# -- editing -------------------------------------------------------------------


def test_picking_a_model_writes_it_into_the_draft(run_app: Any, tmp_path: Path) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[dict[str, Any], bool]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        index = _row_index(screen, "summarize")
        await _set(pilot, screen.query_one(f"#fx-model-{index}", Select), "echo-1")
        return screen.draft.to_mapping(), screen.draft.dirty

    mapping, dirty = run_app(scenario)
    assert mapping["effects"] == {"summarize": {"model": "echo-1"}}
    assert dirty


def test_custom_reveals_a_free_text_box_that_wins(run_app: Any, tmp_path: Path) -> None:
    from circuitry.tui.profile_edit import CUSTOM

    async def scenario(pilot: Pilot[Any]) -> tuple[bool, dict[str, Any]]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        index = _row_index(screen, "summarize")
        await _set(pilot, screen.query_one(f"#fx-model-{index}", Select), CUSTOM)
        box = screen.query_one(f"#fx-custom-{index}", Input)
        await _set(pilot, box, "an-unlisted-model")
        return box.display, screen.draft.to_mapping()

    shown, mapping = run_app(scenario)
    assert shown
    assert mapping["effects"] == {"summarize": {"model": "an-unlisted-model"}}


def test_toggling_an_effect_off_records_enabled_false(
    run_app: Any, tmp_path: Path
) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[dict[str, Any], str]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        index = _row_index(screen, "planner")
        await _set(pilot, screen.query_one(f"#fx-enabled-{index}", Switch), False)
        return screen.draft.to_mapping(), screen.status_text

    mapping, status = run_app(scenario)
    assert mapping["effects"] == {"planner": {"enabled": False}}
    assert "whole subtree" in status


def test_toggling_a_condition_is_refused_in_the_validators_words(
    run_app: Any, tmp_path: Path
) -> None:
    """AC: the same message the validator gives, and the switch springs back."""

    async def scenario(pilot: Pilot[Any]) -> tuple[str, bool, dict[str, Any]]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        screen.draft.name = "fast"
        index = _row_index(screen, "gate.if")
        switch = screen.query_one(f"#fx-enabled-{index}", Switch)
        await _set(pilot, switch, False)
        return screen.status_text, switch.value, screen.draft.to_mapping()

    status, still_on, mapping = run_app(scenario)
    assert status == condition_refusal("gate.if", profile_name="fast")
    assert still_on
    assert mapping == {}


def test_condition_rows_have_their_pickers_disabled(
    run_app: Any, tmp_path: Path
) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[bool, bool]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        index = _row_index(screen, "gate.if")
        return (
            screen.query_one(f"#fx-provider-{index}", Select).disabled,
            screen.query_one(f"#fx-model-{index}", Select).disabled,
        )

    assert run_app(scenario) == (True, True)


def test_inputs_panel_only_records_what_was_typed(run_app: Any, tmp_path: Path) -> None:
    """An untouched optional input must not bake its default into the file."""

    async def scenario(pilot: Pilot[Any]) -> dict[str, Any]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        await _set(pilot, screen.query_one("#pin-0", Input), "circuit design")
        return screen.draft.to_mapping()

    assert run_app(scenario)["inputs"] == {"topic": "circuit design"}


def test_a_mistyped_input_blocks_the_save(run_app: Any, tmp_path: Path) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[Any, str]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        await _set(pilot, screen.query_one("#pin-1", Input), "not-a-number")
        return screen.action_save(), screen.status_text

    saved, status = run_app(scenario)
    assert saved is None
    assert "expected a number" in status


# -- persistence ----------------------------------------------------------------


def test_choosing_a_backend_reveals_its_fields(run_app: Any, tmp_path: Path) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[list[str], dict[str, Any]]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        await _set(pilot, screen.query_one("#persistence-backend", Select), "sqlite")
        await _set(pilot, screen.query_one("#pers-path", Input), "runs.db")
        keys = [f.key for f in screen._backend_fields]
        return keys, screen.draft.to_mapping()

    keys, mapping = run_app(scenario)
    assert keys == ["path", "table"]
    assert mapping["persistence"] == {"backend": "sqlite", "path": "runs.db"}


def test_switching_backend_drops_the_previous_backends_keys(
    run_app: Any, tmp_path: Path
) -> None:
    async def scenario(pilot: Pilot[Any]) -> dict[str, Any]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        backend = screen.query_one("#persistence-backend", Select)
        await _set(pilot, backend, "mongodb")
        await _set(pilot, screen.query_one("#pers-uri", Input), "mongodb://host")
        await _set(pilot, backend, "sqlite")
        return screen.draft.to_mapping()

    assert run_app(scenario)["persistence"] == {"backend": "sqlite"}


def test_a_backend_without_its_required_key_blocks_the_save(
    run_app: Any, tmp_path: Path
) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[Any, str]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        await _set(pilot, screen.query_one("#persistence-backend", Select), "sqlite")
        return screen.action_save(), screen.status_text

    saved, status = run_app(scenario)
    assert saved is None
    assert "needs path" in status


# -- save, switch, duplicate -----------------------------------------------------


def test_saving_writes_where_the_cli_looks_and_clears_dirty(
    run_app: Any, tmp_path: Path
) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[Any, bool, str]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        await _set(pilot, screen.query_one("#profile-name", Input), "fast")
        index = _row_index(screen, "summarize")
        await _set(pilot, screen.query_one(f"#fx-model-{index}", Select), "echo-1")
        path = screen.action_save()
        return path, screen.draft.dirty, screen.status_text

    path, dirty, status = run_app(scenario)
    assert path == tmp_path / "profiles" / "fast.yml"
    assert not dirty
    assert "--profile fast" in status


def test_saved_profiles_show_up_in_the_picker_and_can_be_switched_back_to(
    run_app: Any, tmp_path: Path
) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[list[Any], dict[str, Any]]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        await _set(pilot, screen.query_one("#profile-name", Input), "fast")
        index = _row_index(screen, "summarize")
        await _set(pilot, screen.query_one(f"#fx-model-{index}", Select), "echo-1")
        screen.action_save()
        await pilot.pause()

        picker = screen.query_one("#profile-picker", Select)
        options = [value for _, value in picker._options]
        await _set(pilot, picker, NEW_PROFILE)
        await _set(pilot, picker, "fast")
        return options, screen.draft.to_mapping()

    options, mapping = run_app(scenario)
    assert options == [NEW_PROFILE, "fast"]
    assert mapping["effects"] == {"summarize": {"model": "echo-1"}}


def test_duplicate_gives_a_fresh_unsaved_name(run_app: Any, tmp_path: Path) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[str, dict[str, Any], bool]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        await _set(pilot, screen.query_one("#profile-name", Input), "fast")
        index = _row_index(screen, "summarize")
        await _set(pilot, screen.query_one(f"#fx-model-{index}", Select), "echo-1")
        screen.action_save()
        screen.action_save_as()
        await pilot.pause()
        return screen.draft.name, screen.draft.to_mapping(), screen.draft.dirty

    name, mapping, dirty = run_app(scenario)
    assert name == "fast-copy"
    assert mapping["effects"] == {"summarize": {"model": "echo-1"}}
    assert dirty


def test_orphan_overrides_are_listed_and_droppable_from_the_view(
    run_app: Any, tmp_path: Path
) -> None:
    orchestration = _write(tmp_path, FIXTURE)
    directory = profile_dir_for(orchestration)
    directory.mkdir(parents=True)
    (directory / "stale.yml").write_text(
        yaml.safe_dump({"effects": {"summarise": {"model": "echo-1"}}}),
        encoding="utf-8",
    )

    async def scenario(pilot: Pilot[Any]) -> tuple[str, str, dict[str, Any]]:
        screen = await _open(pilot, _screen(orchestration))
        await _set(pilot, screen.query_one("#profile-picker", Select), "stale")
        listed = screen.status_text
        rendered = "\n".join(
            str(node.render())
            for node in screen.query("#profile-orphans Static").results(Static)
        )
        screen.action_drop_orphans()
        await pilot.pause()
        return listed, rendered, screen.draft.to_mapping()

    status, rendered, mapping = run_app(scenario)
    assert "no longer exist" in status
    assert "summarise" in rendered
    assert mapping == {}


def test_saving_refuses_while_orphans_are_present(run_app: Any, tmp_path: Path) -> None:
    orchestration = _write(tmp_path, FIXTURE)
    directory = profile_dir_for(orchestration)
    directory.mkdir(parents=True)
    (directory / "stale.yml").write_text(
        yaml.safe_dump({"effects": {"summarise": {"model": "echo-1"}}}),
        encoding="utf-8",
    )

    async def scenario(pilot: Pilot[Any]) -> tuple[Any, str]:
        screen = await _open(pilot, _screen(orchestration))
        await _set(pilot, screen.query_one("#profile-picker", Select), "stale")
        return screen.action_save(), screen.status_text

    saved, status = run_app(scenario)
    assert saved is None
    assert "Ctrl-O drops them" in status


# -- dirty state and leaving -----------------------------------------------------


def test_dirty_marker_tracks_the_draft(run_app: Any, tmp_path: Path) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[str, str]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        marker = screen.query_one("#profile-dirty", Static)
        clean = str(marker.render())
        index = _row_index(screen, "summarize")
        await _set(pilot, screen.query_one(f"#fx-model-{index}", Select), "echo-1")
        return clean, str(marker.render())

    clean, dirty = run_app(scenario)
    assert "saved" in clean and "unsaved" not in clean
    assert "unsaved" in dirty


def test_leaving_with_unsaved_edits_asks_first(run_app: Any, tmp_path: Path) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[bool, bool]:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        index = _row_index(screen, "summarize")
        await _set(pilot, screen.query_one(f"#fx-model-{index}", Select), "echo-1")
        screen.action_leave()
        await pilot.pause()
        asked = isinstance(pilot.app.screen_stack[-1], ConfirmDiscard)
        await pilot.press("n")
        await pilot.pause()
        still_here = pilot.app.screen_stack[-1] is screen
        return asked, still_here

    asked, still_here = run_app(scenario)
    assert asked
    assert still_here


def test_discarding_lets_the_navigation_through(run_app: Any, tmp_path: Path) -> None:
    async def scenario(pilot: Pilot[Any]) -> bool:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        index = _row_index(screen, "summarize")
        await _set(pilot, screen.query_one(f"#fx-model-{index}", Select), "echo-1")
        screen.action_leave()
        await pilot.pause()
        await pilot.press("y")
        await pilot.pause()
        await pilot.pause()
        return pilot.app.screen_stack[-1] is not screen

    assert run_app(scenario)


def test_a_clean_draft_leaves_without_a_prompt(run_app: Any, tmp_path: Path) -> None:
    async def scenario(pilot: Pilot[Any]) -> bool:
        screen = await _open(pilot, _screen(_write(tmp_path, FIXTURE)))
        screen.action_leave()
        await pilot.pause()
        await pilot.pause()
        return any(
            isinstance(s, ConfirmDiscard) for s in pilot.app.screen_stack
        )

    assert not run_app(scenario)


# -- hand-off to the launcher ------------------------------------------------------


def test_run_with_this_profile_saves_and_hands_off(run_app: Any, tmp_path: Path) -> None:
    orchestration = _write(tmp_path, FIXTURE)
    handed: list[tuple[Path, str | None]] = []

    class _App:
        pass

    async def scenario(pilot: Pilot[Any]) -> tuple[Path, list[tuple[Path, str | None]]]:
        screen = await _open(pilot, _screen(orchestration))
        # Stand in for the app's launch_run so this test stays about the
        # Profile view; the Run side is covered in test_run_view.py.
        object.__setattr__(
            screen,
            "_launch_probe",
            lambda path, profile=None: handed.append((path, profile)),
        )
        screen.app.launch_run = screen._launch_probe  # type: ignore[method-assign]
        await _set(pilot, screen.query_one("#profile-name", Input), "fast")
        index = _row_index(screen, "summarize")
        await _set(pilot, screen.query_one(f"#fx-model-{index}", Select), "echo-1")
        screen.action_run_with_profile()
        await pilot.pause()
        return orchestration, handed

    path, calls = run_app(scenario)
    assert calls == [(path, "fast")]
    assert (tmp_path / "profiles" / "fast.yml").exists()


def test_the_launcher_picks_up_a_handed_off_profile(run_app: Any, tmp_path: Path) -> None:
    """The Run view pre-loads the orchestration and applies the profile."""
    from circuitry.tui.run_view import RunScreen

    orchestration = _write(tmp_path, FIXTURE)
    ProfileDraft(name="fast").save(profile_dir_for(orchestration))

    async def scenario(pilot: Pilot[Any]) -> tuple[str | None, Any, str]:
        pilot.app.pending_run = orchestration
        pilot.app.pending_profile = "fast"
        run_spec = next(spec for spec in VIEWS if spec.slug == "run")
        screen = RunScreen(
            run_spec,
            config=CircuitryConfig(),
            choices=[],
            adapter=EchoAdapter(),
        )
        await pilot.app.push_screen(screen)
        await pilot.pause()
        await pilot.pause()
        select = screen.query_one("#run-orchestration", Select)
        return screen.profile_name, select.value, screen.status_text

    profile, selected, status = run_app(scenario)
    assert profile == "fast"
    assert selected == str(orchestration)
    assert "Profile: fast" in status


# -- the demo: two profiles, then a real run --------------------------------------


def test_pilot_builds_fast_and_thorough_and_the_engine_applies_them(
    run_app: Any, tmp_path: Path
) -> None:
    """AC/demo: build both profiles in the editor, run one from the plain path."""
    orchestration = _write(tmp_path, FIXTURE)

    async def scenario(pilot: Pilot[Any]) -> dict[str, str]:
        screen = await _open(pilot, _screen(orchestration))
        written: dict[str, str] = {}

        # "fast": cheap model everywhere, planning off.
        await _set(pilot, screen.query_one("#profile-name", Input), "fast")
        await _set(pilot, screen.query_one("#profile-model", Select), "echo-1")
        planner = _row_index(screen, "planner")
        await _set(pilot, screen.query_one(f"#fx-enabled-{planner}", Switch), False)
        written["fast"] = str(screen.action_save())

        # "thorough": a per-effect override and a persistence target.
        await _set(pilot, screen.query_one("#profile-picker", Select), NEW_PROFILE)
        await _set(pilot, screen.query_one("#profile-name", Input), "thorough")
        summarize = _row_index(screen, "summarize")
        await _set(
            pilot, screen.query_one(f"#fx-model-{summarize}", Select), "echo-1"
        )
        await _set(pilot, screen.query_one("#persistence-backend", Select), "jsonl-file")
        await _set(pilot, screen.query_one("#pers-path", Input), "runs.jsonl")
        written["thorough"] = str(screen.action_save())
        return written

    written = run_app(scenario)
    assert Path(written["fast"]).exists()
    assert Path(written["thorough"]).exists()

    # Now the plain engine path — no TUI in sight.
    result = shim_run(
        RunRequest(
            orchestration_path=orchestration,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={"topic": "circuits"},
            config=CircuitryConfig(),
            adapter=EchoAdapter(),
            profile_name="fast",
            skip_preflight=True,
        )
    )

    assert result.ok, result.error
    settings = result.state["runtime"]["effective_settings"]
    assert settings["profile"]["name"] == "fast"
    assert settings["sources"]["model"] == "profile"
    # The toggle the editor wrote is the toggle the runtime honoured.
    assert result.state["prime"]["planner"]["meta"]["disabled"] is True
    assert result.state["prime"]["summarize"]["meta"].get("disabled") is not True


def test_a_profile_the_editor_wrote_survives_the_loaders_validation(
    run_app: Any, tmp_path: Path
) -> None:
    """Nothing the editor can produce trips the engine's path validation."""
    from circuitry.cli.profiles import load_profile

    orchestration = _write(tmp_path, FIXTURE)

    async def scenario(pilot: Pilot[Any]) -> None:
        screen = await _open(pilot, _screen(orchestration))
        await _set(pilot, screen.query_one("#profile-name", Input), "everything")
        await _set(pilot, screen.query_one("#profile-adapter", Select), NO_OVERRIDE)
        for node in list(screen._rows):
            if not node.overridable:
                continue
            index = _row_index(screen, node.path)
            await _set(
                pilot, screen.query_one(f"#fx-model-{index}", Select), "echo-1"
            )
        screen.action_save()

    run_app(scenario)
    loaded = load_profile(
        name="everything", orchestration_path=orchestration, orch=FIXTURE
    )
    assert set(loaded.effects) == {
        "summarize",
        "planner",
        "planner.draft",
        "gate",
        "gate.deep",
    }


# -- snapshots ---------------------------------------------------------------------

FIXTURE_ORCHESTRATION = Path(__file__).parent / "fixtures" / "profile-tree.yml"


class _ProfileApp(CircuitryApp):
    """A shell that opens straight onto the Profile view, on a fixed fixture.

    The stock view discovers whatever orchestrations sit next to the person
    running it, which is exactly what makes it useful and exactly what makes
    it unsnapshottable. Pinning the choices (and an empty config) makes the
    frame a function of the layout alone.
    """

    def on_mount(self) -> None:
        self.push_screen(
            ProfileScreen(
                PROFILE_SPEC,
                config=CircuitryConfig(),
                choices=[_choice(FIXTURE_ORCHESTRATION)],
                cwd=FIXTURE_ORCHESTRATION.parent,
            )
        )


@pytest.mark.parametrize(
    ("name", "size"),
    [("profiles-100x30", (100, 30)), ("profiles-40x12", (40, 12))],
)
def test_profile_view_snapshot(
    run_app: Any, snapshot: Any, capture_frame: Any, name: str, size: tuple[int, int]
) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        screen = pilot.app.screen_stack[-1]
        screen.query_one("#profile-orchestration", Select).value = str(
            FIXTURE_ORCHESTRATION
        )
        await pilot.pause()
        await pilot.pause()
        return str(capture_frame(pilot.app))

    snapshot.assert_match(run_app(scenario, app=_ProfileApp(), size=size), name)


# -- registry ------------------------------------------------------------------


def test_the_view_is_reachable_by_its_number_key(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await pilot.press("9")
        await pilot.pause()
        return str(pilot.app.current_view().slug)

    assert run_app(scenario) == "profiles"


def test_the_view_opens_without_a_selection(run_app: Any) -> None:
    """A fresh open must not explode on an empty draft or absent config."""

    async def scenario(pilot: Pilot[Any]) -> tuple[str, Any]:
        await pilot.press("9")
        await pilot.pause()
        screen = pilot.app.base_screen()
        return screen.status_text, screen.query_one(
            "#persistence-backend", Select
        ).value

    status, backend = run_app(scenario)
    assert "Pick an orchestration" in status
    assert backend == NO_PERSISTENCE
