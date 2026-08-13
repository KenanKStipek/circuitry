"""Pilot tests for the Run view: pick, fill, launch, cancel.

Runs here go through the real ``runtime_shim.run`` with a fake adapter
injected, so a green test means the whole path — form → RunRequest →
worker thread → completion signal — actually works.
"""

from __future__ import annotations

import asyncio
import threading
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest
import yaml

pytest.importorskip("textual")

from textual.pilot import Pilot
from textual.widgets import Button, Input, Select, Static

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, RunResult
from circuitry.tui.launch import CUSTOM_MODEL, NO_OVERRIDE, OrchestrationChoice
from circuitry.tui.run_view import CANCELLED, DONE, FAILED, RunScreen
from circuitry.tui.screens import VIEWS

RUN_SPEC = next(spec for spec in VIEWS if spec.slug == "run")

TWO_INPUTS: dict[str, Any] = {
    "adapter": "echo",
    "model": "echo-1",
    "interface": {
        "inputs": {
            "text": {"type": "string", "required": True, "description": "Say something"},
            "max_words": {"type": "number", "required": False, "default": 20},
        }
    },
    "effects": [
        {"type": "prompt", "name": "summarize", "template": "{{text}} / {{max_words}}"}
    ],
}

NO_INPUTS: dict[str, Any] = {
    "adapter": "echo",
    "model": "echo-1",
    "effects": [{"type": "prompt", "name": "greet", "template": "hello"}],
}


@dataclass(frozen=True)
class EchoAdapter:
    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=prompt, raw={"model": model})


@dataclass
class GateAdapter:
    """Blocks inside ``generate`` so a run can be cancelled mid-flight."""

    entered: threading.Event = field(default_factory=threading.Event)
    release: threading.Event = field(default_factory=threading.Event)
    name: str = "gate"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.entered.set()
        self.release.wait(timeout=10)
        return GenerateResult(text=prompt, raw={})


def _write(tmp_path: Path, orch: dict[str, Any], name: str = "demo.yml") -> Path:
    path = tmp_path / name
    path.write_text(yaml.dump(orch, sort_keys=False), encoding="utf-8")
    return path


def _choice(path: Path) -> OrchestrationChoice:
    return OrchestrationChoice(
        key=str(path), label=path.name, path=path, source="local"
    )


def _screen(path: Path, **kwargs: Any) -> RunScreen:
    kwargs.setdefault("adapter", EchoAdapter())
    return RunScreen(
        RUN_SPEC,
        config=CircuitryConfig(),
        choices=[_choice(path)],
        **kwargs,
    )


async def _open(pilot: Pilot[Any], screen: RunScreen) -> RunScreen:
    """Push the Run screen and select its single orchestration."""
    await pilot.app.push_screen(screen)
    await pilot.pause()
    screen.query_one("#run-orchestration", Select).value = screen._choices[0].key
    await pilot.pause()
    await pilot.pause()
    return screen


async def _settle(pilot: Pilot[Any], predicate: Any, timeout: float = 15.0) -> bool:
    """Pump the event loop until ``predicate`` holds (or we give up)."""
    deadline = asyncio.get_event_loop().time() + timeout
    while asyncio.get_event_loop().time() < deadline:
        if predicate():
            return True
        await pilot.pause()
        await asyncio.sleep(0.01)
    return predicate()


def _fill(screen: RunScreen, **values: str) -> None:
    for spec, box in screen._fields:
        if spec.name in values:
            box.value = values[spec.name]


def _errors(screen: RunScreen) -> list[str]:
    return [str(node.render()) for node in screen.query(".field-error").results(Static)]


# -- the generated form ------------------------------------------------------


