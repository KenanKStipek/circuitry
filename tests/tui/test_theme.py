"""One theme: the app may not invent a colour or a glyph on the side.

Two rules, both enforced against the source rather than against a rendering,
because the failure mode is a *new* view quietly hard-coding ``green`` — and a
snapshot of the views that exist today would never notice.

1. **Colours come from the theme.** A CSS rule names a ``$token``; a Rich
   style resolves one through :mod:`circuitry.tui.theme`. Neither writes out
   an ANSI colour name, which is what keeps the app readable on a light
   terminal as well as a dark one.
2. **There is one tick and one cross.** :data:`circuitry.tui.execution.GLYPHS`
   is the whole vocabulary; a lookalike from a neighbouring Unicode block is a
   second theme wearing the first one's clothes.
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

import pytest

from circuitry.tui import execution, theme

pytest.importorskip("textual")

from textual.pilot import Pilot

from circuitry.tui.app import CircuitryApp

TUI = Path(execution.__file__).parent

#: Colour words that mean a fixed colour rather than a themed one. ``dim`` and
#: ``italic`` are absent on purpose: they are attributes, not colours, and say
#: the same thing on either background.
ANSI_COLOURS = (
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
)

#: Marks that are *not* ours. Each is a plausible-looking substitute for
#: something in ``GLYPHS`` — the failure this guards against is a view drawing
#: a verdict with a tick the run tree has never used.
LOOKALIKES = ("✔", "✘", "✅", "❌", "✖", "☑", "√")


def tui_sources() -> list[Path]:
    return sorted(path for path in TUI.glob("*.py") if path.name != "__init__.py")


def test_the_glyph_table_is_the_only_glyph_table() -> None:
    """No module draws a mark that means the same as one in ``GLYPHS``."""
    offenders = {
        path.name: [mark for mark in LOOKALIKES if mark in path.read_text(encoding="utf-8")]
        for path in tui_sources()
    }
    assert not {name: marks for name, marks in offenders.items() if marks}


def test_the_verdict_glyphs_are_the_run_trees() -> None:
    """A "valid draft" tick is the same tick a finished effect gets."""
    assert execution.GLYPHS[execution.DONE] == theme.OK_GLYPH
    assert execution.GLYPHS[execution.FAILED] == theme.BAD_GLYPH


@pytest.mark.parametrize("colour", ANSI_COLOURS)
def test_no_module_hard_codes_an_ansi_colour(colour: str) -> None:
    """Styles name a theme token, never a colour.

    Matched as a whole word inside a quoted string so ``"yellow"`` trips it
    and ``mellow`` does not.
    """
    pattern = re.compile(rf'"[^"\n]*\b{colour}\b[^"\n]*"')
    offenders = [
        path.name
        for path in tui_sources()
        if pattern.search(path.read_text(encoding="utf-8"))
    ]
    assert not offenders, f"{colour!r} written out in {offenders}"


def test_status_style_resolves_through_the_running_theme() -> None:
    """Each status that has a colour gets it from the theme's variables."""
    variables = {"success": "#4EBF71", "warning": "#FEA62B", "error": "#B93C5B"}
    assert theme.status_style(execution.DONE, variables) == "#4EBF71"
    assert theme.status_style(execution.RUNNING, variables) == "bold #FEA62B"
    assert theme.status_style(execution.FAILED, variables) == "bold #B93C5B"
    # Attributes need no theme, so they survive a missing one.
    assert theme.status_style(execution.PENDING, variables) == "dim"
    assert theme.status_style(execution.SKIPPED, {}) == "dim italic"


def test_a_theme_without_the_variable_loses_the_colour_not_the_row() -> None:
    """A CSS-only value (``auto 60%``) is dropped rather than handed to Rich."""
    assert theme.status_style(execution.DONE, {"success": "auto 60%"}) == ""
    assert theme.status_style(execution.DONE, None) == ""
    assert theme.status_style("not-a-status", {"success": "#fff"}) == ""


def test_every_status_the_tree_can_draw_has_a_style() -> None:
    """A status with no entry would render as unstyled text, silently."""
    assert set(theme.STATUS_TOKENS) == set(execution.GLYPHS)


# -- the light-terminal sanity check -----------------------------------------
#
# Everything above works on fabricated variables, which proves the lookup and
# not the wiring. These two boot the real app and read the real theme, because
# "is this legible on a light terminal" is a question about what the running
# app resolves, not about what a dict says.


LIGHT = "solarized-light"
DARK = "textual-dark"


def _styles(app: CircuitryApp) -> list[str]:
    """The run tree's colours for done/running/failed under the live theme."""
    variables = app.theme_variables
    return [
        theme.status_style(status, variables)
        for status in (execution.DONE, execution.RUNNING, execution.FAILED)
    ]


def test_the_run_tree_follows_the_theme_onto_a_light_background(run_app: Any) -> None:
    """Switching to a light theme changes the colours the tree draws with.

    This is the regression the hard-coded table could not have: ``green`` on
    Solarized Light is the terminal's own bright green on near-white paper.
    The theme's ``$success`` there is a dark olive, and that is what the tree
    now asks for.
    """

    async def scenario(pilot: Pilot[Any]) -> tuple[list[str], list[str]]:
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        app.theme = DARK
        await pilot.pause()
        dark = _styles(app)
        app.theme = LIGHT
        await pilot.pause()
        return dark, _styles(app)

    dark, light = run_app(scenario)
    assert all(dark), "the dark theme resolved no colour at all"
    assert all(light), "the light theme resolved no colour at all"
    assert dark != light, "the tree drew the same colours on both backgrounds"


@pytest.mark.parametrize("name", [DARK, LIGHT, "textual-light", "gruvbox", "nord"])
def test_no_theme_leaves_the_tree_asking_for_a_raw_ansi_colour(
    run_app: Any, name: str
) -> None:
    """Whatever the theme, the style is the theme's answer — not ``"green"``.

    ``ansi-dark``/``ansi-light`` are excluded on purpose: those themes exist
    precisely to hand the terminal's own palette back, so ``ansi_green`` there
    is the correct answer rather than a hard-coded one.
    """

    async def scenario(pilot: Pilot[Any]) -> list[str]:
        app: CircuitryApp = pilot.app  # type: ignore[assignment]
        app.theme = name
        await pilot.pause()
        return _styles(app)

    for style in run_app(scenario):
        words = style.split()
        assert words, f"{name} resolved no style"
        assert not any(word in ANSI_COLOURS for word in words), f"{name} → {style!r}"
