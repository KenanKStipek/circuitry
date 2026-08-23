"""One theme: the colours and glyphs every view is allowed to draw with.

Most of the chrome is styled in CSS, where ``$success`` and friends already
resolve against whatever theme is loaded. This module exists for the handful
of places that assemble a Rich :class:`~rich.text.Text` by hand — the run
tree, mainly — and so cannot name a ``$token``. Those styles are looked up in
the running app's theme variables instead of being written out as ANSI colour
names, which is the difference between a run tree that reads on a light
terminal and one that does not.

The status glyphs are re-exported from :mod:`circuitry.tui.execution` rather
than redefined, so "which tick does this app use" has exactly one answer and
a view that wants one imports it instead of typing a lookalike.
"""

from __future__ import annotations

from collections.abc import Mapping

from .execution import DONE, FAILED, GLYPHS, PENDING, RUNNING, SKIPPED

__all__ = [
    "BAD_GLYPH",
    "GLYPHS",
    "OK_GLYPH",
    "STATUS_TOKENS",
    "status_style",
    "theme_colour",
]

#: The tick and the cross, for views that report a verdict rather than track
#: an effect. Same marks the run tree uses, so nothing has to be recognised
#: twice.
OK_GLYPH = GLYPHS[DONE]
BAD_GLYPH = GLYPHS[FAILED]

#: Effect status → (theme variable, extra style words).
#:
#: ``None`` for the variable means "no colour": ``dim`` and ``italic`` are
#: terminal attributes rather than colours, so they already say the same
#: thing on a light background as on a dark one and need no lookup.
STATUS_TOKENS: dict[str, tuple[str | None, str]] = {
    PENDING: (None, "dim"),
    RUNNING: ("warning", "bold"),
    DONE: ("success", ""),
    FAILED: ("error", "bold"),
    SKIPPED: (None, "dim italic"),
}


def theme_colour(name: str, variables: Mapping[str, str] | None) -> str:
    """The theme's colour for ``name``, or "" when it cannot be resolved.

    Textual allows a theme variable to hold a CSS-only value such as
    ``auto 60%``, which Rich would choke on. Anything that is not a plain
    colour is dropped rather than passed through — losing the colour is a
    cosmetic loss, raising mid-render is not.
    """
    if not variables:
        return ""
    value = variables.get(name, "")
    if not value or " " in value:
        return ""
    return value


def status_style(status: str, variables: Mapping[str, str] | None = None) -> str:
    """Rich style for a row describing an effect in ``status``.

    ``variables`` is the app's ``theme_variables``; pass ``None`` (or run
    before a theme is loaded) and the colour is simply left off, which
    degrades to the terminal's own foreground rather than to a wrong colour.
    """
    token, extra = STATUS_TOKENS.get(status, (None, ""))
    colour = theme_colour(token, variables) if token else ""
    return " ".join(part for part in (extra, colour) if part)