def test_selecting_an_orchestration_generates_its_typed_form(
    run_app: Any, tmp_path: Path
) -> None:
    path = _write(tmp_path, TWO_INPUTS)

    async def scenario(pilot: Pilot[Any]) -> tuple[list[str], list[str], list[str]]:
        screen = await _open(pilot, _screen(path))
        # Scoped to the generated form: the setup pane also holds the
        # model dropdown's free-text box.
        boxes = list(screen.query("#run-form Input").results(Input))
        return (
            [spec.label for spec, _ in screen._fields],
            [box.value for box in boxes],
            [box.placeholder for box in boxes],
        )

    labels, values, placeholders = run_app(scenario)
    assert labels == ["text * (string)", "max_words (number)"]
    assert values == ["", "20"]  # the declared default is prefilled
    assert placeholders[0] == "Say something"


def test_an_orchestration_without_inputs_says_so(run_app: Any, tmp_path: Path) -> None:
    path = _write(tmp_path, NO_INPUTS)

    async def scenario(pilot: Pilot[Any]) -> tuple[int, str]:
        screen = await _open(pilot, _screen(path))
        return len(screen._fields), screen.status_text

    fields, status = run_app(scenario)
    assert fields == 0
    assert "no inputs declared" in status


def test_a_broken_orchestration_reports_instead_of_crashing(
    run_app: Any, tmp_path: Path
) -> None:
    path = tmp_path / "broken.yml"
    path.write_text("effects: [", encoding="utf-8")

    async def scenario(pilot: Pilot[Any]) -> tuple[str, bool]:
        screen = await _open(pilot, _screen(path))
        return screen.status_text, bool(pilot.app.is_running)

    status, running = run_app(scenario)
    assert status.startswith(f"{FAILED} to load")
    assert running


# -- validation --------------------------------------------------------------


def test_a_missing_required_input_blocks_the_launch(run_app: Any, tmp_path: Path) -> None:
    path = _write(tmp_path, TWO_INPUTS)

    async def scenario(pilot: Pilot[Any]) -> tuple[str, list[str], Any]:
        screen = await _open(pilot, _screen(path))
        _fill(screen, text="")
        screen.query_one("#run-launch", Button).press()
        await pilot.pause()
        return screen.status_text, _errors(screen), screen.last_result

    status, errors, result = run_app(scenario)
    assert "need attention" in status
    assert errors[0] == "text is required"
    assert result is None, "nothing should have been launched"


def test_a_mistyped_input_blocks_the_launch(run_app: Any, tmp_path: Path) -> None:
    path = _write(tmp_path, TWO_INPUTS)

    async def scenario(pilot: Pilot[Any]) -> tuple[list[str], Any]:
        screen = await _open(pilot, _screen(path))
        _fill(screen, text="hello", max_words="loads")
        screen.action_launch()
        await pilot.pause()
        return _errors(screen), screen.last_result

    errors, result = run_app(scenario)
    assert errors[0] == ""
    assert "expected a number" in errors[1]
    assert result is None


# -- launching ---------------------------------------------------------------


def test_form_fill_then_launch_reaches_completion(run_app: Any, tmp_path: Path) -> None:
    """The demo path: two inputs, launch, completion signal."""
    path = _write(tmp_path, TWO_INPUTS)

    async def scenario(pilot: Pilot[Any]) -> tuple[str, RunResult | None, bool]:
        screen = await _open(pilot, _screen(path))
        _fill(screen, text="hello", max_words="5")
        screen.query_one("#run-launch", Button).press()
        await _settle(pilot, lambda: screen.last_result is not None)
        return (
            screen.status_text,
            screen.last_result,
            screen.query_one("#run-launch", Button).disabled,
        )

    status, result, launch_disabled = run_app(scenario)
    assert result is not None and result.ok, result
    assert result.state["prime"]["summarize"]["value"] == "hello / 5"
    assert status.startswith(DONE)
    assert launch_disabled is False  # ready for the next run


