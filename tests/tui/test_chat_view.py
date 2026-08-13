"""Chat view: a conversation that ends in a file `cof check` accepts.

The flagship test here drives the *real* wizard orchestration — its interpret
step, its ask-or-draft conditional, its `validate_yaml` tool and its `done`
gate — over a scripted adapter, through the actual screen, to a saved file.
Only the model is fake; every gate between a typed sentence and a file on disk
runs for real.
"""

from __future__ import annotations

import asyncio
import json
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("textual")

from scripted_wizard import (
    INVALID_DRAFT,
    VALID_DRAFT,
    ScriptedTurn,
    ScriptedWizardAdapter,
    ask,
    draft,
)
from textual.pilot import Pilot
from textual.widgets import Input

from circuitry.tui.app import CircuitryApp
from circuitry.tui.chat import EMPTY_DRAFT, ChatScreen, draft_preview
from circuitry.tui.screens import VIEWS, CircuitryScreen
from circuitry.tui.wizard_host import Seed, Turn, TurnRunner, run_turn

SEED = Seed(name="Summarize And Translate", category="recipes", goal="Summarize, then translate.")

CHAT = next(spec for spec in VIEWS if spec.slug == "chat")


class ViewApp(CircuitryApp):
    """The real shell with a pre-configured chat screen opened over home.

    Pushed rather than made the default screen, so navigating away from it —
    which "run it now" does — behaves exactly as it does in the real app.
    """

    def __init__(self, screen: CircuitryScreen) -> None:
        super().__init__()
        self._view = screen

    def on_mount(self) -> None:
        self.push_screen(self._view)


def scripted_runner(*turns: ScriptedTurn) -> tuple[TurnRunner, ScriptedWizardAdapter]:
    """A turn runner that drives the real wizard over a scripted adapter."""
    adapter = ScriptedWizardAdapter(list(turns))
    return (lambda state: run_turn(state, adapter=adapter)), adapter


def chat_app(
    runner: TurnRunner,
    *,
    seed: Seed | None = SEED,
    library_dir: Path | None = None,
) -> ViewApp:
    return ViewApp(ChatScreen(CHAT, runner=runner, seed=seed, library_dir=library_dir))


async def settle(pilot: Pilot[Any], done: Callable[[], bool], tries: int = 400) -> bool:
    """Wait for a worker-driven condition without sleeping a fixed amount."""
    for _ in range(tries):
        if done():
            return True
        await pilot.pause()
        await asyncio.sleep(0.01)
    return done()


def screen_of(pilot: Pilot[Any]) -> ChatScreen:
    return pilot.app.screen  # type: ignore[return-value]


def _idle(pilot: Pilot[Any]) -> bool:
    screen = pilot.app.screen
    return isinstance(screen, ChatScreen) and not screen.busy


async def idle(pilot: Pilot[Any]) -> ChatScreen:
    """Wait out whatever turn is in flight and hand back the screen."""
    await settle(pilot, lambda: _idle(pilot))
    await pilot.pause()
    return screen_of(pilot)


async def say(pilot: Pilot[Any], text: str) -> ChatScreen:
    screen_of(pilot).send(text)
    return await idle(pilot)


def status_of(screen: ChatScreen) -> str:
    return screen.status_text


# ── the whole flow, end to end ───────────────────────────────────────────────


