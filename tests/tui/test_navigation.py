"""Navigation state machine: number keys, Tab cycling, back-then-quit, Ctrl-C."""

from __future__ import annotations

from typing import Any

import pytest

pytest.importorskip("textual")

from textual.pilot import Pilot
from textual.widgets import ListView

from circuitry.tui.app import CircuitryApp
from circuitry.tui.screens import VIEWS, HomeScreen, PlaceholderScreen, ViewScreen


def _slug(app: CircuitryApp) -> str | None:
    spec = app.current_view()
    return spec.slug if spec is not None else None


@pytest.mark.parametrize("spec", VIEWS, ids=[spec.slug for spec in VIEWS])
def test_number_key_opens_its_view(run_app: Any, spec: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[str | None, str]:
        await pilot.press(spec.key)
        await pilot.pause()
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        return _slug(app), type(app.screen).__name__

    assert run_app(scenario) == (spec.slug, PlaceholderScreen.__name__)


def test_view_screens_stack_one_deep(run_app: Any) -> None:
    """Switching views replaces rather than stacks, so Esc always reaches home."""

    async def scenario(pilot: Pilot[Any]) -> tuple[int, str | None]:
        for key in ("1", "3", "5", "2"):
            await pilot.press(key)
            await pilot.pause()
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        return len(app.screen_stack), _slug(app)

    assert run_app(scenario) == (2, "run")


def test_reopening_the_current_view_is_a_no_op(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[int, int]:
        await pilot.press("4")
        await pilot.pause()
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        first = id(app.screen)
        await pilot.press("4")
        await pilot.pause()
        return first, id(app.screen)

    first, second = run_app(scenario)
    assert first == second


def test_tab_cycles_home_then_every_view(run_app: Any) -> None:
    """Tab walks home -> each view in registry order -> back to home."""

    async def scenario(pilot: Pilot[Any]) -> list[str | None]:
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        seen: list[str | None] = [_slug(app)]
        for _ in range(len(VIEWS) + 1):
            await pilot.press("tab")
            await pilot.pause()
            seen.append(_slug(app))
        return seen

    expected = [None, *(spec.slug for spec in VIEWS), None]
    assert run_app(scenario) == expected


def test_shift_tab_cycles_backwards(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> list[str | None]:
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        seen: list[str | None] = []
        for _ in range(3):
            await pilot.press("shift+tab")
            await pilot.pause()
            seen.append(_slug(app))
        return seen

    assert run_app(scenario) == [VIEWS[-1].slug, VIEWS[-2].slug, VIEWS[-3].slug]


def test_tab_beats_the_screen_focus_binding(run_app: Any) -> None:
    """Textual's Screen binds Tab to focus-next; ours must win."""

    async def scenario(pilot: Pilot[Any]) -> str | None:
        await pilot.press("tab")
        await pilot.pause()
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        return _slug(app)

    assert run_app(scenario) == VIEWS[0].slug


@pytest.mark.parametrize("key", ["q", "escape"])
def test_back_then_quit(run_app: Any, key: str) -> None:
    """First press goes home; the next one quits."""

    async def scenario(pilot: Pilot[Any]) -> tuple[str, bool]:
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        await pilot.press("3")
        await pilot.pause()
        assert isinstance(app.screen, ViewScreen)
        await pilot.press(key)
        await pilot.pause()
        home = type(app.screen).__name__
        await pilot.press(key)
        await pilot.pause()
        return home, app.is_running

    assert run_app(scenario) == (HomeScreen.__name__, False)


@pytest.mark.parametrize("keys", [(), ("2",), ("2", "question_mark"), ("question_mark",)])
def test_ctrl_c_always_quits(run_app: Any, keys: tuple[str, ...]) -> None:
    async def scenario(pilot: Pilot[Any]) -> bool:
        for key in keys:
            await pilot.press(key)
            await pilot.pause()
        await pilot.press("ctrl+c")
        await pilot.pause()
        return bool(pilot.app.is_running)

    assert run_app(scenario) is False


def test_enter_on_the_home_list_opens_the_highlighted_view(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str | None:
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        views = app.query_one("#home-views", ListView)
        views.focus()
        views.index = 2
        await pilot.pause()
        await pilot.press("enter")
        await pilot.pause()
        await pilot.pause()
        return _slug(app)

    assert run_app(scenario) == VIEWS[2].slug


def test_home_lists_every_view_with_its_key(render: Any) -> None:
    frame = render(size=(100, 30))
    for spec in VIEWS:
        assert f"{spec.key}  {spec.name}" in frame


def test_navigation_keys_are_declared_on_the_app() -> None:
    keys = {binding.key for binding in CircuitryApp.BINDINGS}
    assert {"q", "escape", "tab", "shift+tab", "ctrl+c", "question_mark"} <= keys
    assert {spec.key for spec in VIEWS} <= keys