def test_the_ui_stays_responsive_while_the_run_is_in_flight(
    run_app: Any, tmp_path: Path
) -> None:
    """The run is off the UI thread: keys still land while it is blocked."""
    path = _write(tmp_path, TWO_INPUTS)
    adapter = GateAdapter()

    async def scenario(pilot: Pilot[Any]) -> tuple[bool, str, bool]:
        screen = await _open(pilot, _screen(path, adapter=adapter))
        _fill(screen, text="hello")
        screen.action_launch()
        await _settle(pilot, adapter.entered.is_set)
        assert screen.query_one("#run-launch", Button).disabled
        assert not screen.query_one("#run-cancel", Button).disabled
        # The event loop is alive: a keypress is still processed.
        await pilot.press("question_mark")
        await pilot.pause()
        help_open = bool(pilot.app.help_is_open)
        await pilot.press("escape")
        adapter.release.set()
        await _settle(pilot, lambda: screen.last_result is not None)
        return help_open, screen.status_text, bool(pilot.app.is_running)

    help_open, status, alive = run_app(scenario)
    assert help_open, "the UI thread was blocked by the run"
    assert status.startswith(DONE)
    assert alive


def test_cancelling_stops_the_run_and_leaves_the_app_intact(
    run_app: Any, tmp_path: Path
) -> None:
    path = _write(tmp_path, TWO_INPUTS)
    adapter = GateAdapter()

    async def scenario(pilot: Pilot[Any]) -> tuple[str, RunResult | None, bool, str]:
        screen = await _open(pilot, _screen(path, adapter=adapter))
        _fill(screen, text="hello")
        screen.action_launch()
        await _settle(pilot, adapter.entered.is_set)
        screen.query_one("#run-cancel", Button).press()
        await pilot.pause()
        adapter.release.set()
        await _settle(pilot, lambda: screen.last_result is not None)
        frame = "\n".join(
            strip.text for strip in pilot.app.screen._compositor.render_strips()
        )
        return screen.status_text, screen.last_result, bool(pilot.app.is_running), frame

    status, result, alive, frame = run_app(scenario)
    assert status == CANCELLED
    assert result is not None and result.ok is False
    assert result.error, "a cancelled run surfaces its error on the result"
    assert alive and frame.strip(), "the terminal is still being painted"


def test_launch_is_ignored_until_an_orchestration_is_picked(
    run_app: Any, tmp_path: Path
) -> None:
    path = _write(tmp_path, TWO_INPUTS)

    async def scenario(pilot: Pilot[Any]) -> tuple[str, Any]:
        screen = _screen(path)
        await pilot.app.push_screen(screen)
        await pilot.pause()
        screen.action_launch()
        await pilot.pause()
        return screen.status_text, screen.last_result

    status, result = run_app(scenario)
    assert status == "Pick an orchestration first."
    assert result is None


def test_cancel_before_a_run_is_a_no_op(run_app: Any, tmp_path: Path) -> None:
    path = _write(tmp_path, TWO_INPUTS)

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await _open(pilot, _screen(path))
        screen.action_cancel_run()
        await pilot.pause()
        return screen.status_text

    assert "2 inputs" in run_app(scenario)


# -- overrides ---------------------------------------------------------------


def test_dropdowns_offer_configured_adapters_and_models(
    run_app: Any, tmp_path: Path
) -> None:
    path = _write(tmp_path, TWO_INPUTS)
    cfg = CircuitryConfig(
        default_adapter="ollama",
        default_model="llama3.1:8b",
        runtime={"adapters": {"openai": {"default_model": "gpt-4o-mini"}}},
    )

    async def scenario(pilot: Pilot[Any]) -> tuple[list[Any], list[Any]]:
        screen = RunScreen(
            RUN_SPEC, config=cfg, choices=[_choice(path)], adapter=EchoAdapter()
        )
        await _open(pilot, screen)
        return (
            [value for _, value in screen.query_one("#run-adapter", Select)._options],
            [value for _, value in screen.query_one("#run-model", Select)._options],
        )

    adapters, models = run_app(scenario)
    assert adapters == [NO_OVERRIDE, "ollama", "openai"]
    # The orchestration's own model joins the configured ones, and the
    # free-text escape hatch is always last.
    assert models == [
        NO_OVERRIDE,
        "echo-1",
        "gpt-4o-mini",
        "llama3.1:8b",
        CUSTOM_MODEL,
    ]


