"""The Circuitry Textual application — the shell every view plugs into.

Responsibilities that live here and nowhere else: the screen stack and the
navigation on top of it (number keys, Tab, ``q``/Esc back-then-quit, Ctrl-C
always quits), the ``?`` help overlay, and the CSS breakpoints that keep the
chrome renderable in a tiny terminal. Views themselves live in
:mod:`circuitry.tui.screens`.

Importing this module requires the ``tui`` extra — go through
:mod:`circuitry.tui` instead.
"""

from __future__ import annotations

from pathlib import Path
from typing import ClassVar

from textual.app import App
from textual.binding import Binding, BindingType
from textual.screen import Screen

from .help import HelpOverlay, binding_rows
from .screens import VIEWS, HomeScreen, ViewScreen, ViewSpec

__all__ = ["PLANNED_VIEWS", "VIEWS", "CircuitryApp"]

#: Legacy view listing kept for callers that only need names and blurbs.
PLANNED_VIEWS: tuple[tuple[str, str], ...] = tuple((spec.name, spec.blurb) for spec in VIEWS)


class CircuitryApp(App[None]):
    """Minimal Textual shell for the `cof` TUI."""

    TITLE = "Circuitry"
    SUB_TITLE = "cof"

    #: Set by :meth:`launch_run` — the orchestration the Run view should pick
    #: up when it opens. ``None`` when the user navigated there themselves.
    pending_run: Path | None = None

    CSS = """
    CircuitryScreen #body {
        height: 1fr;
        padding: 1 2;
    }

    CircuitryScreen.-compact #body {
        padding: 0 1;
    }

    CircuitryScreen.-tiny #body {
        padding: 0;
    }

    /* A 4-row terminal has no rows to spare for chrome. */
    CircuitryScreen.-tiny #chrome-header,
    CircuitryScreen.-tiny #chrome-footer {
        display: none;
    }

    #home-title {
        text-style: bold;
        color: $accent;
    }

    #home-tagline {
        color: $text-muted;
        margin-bottom: 1;
    }

    CircuitryScreen.-compact #home-tagline {
        display: none;
    }

    #home-views {
        border: round $panel;
        height: auto;
    }

    CircuitryScreen.-compact #home-views {
        border: none;
    }

    .view-title {
        text-style: bold;
        color: $accent;
    }

    .view-blurb {
        margin-bottom: 1;
    }

    CircuitryScreen.-compact .view-blurb {
        margin-bottom: 0;
    }

    .view-note {
        color: $text-muted;
    }
    """

    # Declaration order is the order the help overlay lists them in, so the
    # view keys lead. Priority is needed where a screen-level binding would
    # otherwise win: Ctrl-C (copy) and Tab/Shift+Tab (focus cycling).
    BINDINGS: ClassVar[list[BindingType]] = [
        *(
            Binding(spec.key, f"show_view('{spec.slug}')", spec.name, show=False)
            for spec in VIEWS
        ),
        Binding("tab", "next_view", "Next view", priority=True),
        Binding("shift+tab", "previous_view", "Previous view", show=False, priority=True),
        Binding("question_mark", "help", "Help", key_display="?"),
        Binding("q", "back_or_quit", "Back / Quit"),
        Binding("escape", "back_or_quit", "Back / Quit", show=False),
        Binding("ctrl+c", "quit", "Quit", show=False, priority=True),
    ]

    def get_default_screen(self) -> HomeScreen:
        """Home is the bottom of the screen stack; every view stacks on it."""
        return HomeScreen(id="home")

    # -- screen stack helpers ------------------------------------------------

    @property
    def help_is_open(self) -> bool:
        """True when the help overlay is the top screen."""
        return isinstance(self.screen_stack[-1], HelpOverlay)

    def base_screen(self) -> Screen[None]:
        """The topmost screen that is not the help overlay."""
        for screen in reversed(self.screen_stack):
            if not isinstance(screen, HelpOverlay):
                return screen
        return self.screen_stack[0]

    def current_view(self) -> ViewSpec | None:
        """The view currently on screen, or ``None`` when home is showing."""
        screen = self.base_screen()
        return screen.spec if isinstance(screen, ViewScreen) else None

    def close_help(self) -> bool:
        """Dismiss the help overlay if it is open. Returns whether it was."""
        if not self.help_is_open:
            return False
        self.pop_screen()
        return True

    # -- navigation ----------------------------------------------------------

    def show_view(self, spec: ViewSpec) -> None:
        """Bring ``spec``'s screen to the front, replacing any current view."""
        self.close_help()
        current = self.current_view()
        if current is None:
            self.push_screen(spec.build())
        elif current.slug != spec.slug:
            self.switch_screen(spec.build())

    def launch_run(self, path: Path) -> None:
        """Hand a saved orchestration to the Run view.

        The Chat view's "run it now" and the Run view are separate stories, so
        the hand-off is a value on the app rather than a call into a screen
        that may still be a placeholder: whoever builds Run reads
        ``app.pending_run`` on mount and needs nothing from here.
        """
        self.pending_run = path
        for spec in VIEWS:
            if spec.slug == "run":
                self.show_view(spec)
                return

    def show_home(self) -> None:
        """Return to the home screen, dropping any view on top of it."""
        self.close_help()
        while len(self.screen_stack) > 1:
            self.pop_screen()

    def _cycle_view(self, step: int) -> None:
        order: list[ViewSpec | None] = [None, *VIEWS]
        index = order.index(self.current_view())
        target = order[(index + step) % len(order)]
        if target is None:
            self.show_home()
        else:
            self.show_view(target)

    # -- actions -------------------------------------------------------------

    def action_show_view(self, slug: str) -> None:
        """Open the view with this slug (bound to the number keys)."""
        for spec in VIEWS:
            if spec.slug == slug:
                self.show_view(spec)
                return

    def action_next_view(self) -> None:
        self._cycle_view(1)

    def action_previous_view(self) -> None:
        self._cycle_view(-1)

    def action_back_or_quit(self) -> None:
        """Close help, else go back to home, else quit.

        One key that always means "get me out of here", with quitting only
        ever one level deep so it is never a surprise.
        """
        if self.close_help():
            return
        if self.current_view() is not None:
            self.show_home()
            return
        self.exit()

    def action_help(self) -> None:
        """Toggle the help overlay for the screen underneath."""
        if self.close_help():
            return
        screen = self.base_screen()
        heading = getattr(screen, "sub_title", "") or "Circuitry"
        self.push_screen(HelpOverlay(binding_rows(self), title=f"Keys — {heading}"))
