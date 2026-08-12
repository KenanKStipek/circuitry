"""The Circuitry Textual application.

Scaffold only: the home screen lists the planned views as placeholders so
later stories can fill them in one at a time. Importing this module
requires the ``tui`` extra — go through :mod:`circuitry.tui` instead.
"""

from __future__ import annotations

from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Vertical
from textual.widgets import Footer, Header, Label, ListItem, ListView, Static

#: Planned views, rendered as placeholders on the home screen.
PLANNED_VIEWS: tuple[tuple[str, str], ...] = (
    ("Library", "Browse bundled and shared orchestrations"),
    ("Run", "Execute an orchestration and watch effects stream"),
    ("Inspect", "Orchestration metadata, schema validation, and warnings"),
    ("Runs", "History of past runs and their final state"),
    ("Doctor", "Backend, config, and connectivity diagnostics"),
    ("Settings", "Effective configuration and where each value came from"),
)


class HomeView(Vertical):
    """Home screen: a title, a one-liner, and the placeholder view list."""

    def compose(self) -> ComposeResult:
        yield Static("Circuitry", id="home-title")
        yield Static(
            "Cybernetic orchestration framework. Views below are placeholders.",
            id="home-tagline",
        )
        yield ListView(
            *(
                ListItem(Label(f"{name} — {blurb}"), id=f"view-{name.lower()}")
                for name, blurb in PLANNED_VIEWS
            ),
            id="home-views",
        )


class CircuitryApp(App[None]):
    """Minimal Textual shell for the `cof` TUI."""

    TITLE = "Circuitry"
    SUB_TITLE = "cof"

    CSS = """
    #home {
        padding: 1 2;
    }

    #home-title {
        text-style: bold;
        color: $accent;
    }

    #home-tagline {
        color: $text-muted;
        margin-bottom: 1;
    }

    #home-views {
        border: round $panel;
        height: auto;
    }
    """

    BINDINGS = [
        Binding("q", "quit", "Quit"),
    ]

    def compose(self) -> ComposeResult:
        yield Header()
        yield HomeView(id="home")
        yield Footer()