def test_full_conversation_saves_a_file_that_passes_cof_check(
    run_app: Any, tmp_path: Path
) -> None:
    """Seed form → question turn → typed reply → draft turn → Ctrl-S → `cof check`.

    Everything between the keystrokes and the file is real: the wizard
    orchestration, its `validate_yaml` tool, its `done` gate, the draft pane's
    validator, and the save. Only the model is scripted.
    """
    from typer.testing import CliRunner

    from circuitry.cli.app import app as cli_app

    target = tmp_path / "summarize_and_translate.yml"
    runner, adapter = scripted_runner(
        ask("Which language should it translate into?"),
        draft("Built a two-step pipeline: summarize, then translate.", done=True),
    )

    async def scenario(pilot: Pilot[Any]) -> dict[str, Any]:
        screen = await idle(pilot)  # turn 1, seeded by the form
        first = screen.conversation.messages[-1].content

        await pilot.press(*"French.")  # turn 2 — typed into the message box
        await pilot.press("enter")
        screen = await idle(pilot)

        screen.query_one("#chat-save-path", Input).value = str(target)
        await pilot.press("ctrl+s")
        await pilot.pause()
        return {
            "first": first,
            "roles": [m.role for m in screen.conversation.messages],
            "reply": screen.conversation.messages[1].content,
            "done": screen.conversation.done,
            "status": status_of(screen),
        }

    result = run_app(scenario, app=chat_app(runner), size=(120, 40))

    assert result["first"] == "Which language should it translate into?"
    assert result["roles"] == ["wizard", "user", "wizard"]
    assert result["reply"] == "French."
    assert result["done"] is True
    assert str(target) in result["status"]

    # The artefact, judged by the command the human would reach for.
    assert target.exists()
    checked = CliRunner().invoke(cli_app, ["check", str(target), "--skip-preflight"])
    assert checked.exit_code == 0, checked.output

    # The second turn genuinely saw the first turn's transcript.
    assert any("French." in prompt for prompt in adapter.prompts)


def test_the_seed_form_starts_the_conversation(run_app: Any) -> None:
    runner, _ = scripted_runner(draft("Here is a draft.", done=True))

    async def scenario(pilot: Pilot[Any]) -> dict[str, Any]:
        for widget_id, value in (
            ("#seed-name", "My Pipeline"),
            ("#seed-category", "recipes"),
            ("#seed-goal", "Summarize an article."),
        ):
            pilot.app.screen.query_one(widget_id, Input).value = value
        screen_of(pilot).submit_seed()
        screen = await idle(pilot)
        return {
            "started": screen.started,
            "seed": screen.conversation.seed,
            "save_path": screen.query_one("#chat-save-path", Input).value,
            "reply": screen.conversation.messages[-1].content,
        }

    result = run_app(scenario, app=chat_app(runner, seed=None), size=(120, 40))
    assert result["started"] is True
    assert result["seed"].goal == "Summarize an article."
    assert result["save_path"] == "my_pipeline.yml"
    assert result["reply"] == "Here is a draft."


def test_an_incomplete_seed_form_does_not_start(run_app: Any, capture_frame: Any) -> None:
    runner, adapter = scripted_runner(draft("Never asked for."))

    async def scenario(pilot: Pilot[Any]) -> tuple[bool, str, int]:
        screen_of(pilot).submit_seed()
        await pilot.pause()
        return (screen_of(pilot).started, capture_frame(pilot.app), len(adapter.prompts))

    started, frame, prompts = run_app(scenario, app=chat_app(runner, seed=None), size=(120, 40))
    assert started is False
    assert "Give it a name." in frame
    assert prompts == 0


# ── the validation pane ──────────────────────────────────────────────────────


def test_a_valid_draft_turns_the_pane_green(run_app: Any, capture_frame: Any) -> None:
    runner, _ = scripted_runner(draft("Built it.", done=True))

    async def scenario(pilot: Pilot[Any]) -> tuple[str, Any]:
        screen = await idle(pilot)
        return capture_frame(pilot.app), screen.conversation.status

    frame, status = run_app(scenario, app=chat_app(runner), size=(140, 50))
    assert status is not None and status.ok
    assert "✔ Valid" in frame
    assert "type: prompt" in frame  # the draft itself is on screen


def test_an_invalid_draft_turns_the_pane_red_and_lists_the_errors(
    run_app: Any, capture_frame: Any
) -> None:
    """The wizard claiming `done` does not override the validator's verdict."""
    runner, _ = scripted_runner(draft("Shipped it.", INVALID_DRAFT, done=True))

    async def scenario(pilot: Pilot[Any]) -> tuple[str, Any]:
        screen = await idle(pilot)
        return capture_frame(pilot.app), screen.conversation

    frame, convo = run_app(scenario, app=chat_app(runner), size=(140, 50))
    assert convo.status is not None and not convo.status.ok
    assert convo.done is False
    assert "✘" in frame
    assert "iter_0" in frame  # the validator's own words, not a summary


