"""Textual pilot smoke test: the app boots, renders home, and quits."""

from __future__ import annotations

import asyncio

import pytest

pytest.importorskip("textual")

from textual.widgets import ListView

from circuitry.tui.app import PLANNED_VIEWS, CircuitryApp


def test_home_renders_every_planned_view() -> None:
    async def _drive() -> str:
        app = CircuitryApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            views = app.query_one("#home-views", ListView)
            assert len(views.children) == len(PLANNED_VIEWS)
            return "\n".join(str(item.render()) for item in views.query("Label"))

    rendered = asyncio.run(_drive())
    for name, blurb in PLANNED_VIEWS:
        assert name in rendered
        assert blurb in rendered


def test_q_quits_cleanly() -> None:
    async def _drive() -> CircuitryApp:
        app = CircuitryApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            assert app.is_running
            await pilot.press("q")
            await pilot.pause()
        return app

    app = asyncio.run(_drive())
    assert not app.is_running
    assert app.return_value is None


def test_quit_binding_is_declared() -> None:
    keys = {binding.key for binding in CircuitryApp.BINDINGS}
    assert "q" in keys


def test_run_tui_uses_the_textual_app(monkeypatch: pytest.MonkeyPatch) -> None:
    """``run_tui`` imports lazily and drives ``CircuitryApp.run``."""
    from circuitry import tui

    calls: list[str] = []
    monkeypatch.setattr(CircuitryApp, "run", lambda self: calls.append("run"))

    tui.run_tui()

    assert calls == ["run"]
