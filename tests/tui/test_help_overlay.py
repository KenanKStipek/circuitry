"""The `?` overlay: it opens, it closes, and every row is a real binding."""

from __future__ import annotations

from typing import Any, ClassVar

import pytest

pytest.importorskip("textual")

from textual.app import ComposeResult
from textual.binding import Binding, BindingType
from textual.pilot import Pilot
from textual.widgets import Static

from circuitry.tui.app import CircuitryApp
from circuitry.tui.help import (
    FOCUS_GROUP,
    GLOBAL_GROUP,
    HelpOverlay,
    HelpRow,
    binding_rows,
)
from circuitry.tui.screens import VIEWS, CircuitryScreen


def test_question_mark_opens_the_overlay(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> bool:
        await pilot.press("question_mark")
        await pilot.pause()
        return isinstance(pilot.app.screen, HelpOverlay)

    assert run_app(scenario) is True


@pytest.mark.parametrize("key", ["question_mark", "escape", "q"])
def test_overlay_closes_and_leaves_the_screen_underneath(run_app: Any, key: str) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[bool, str]:
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        await pilot.press("2")
        await pilot.pause()
        await pilot.press("question_mark")
        await pilot.pause()
        assert app.help_is_open
        await pilot.press(key)
        await pilot.pause()
        spec = app.current_view()
        return app.help_is_open, "" if spec is None else spec.slug

    assert run_app(scenario) == (False, "run")


def test_overlay_lists_every_view_key(render: Any) -> None:
    frame = render(size=(90, 40), keys=["question_mark"])
    for spec in VIEWS:
        assert f"{spec.key}  {spec.name}" in frame
    assert "Next view" in frame
    assert "Back / Quit" in frame


def test_rows_match_the_live_bindings(run_app: Any) -> None:
    """Every row must correspond to a binding Textual would actually dispatch."""

    async def scenario(pilot: Pilot[Any]) -> tuple[list[HelpRow], set[tuple[str, str]]]:
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        rows = binding_rows(app)
        live = {
            (app.get_key_display(active.binding), active.binding.description)
            for active in app.active_bindings.values()
        }
        return rows, live

    rows, live = run_app(scenario)
    assert rows
    for row in rows:
        assert (row.key, row.description) in live


def test_rows_skip_system_and_undescribed_bindings(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> list[HelpRow]:
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        return binding_rows(app)

    rows = run_app(scenario)
    assert all(row.description for row in rows)
    # Textual's system ctrl+c "help_quit" binding carries no description and
    # is superseded by our own quit binding.
    assert [row for row in rows if row.key == "^c"] == [HelpRow("^c", "Quit", GLOBAL_GROUP)]


class _ScreenWithExtraKey(CircuitryScreen):
    BINDINGS: ClassVar[list[BindingType]] = [
        Binding("z", "zap", "Zap the thing"),
    ]

    def compose_body(self) -> ComposeResult:
        yield Static("custom")


class _AppWithCustomScreen(CircuitryApp):
    def get_default_screen(self) -> Any:
        return _ScreenWithExtraKey(id="custom")


def test_overlay_picks_up_screen_specific_bindings(render: Any) -> None:
    """A screen's own binding shows up without anyone updating a help table."""
    frame = render(app=_AppWithCustomScreen(), size=(90, 40), keys=["question_mark"])
    assert "Zap the thing" in frame
    assert "This screen" in frame


def test_rows_are_grouped_global_first(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> list[str]:
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        seen: list[str] = []
        for row in binding_rows(app):
            if row.group not in seen:
                seen.append(row.group)
        return seen

    groups = run_app(scenario)
    assert groups[0] == GLOBAL_GROUP
    assert FOCUS_GROUP not in groups[:1]


def test_overlay_heading_names_the_screen_underneath(render: Any) -> None:
    assert "Keys — Home" in render(size=(90, 40), keys=["question_mark"])
    assert "Keys — Doctor" in render(size=(90, 40), keys=["5", "question_mark"])


def test_overlay_renders_a_message_when_there_is_nothing_to_show(run_app: Any) -> None:
    """The table degrades to a sentence rather than an empty box."""
    from circuitry.tui.help import HelpTable

    async def scenario(pilot: Pilot[Any]) -> str:
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        await app.push_screen(HelpOverlay([], title="Keys"))
        await pilot.pause()
        table = app.screen.query_one("#help-rows", HelpTable)
        return str(table.render())

    assert "No bindings" in run_app(scenario)
