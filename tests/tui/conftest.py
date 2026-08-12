"""Shared Textual test harness for the TUI suite.

Three fixtures, all synchronous (the repo has no async plugin — the event
loop is owned by the harness, not the test):

``run_app``
    Drive a Textual pilot from a plain test function and get whatever your
    scenario returns.
``render``
    Boot the app at a given size, optionally press some keys, and get the
    rendered frame back as text.
``snapshot``
    Compare rendered text against a file in ``tests/tui/__snapshots__``.
    Set ``CIRCUITRY_SNAPSHOT_UPDATE=1`` to (re)write the files; a missing
    snapshot is a failure otherwise, so CI never passes on an empty record.

Usage::

    def test_number_key_opens_a_view(run_app):
        async def scenario(pilot):
            await pilot.press("2")
            await pilot.pause()
            return pilot.app.current_view().slug

        assert run_app(scenario) == "run"

    def test_home_is_stable(render, snapshot):
        snapshot.assert_match(render(size=(80, 24)), "home-80x24")

    def test_tiny_terminal_still_renders(render):
        assert render(size=(10, 4))  # no exception, non-empty frame

Pass ``app=...`` to any of them to drive a subclass or a pre-configured app
instead of a default :class:`CircuitryApp`.
"""

from __future__ import annotations

import asyncio
import os
from collections.abc import Awaitable, Callable, Iterable, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, TypeVar

import pytest

pytest.importorskip("textual")

from textual.app import App
from textual.pilot import Pilot

from circuitry.tui.app import CircuitryApp

T = TypeVar("T")

#: Default terminal size for pilot runs — comfortably above every breakpoint.
DEFAULT_SIZE = (80, 24)

SNAPSHOT_DIR = Path(__file__).parent / "__snapshots__"
SNAPSHOT_UPDATE_ENV = "CIRCUITRY_SNAPSHOT_UPDATE"


def screen_text(app: App[Any]) -> str:
    """Render the app's current frame to plain text.

    The one place that touches Textual's compositor, so a future API change
    is a single-line fix for the whole suite.
    """
    strips = app.screen._compositor.render_strips()
    return "\n".join(strip.text for strip in strips)


def frame_lines(app: App[Any]) -> list[str]:
    """The current frame as a list of rows (trailing blanks preserved)."""
    return screen_text(app).split("\n")


@pytest.fixture
def capture_frame() -> Callable[[App[Any]], str]:
    """Return :func:`screen_text`, for rendering mid-scenario.

    Usage::

        def test_frame(run_app, capture_frame):
            async def scenario(pilot):
                await pilot.resize_terminal(10, 4)
                await pilot.pause()
                return capture_frame(pilot.app)

            assert run_app(scenario).count("\\n") == 3
    """
    return screen_text


@pytest.fixture
def run_app() -> Callable[..., Any]:
    """Return a runner that drives ``scenario(pilot)`` inside a live app."""

    def _run(
        scenario: Callable[[Pilot[Any]], Awaitable[T]],
        *,
        app: App[Any] | None = None,
        size: tuple[int, int] = DEFAULT_SIZE,
    ) -> T:
        async def _drive() -> T:
            application = app if app is not None else CircuitryApp()
            async with application.run_test(size=size) as pilot:
                await pilot.pause()
                return await scenario(pilot)

        return asyncio.run(_drive())

    return _run


@pytest.fixture
def render(run_app: Callable[..., Any]) -> Callable[..., str]:
    """Return a helper that renders the app (after optional keypresses)."""

    def _render(
        *,
        app: App[Any] | None = None,
        size: tuple[int, int] = DEFAULT_SIZE,
        keys: Sequence[str] = (),
        resizes: Iterable[tuple[int, int]] = (),
    ) -> str:
        async def _scenario(pilot: Pilot[Any]) -> str:
            for key in keys:
                await pilot.press(key)
                await pilot.pause()
            for width, height in resizes:
                await pilot.resize_terminal(width, height)
                await pilot.pause()
            return screen_text(pilot.app)

        return str(run_app(_scenario, app=app, size=size))

    return _render


@dataclass(frozen=True)
class Snapshot:
    """Text snapshot comparison rooted at ``tests/tui/__snapshots__``."""

    directory: Path
    update: bool

    def path(self, name: str) -> Path:
        return self.directory / f"{name}.txt"

    def assert_match(self, text: str, name: str) -> None:
        """Compare ``text`` with the stored snapshot ``name``."""
        path = self.path(name)
        payload = text if text.endswith("\n") else text + "\n"
        if self.update:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(payload, encoding="utf-8")
            return
        if not path.exists():
            pytest.fail(
                f"missing snapshot {path.name}; record it with "
                f"{SNAPSHOT_UPDATE_ENV}=1 pytest tests/tui"
            )
        expected = path.read_text(encoding="utf-8")
        assert payload == expected, (
            f"snapshot {name} changed; re-record with "
            f"{SNAPSHOT_UPDATE_ENV}=1 pytest tests/tui if this is intended"
        )


@pytest.fixture
def snapshot() -> Snapshot:
    """Snapshot comparator; set ``CIRCUITRY_SNAPSHOT_UPDATE=1`` to re-record."""
    return Snapshot(SNAPSHOT_DIR, os.environ.get(SNAPSHOT_UPDATE_ENV) == "1")
