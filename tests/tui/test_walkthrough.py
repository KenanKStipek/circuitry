"""The keyboard-only walkthrough: explore → create → profile → run → inspect.

One continuous session, one app, and exactly one way to drive it —
``pilot.press``. Nothing in this module calls ``widget.focus()`` or assigns a
widget's ``value``, because the point is to prove those affordances are not
what a person needs. If a leg of the journey can only be reached by touching
a widget, this test is supposed to fail. That ban is checked rather than
trusted: :func:`test_no_step_reaches_past_the_keyboard` reads the file back.

Reading widgets is fine. Driving them is not.

The second half is the general form of the same claim.
:func:`test_every_focusable_widget_is_reachable_with_ctrl_n` walks each view's
focus ring and asserts it visits everything the screen can focus. Before
Ctrl-N existed that ring had one reachable member on every screen: Tab belongs
to the views, and nothing had replaced Textual's focus binding.
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("textual")

from textual.pilot import Pilot

from circuitry.tui.app import CircuitryApp
from circuitry.tui.screens import VIEWS


async def press(pilot: Pilot[Any], *keys: str) -> None:
    """Send keys and let the app settle. The only door into the app."""
    for key in keys:
        await pilot.press(key)
        await pilot.pause()


def slug(app: CircuitryApp) -> str | None:
    spec = app.current_view()
    return spec.slug if spec is not None else None


def focused_id(app: CircuitryApp) -> str | None:
    widget = app.screen.focused
    return None if widget is None else widget.id


def focus_ring(app: CircuitryApp) -> list[str | None]:
    """Ids of everything the current screen can put the keyboard on."""
    return [widget.id for widget in app.screen.focus_chain]


# -- the walkthrough ---------------------------------------------------------


def test_keyboard_only_walkthrough(run_app: Any) -> None:
    """Five views, one session, no mouse.

    Kept as one test rather than five: the claim is that the *journey* works,
    and per-view tests would let the app be keyboard-complete one screen at a
    time while the seams between them stayed mouse-only.
    """

    async def scenario(pilot: Pilot[Any]) -> dict[str, tuple[str | None, str | None]]:
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        log: dict[str, tuple[str | None, str | None]] = {}

        def note(step: str) -> None:
            log[step] = (slug(app), focused_id(app))

        # ── 0. home ─────────────────────────────────────────────────────────
        note("home")

        # ── 1. explore: the library, its search box, its three panes ────────
        await press(pilot, "1")
        note("library")
        # The results list owns the keyboard on arrival, so arrows browse.
        await press(pilot, "down", "down")
        # "/" is the screen's own way into the search box.
        await press(pilot, "slash")
        note("library-search")
        await press(pilot, "h", "e", "l", "l", "o")
        # Esc backs out of the search without leaving the view.
        await press(pilot, "escape")
        note("library-search-cleared")
        # Ctrl-N hands the keyboard on around the ring. This is the step that
        # was impossible before the focus bindings landed.
        await press(pilot, "ctrl+n")
        note("library-next-pane")

        # ── 2. create: the wizard chat's seed form ──────────────────────────
        await press(pilot, "8")
        note("chat")
        # The seed form has the keyboard, so a name can just be typed.
        await press(pilot, "d", "e", "m", "o")
        # Ctrl-N walks seed name → category → goal, and Ctrl-B walks back.
        await press(pilot, "ctrl+n")
        note("chat-category")
        await press(pilot, "ctrl+n")
        note("chat-goal")
        await press(pilot, "ctrl+b")
        note("chat-back-to-category")

        # ── 3. profile: the profile editor ──────────────────────────────────
        # A digit typed at a focused box is text, so the way out of one is
        # Esc (home) or Tab (next view) — never a number key.
        await press(pilot, "escape")
        note("chat-escaped")
        await press(pilot, "9")
        note("profiles")
        ring = await walk_ring(pilot, app)
        note("profiles-ring-walked")
        assert "profile-orchestration" in ring, ring

        # ── 4. run: picker, overrides, launch button ────────────────────────
        await press(pilot, "escape")
        await press(pilot, "2")
        note("run")
        ring = await walk_ring(pilot, app)
        note("run-ring-walked")
        # Every control in the run form, reached with one key.
        assert {"run-orchestration", "run-adapter", "run-model", "run-launch"} <= ring

        # ── 5. inspect: the run-state tree ──────────────────────────────────
        await press(pilot, "4")
        note("runs")
        # The tree, not the path box, has the keyboard — so "o" and "y" work
        # the moment the view opens instead of typing themselves into a box.
        await press(pilot, "down")
        await press(pilot, "ctrl+n")
        note("runs-next-pane")

        # ── 6. the help overlay, then all the way out ───────────────────────
        await press(pilot, "question_mark")
        log["help"] = (slug(app), type(app.screen).__name__)
        await press(pilot, "escape")
        note("help-closed")
        await press(pilot, "escape")
        note("home-again")
        return log

    steps = run_app(scenario, size=(100, 30))

    # Every leg arrived where it said it would.
    assert steps["home"][0] is None
    assert steps["library"][0] == "library"
    assert steps["chat"][0] == "chat"
    assert steps["chat-escaped"][0] is None
    assert steps["profiles"][0] == "profiles"
    assert steps["run"][0] == "run"
    assert steps["runs"][0] == "runs"
    assert steps["home-again"][0] is None

    # Each view handed the keyboard to something usable on arrival.
    assert steps["library"][1] == "library-list"
    assert steps["library-search"][1] == "library-search"
    assert steps["chat"][1] == "seed-name"
    assert steps["runs"][1] == "runs-tree"

    # Ctrl-N walked the chat seed form and Ctrl-B walked back.
    assert steps["chat-category"][1] == "seed-category"
    assert steps["chat-goal"][1] == "seed-goal"
    assert steps["chat-back-to-category"][1] == "seed-category"

    # Every ring walk moved the keyboard off where it started.
    assert steps["library-next-pane"][1] != "library-list"
    assert steps["runs-next-pane"][1] != "runs-tree"

    # The help overlay opened over the run inspector and closed back onto it.
    assert steps["help"][1] == "HelpOverlay"
    assert steps["help-closed"][0] == "runs"


async def walk_ring(pilot: Pilot[Any], app: CircuitryApp) -> set[str | None]:
    """Press Ctrl-N once per focusable widget; return everywhere it landed."""
    seen: set[str | None] = {focused_id(app)}
    for _ in range(len(focus_ring(app))):
        await press(pilot, "ctrl+n")
        seen.add(focused_id(app))
    return seen


def test_a_focused_box_keeps_the_digits_it_is_typed(run_app: Any) -> None:
    """Typing is typing — the view keys must not steal a digit from a box.

    The other half of "keyboard-complete": the number keys are live app-wide,
    but a pipeline called ``step2`` still has to be nameable. The escape
    hatches (Esc, Tab) are what keep that from being a trap, and the help
    overlay says so out loud.
    """

    async def scenario(pilot: Pilot[Any]) -> tuple[str, str | None]:
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        await press(pilot, "8")
        await press(pilot, "s", "t", "e", "p", "2")
        typed = str(getattr(app.screen.query_one("#seed-name"), "value", ""))
        # Tab is priority-bound, so it works even from inside the box.
        await press(pilot, "tab")
        return typed, slug(app)

    typed, after_tab = run_app(scenario)
    assert typed == "step2"
    assert after_tab == "profiles"


# -- the general claim -------------------------------------------------------


@pytest.mark.parametrize("spec", VIEWS, ids=[spec.slug for spec in VIEWS])
def test_every_focusable_widget_is_reachable_with_ctrl_n(run_app: Any, spec: Any) -> None:
    """Ctrl-N, pressed once per focusable widget, visits all of them."""

    async def scenario(pilot: Pilot[Any]) -> tuple[set[str | None], set[str | None]]:
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        await press(pilot, spec.key)
        return set(focus_ring(app)), await walk_ring(pilot, app)

    expected, seen = run_app(scenario, size=(100, 30))
    assert expected <= seen, f"{spec.slug}: unreachable by keyboard: {expected - seen}"


@pytest.mark.parametrize("spec", VIEWS, ids=[spec.slug for spec in VIEWS])
def test_ctrl_b_walks_the_ring_backwards(run_app: Any, spec: Any) -> None:
    """Ctrl-B is Ctrl-N's inverse, so a wrong turn costs one keystroke."""

    async def scenario(pilot: Pilot[Any]) -> tuple[str | None, str | None]:
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        await press(pilot, spec.key)
        start = focused_id(app)
        await press(pilot, "ctrl+n", "ctrl+b")
        return start, focused_id(app)

    start, back = run_app(scenario, size=(100, 30))
    assert start == back


# -- self-check: everything below is about this file, not about the app ------

#: Split marker so the ban list below can name what it bans.
GUARD = "# -- self-check"


def test_no_step_reaches_past_the_keyboard() -> None:
    """This module may not drive a widget directly — that is the whole point.

    A walkthrough that quietly calls ``focus()`` proves nothing about whether
    a person could have got there, so the ban is enforced rather than trusted.
    Reads are allowed; only the ways of *driving* a widget are banned.
    """
    source = Path(__file__).read_text(encoding="utf-8")
    walkthrough, _, _ = source.partition(GUARD)
    # Drop the docstrings, which talk about the forbidden calls by name.
    body = re.sub(r'"""[\s\S]*?"""', "", walkthrough)
    for banned in (".focus()", ".value =", ".index =", ".action_", "call_later("):
        assert banned not in body, f"walkthrough reached past the keyboard: {banned}"