def test_chosen_overrides_ride_along_on_the_request(run_app: Any, tmp_path: Path) -> None:
    path = _write(tmp_path, TWO_INPUTS)
    captured: list[RunRequest] = []

    def runner(request: RunRequest) -> RunResult:
        captured.append(request)
        return RunResult(ok=True, state={}, warnings=[])

    async def scenario(pilot: Pilot[Any]) -> None:
        screen = await _open(pilot, _screen(path, runner=runner))
        _fill(screen, text="hello", max_words="7")
        screen.query_one("#run-adapter", Select).value = "ollama"
        screen.query_one("#run-model", Select).value = "echo-1"
        await pilot.pause()
        screen.action_launch()
        await _settle(pilot, lambda: screen.last_result is not None)

    run_app(scenario)
    assert len(captured) == 1
    request = captured[0]
    assert request.adapter_override == "ollama"
    assert request.model_override == "echo-1"
    assert request.initial_state == {"text": "hello", "max_words": 7}
    assert request.skip_preflight is False
    assert request.state_observer is not None


def test_leaving_the_dropdowns_alone_overrides_nothing(
    run_app: Any, tmp_path: Path
) -> None:
    path = _write(tmp_path, TWO_INPUTS)
    captured: list[RunRequest] = []

    def runner(request: RunRequest) -> RunResult:
        captured.append(request)
        return RunResult(ok=True, state={}, warnings=[])

    async def scenario(pilot: Pilot[Any]) -> None:
        screen = await _open(pilot, _screen(path, runner=runner))
        _fill(screen, text="hello")
        screen.action_launch()
        await _settle(pilot, lambda: screen.last_result is not None)

    run_app(scenario)
    assert captured[0].adapter_override is None
    assert captured[0].model_override is None


# -- adapter-reported models -------------------------------------------------


def _model_values(screen: RunScreen) -> list[Any]:
    return [value for _, value in screen.query_one("#run-model", Select)._options]