def test_a_question_turn_leaves_the_pane_empty(run_app: Any, capture_frame: Any) -> None:
    runner, _ = scripted_runner(ask("What should it output?"))

    async def scenario(pilot: Pilot[Any]) -> tuple[str, Any]:
        screen = await idle(pilot)
        return capture_frame(pilot.app), screen.conversation.status

    frame, status = run_app(scenario, app=chat_app(runner), size=(140, 50))
    assert status is None
    assert EMPTY_DRAFT in frame


def test_the_pane_tracks_the_verdict_across_turns(run_app: Any) -> None:
    """Every draft is re-validated — a red pane can go green and back."""
    runner, _ = scripted_runner(
        draft("First attempt.", INVALID_DRAFT),
        draft("Fixed it."),
        draft("Broke it again.", INVALID_DRAFT),
    )

    async def scenario(pilot: Pilot[Any]) -> list[bool]:
        verdicts = []
        screen = await idle(pilot)
        verdicts.append(screen.conversation.status.ok)
        for reply in ("fix it", "now break it"):
            screen = await say(pilot, reply)
            verdicts.append(screen.conversation.status.ok)
        return verdicts

    assert run_app(scenario, app=chat_app(runner), size=(120, 40)) == [False, True, False]


# ── saving ───────────────────────────────────────────────────────────────────


def test_an_invalid_draft_cannot_be_saved(run_app: Any, tmp_path: Path) -> None:
    target = tmp_path / "nope.yml"
    runner, _ = scripted_runner(draft("Shipped it.", INVALID_DRAFT, done=True))

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await idle(pilot)
        screen.query_one("#chat-save-path", Input).value = str(target)
        screen.action_save_file()
        screen.action_save_library()
        await pilot.pause()
        return status_of(screen)

    status = run_app(scenario, app=chat_app(runner, library_dir=tmp_path / "lib"), size=(120, 40))
    assert "cannot be saved" in status
    assert not target.exists()
    assert not (tmp_path / "lib").exists()


def test_saving_before_there_is_a_draft_says_so(run_app: Any) -> None:
    runner, _ = scripted_runner(ask("What should it output?"))

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await idle(pilot)
        screen.action_save_file()
        await pilot.pause()
        return status_of(screen)

    assert "no draft to save" in run_app(scenario, app=chat_app(runner), size=(120, 40))


def test_saving_without_a_path_asks_for_one(run_app: Any) -> None:
    runner, _ = scripted_runner(draft("Built it.", done=True))

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await idle(pilot)
        screen.query_one("#chat-save-path", Input).value = ""
        screen.action_save_file()
        await pilot.pause()
        return status_of(screen)

    assert "Type a path" in run_app(scenario, app=chat_app(runner), size=(120, 40))


def test_save_to_library_writes_yaml_and_a_manifest_entry(
    run_app: Any, tmp_path: Path
) -> None:
    runner, _ = scripted_runner(draft("Built it.", done=True))

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await idle(pilot)
        screen.action_save_library()
        await pilot.pause()
        return status_of(screen)

    status = run_app(scenario, app=chat_app(runner, library_dir=tmp_path), size=(120, 40))

    saved = tmp_path / "recipes" / "summarize_and_translate.yml"
    assert saved.read_text(encoding="utf-8") == VALID_DRAFT
    assert "recipes/summarize_and_translate" in status

    manifest = json.loads((tmp_path / "manifest.json").read_text(encoding="utf-8"))
    assert manifest["entries"][0]["file"] == "recipes/summarize_and_translate.yml"


def test_a_saved_orchestration_is_reachable_as_a_library_entry(
    run_app: Any, tmp_path: Path
) -> None:
    """The point of the manifest: `cof run <name>` can find what was saved."""
    from circuitry.cli.config import CircuitryConfig
    from circuitry.cli.library_sources import LibraryRegistry

    runner, _ = scripted_runner(draft("Built it.", done=True))

    async def scenario(pilot: Pilot[Any]) -> None:
        screen = await idle(pilot)
        screen.action_save_library()
        await pilot.pause()

    run_app(scenario, app=chat_app(runner, library_dir=tmp_path), size=(120, 40))

    registry = LibraryRegistry.from_config(
        CircuitryConfig(
            runtime={
                "library": {"sources": [{"type": "folder", "name": "local", "path": str(tmp_path)}]}
            }
        )
    )
    resolution = registry.resolve("recipes/summarize_and_translate")
    assert resolution is not None
    assert resolution.path == tmp_path / "recipes" / "summarize_and_translate.yml"
    assert resolution.entry.metadata["intent"] == SEED.goal


def test_slash_commands_save_the_same_way_the_keys_do(run_app: Any, tmp_path: Path) -> None:
    target = tmp_path / "typed.yml"
    runner, _ = scripted_runner(draft("Built it.", done=True))

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await idle(pilot)
        screen.send(f"/save {target}")
        await pilot.pause()
        return status_of(screen)

    status = run_app(scenario, app=chat_app(runner), size=(120, 40))
    assert target.exists()
    assert str(target) in status


# ── the run hand-off ─────────────────────────────────────────────────────────


def test_run_it_now_hands_the_saved_path_to_the_run_view(
    run_app: Any, tmp_path: Path
) -> None:
    target = tmp_path / "runnable.yml"
    runner, _ = scripted_runner(draft("Built it.", done=True))

    async def scenario(pilot: Pilot[Any]) -> tuple[Any, Any]:
        screen = await idle(pilot)
        screen.query_one("#chat-save-path", Input).value = str(target)
        screen.action_save_file()
        screen.action_run_it()
        await pilot.pause()
        return pilot.app.pending_run, pilot.app.current_view().slug

    pending, slug = run_app(scenario, app=chat_app(runner), size=(120, 40))
    assert pending == target
    assert slug == "run"


def test_run_it_now_before_saving_says_so(run_app: Any) -> None:
    runner, _ = scripted_runner(draft("Built it.", done=True))

    async def scenario(pilot: Pilot[Any]) -> tuple[str, Any]:
        screen = await idle(pilot)
        screen.action_run_it()
        await pilot.pause()
        return status_of(screen), pilot.app.pending_run

    status, pending = run_app(scenario, app=chat_app(runner), size=(120, 40))
    assert "Save it first" in status
    assert pending is None


# ── failure and rendering ────────────────────────────────────────────────────


def test_a_failing_turn_is_reported_not_swallowed(run_app: Any, capture_frame: Any) -> None:
    def explode(state: dict[str, Any]) -> Turn:
        raise RuntimeError("the adapter is unreachable")

    async def scenario(pilot: Pilot[Any]) -> str:
        await idle(pilot)
        return capture_frame(pilot.app)

    frame = run_app(scenario, app=chat_app(explode), size=(120, 40))
    assert "the adapter is unreachable" in frame


def test_the_message_box_is_released_when_a_turn_lands(run_app: Any) -> None:
    runner, _ = scripted_runner(draft("Built it.", done=True))

    async def scenario(pilot: Pilot[Any]) -> bool:
        screen = await idle(pilot)
        return screen.query_one("#chat-message", Input).disabled

    assert run_app(scenario, app=chat_app(runner), size=(120, 40)) is False


def test_a_message_sent_mid_turn_is_ignored(run_app: Any) -> None:
    """Enter while the wizard is thinking must not queue a second turn."""
    runner, _ = scripted_runner(draft("Built it.", done=True))

    async def scenario(pilot: Pilot[Any]) -> list[str]:
        screen = await idle(pilot)
        screen.busy = True
        screen.send("too soon")
        await pilot.pause()
        return [m.content for m in screen.conversation.messages]

    assert run_app(scenario, app=chat_app(runner), size=(120, 40)) == ["Built it."]


def test_draft_preview_elides_a_long_draft() -> None:
    long_draft = "\n".join(f"line {n}" for n in range(60))
    preview = draft_preview(long_draft, limit=10)
    assert preview.splitlines()[-1] == "… 50 more lines"
    assert preview.startswith("line 0")


def test_draft_preview_of_nothing_is_the_empty_state() -> None:
    assert draft_preview("   ") == EMPTY_DRAFT
    assert draft_preview("effects: []") == "effects: []"