def test_picking_an_adapter_fills_the_model_dropdown(
    run_app: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The demo: select ollama, see the tags this machine actually has."""
    monkeypatch.setattr(
        "circuitry.tui.launch.list_adapter_models",
        lambda *, adapter_name, runtime: ["gpt-oss:20b", "phi3:mini"],
    )
    path = _write(tmp_path, TWO_INPUTS)

    async def scenario(pilot: Pilot[Any]) -> tuple[list[Any], str]:
        screen = await _open(pilot, _screen(path))
        screen.query_one("#run-adapter", Select).value = "ollama"
        await _settle(pilot, lambda: "gpt-oss:20b" in _model_values(screen))
        note = str(screen.query_one("#run-model-note", Static).render())
        return _model_values(screen), note

    values, note = run_app(scenario)
    # Adapter-reported tags merge with the config/orchestration-derived ones.
    assert values == [NO_OVERRIDE, "echo-1", "gpt-oss:20b", "phi3:mini", CUSTOM_MODEL]
    assert "2 models from ollama" in note


def test_an_adapter_that_reports_nothing_leaves_the_list_alone(
    run_app: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A missing hook or an unreachable daemon degrades, never errors."""

    def nothing(*, adapter_name: str, runtime: dict[str, Any]) -> list[str]:
        return []

    monkeypatch.setattr("circuitry.tui.launch.list_adapter_models", nothing)
    path = _write(tmp_path, TWO_INPUTS)

    async def scenario(pilot: Pilot[Any]) -> tuple[list[Any], str, bool]:
        screen = await _open(pilot, _screen(path))
        screen.query_one("#run-adapter", Select).value = "openai"
        await _settle(
            pilot,
            lambda: "did not report"
            in str(screen.query_one("#run-model-note", Static).render()),
        )
        return (
            _model_values(screen),
            str(screen.query_one("#run-model-note", Static).render()),
            bool(pilot.app.is_running),
        )

    values, note, running = run_app(scenario)
    assert values == [NO_OVERRIDE, "echo-1", CUSTOM_MODEL]
    assert "openai did not report any models" in note
    assert running


def test_a_slow_answer_for_a_stale_adapter_is_dropped(
    run_app: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The user moved on; a late reply must not repopulate the dropdown."""
    monkeypatch.setattr(
        "circuitry.tui.launch.list_adapter_models",
        lambda *, adapter_name, runtime: [],
    )
    path = _write(tmp_path, TWO_INPUTS)

    async def scenario(pilot: Pilot[Any]) -> list[Any]:
        screen = await _open(pilot, _screen(path))
        screen.query_one("#run-adapter", Select).value = "openai"
        await pilot.pause()
        screen.post_message(screen.AdapterModelsLoaded("ollama", ["ghost:7b"]))
        await pilot.pause()
        await pilot.pause()
        return _model_values(screen)

    assert "ghost:7b" not in run_app(scenario)


def test_custom_swaps_the_dropdown_for_a_box_and_threads_the_value(
    run_app: Any, tmp_path: Path
) -> None:
    """The escape hatch: any string, exactly like ``--model``."""
    path = _write(tmp_path, TWO_INPUTS)
    captured: list[RunRequest] = []

    def runner(request: RunRequest) -> RunResult:
        captured.append(request)
        return RunResult(ok=True, state={}, warnings=[])

    async def scenario(pilot: Pilot[Any]) -> tuple[bool, bool]:
        screen = await _open(pilot, _screen(path, runner=runner))
        _fill(screen, text="hello")
        screen.query_one("#run-model", Select).value = CUSTOM_MODEL
        await pilot.pause()
        box = screen.query_one("#run-model-custom", Input)
        hidden = screen.query_one("#run-model", Select).has_class("hidden")
        box.value = "gpt-oss:20b"
        screen.action_launch()
        await _settle(pilot, lambda: screen.last_result is not None)
        return hidden, box.has_class("hidden")

    select_hidden, box_hidden = run_app(scenario)
    assert select_hidden and not box_hidden
    assert captured[0].model_override == "gpt-oss:20b"


def test_an_empty_custom_box_gives_the_dropdown_back(
    run_app: Any, tmp_path: Path
) -> None:
    path = _write(tmp_path, TWO_INPUTS)

    async def scenario(pilot: Pilot[Any]) -> tuple[bool, Any, Any]:
        screen = await _open(pilot, _screen(path))
        screen.query_one("#run-model", Select).value = CUSTOM_MODEL
        await pilot.pause()
        box = screen.query_one("#run-model-custom", Input)
        box.post_message(Input.Submitted(box, ""))
        await pilot.pause()
        await pilot.pause()
        return (
            screen.query_one("#run-model", Select).has_class("hidden"),
            screen.query_one("#run-model", Select).value,
            screen._model_override(),
        )

    select_hidden, value, override = run_app(scenario)
    assert not select_hidden
    assert value == NO_OVERRIDE
    assert override is None


# -- failure reporting -------------------------------------------------------


def test_a_failed_run_surfaces_its_error(run_app: Any, tmp_path: Path) -> None:
    path = _write(tmp_path, TWO_INPUTS)

    def runner(request: RunRequest) -> RunResult:
        return RunResult(ok=False, state={}, warnings=[], error="adapter exploded")

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await _open(pilot, _screen(path, runner=runner))
        _fill(screen, text="hello")
        screen.action_launch()
        await _settle(pilot, lambda: screen.last_result is not None)
        return screen.status_text

    assert run_app(scenario) == f"{FAILED}: adapter exploded"


# -- keyboard ----------------------------------------------------------------


def test_enter_walks_the_form_then_lands_on_launch(run_app: Any, tmp_path: Path) -> None:
    """Tab belongs to view navigation, so Enter is what moves through fields."""
    path = _write(tmp_path, TWO_INPUTS)

    async def scenario(pilot: Pilot[Any]) -> list[str | None]:
        screen = await _open(pilot, _screen(path))
        focused: list[str | None] = []
        screen._fields[0][1].focus()
        await pilot.pause()
        for _ in range(2):
            await pilot.press("enter")
            await pilot.pause()
            node = pilot.app.focused
            focused.append(node.id if node is not None else None)
        return focused

    assert run_app(scenario) == ["run-field-1", "run-launch"]
