"""The `?` help overlay, generated from the bindings that are actually live.

The rows are read off Textual's ``active_bindings`` for the screen the user
was looking at — the same table that dispatches keypresses — so the overlay
cannot drift from reality. Nothing is hand-maintained here.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, ClassVar

from textual.app import App, ComposeResult
from textual.binding import Binding, BindingType
from textual.containers import Vertical, VerticalScroll
from textual.screen import ModalScreen
from textual.widgets import Static

from .layout import ResponsiveLayout, fit, key_column_width

if TYPE_CHECKING:  # pragma: no cover - typing only
    from textual.events import Resize

__all__ = ["HelpOverlay", "HelpRow", "binding_rows"]


#: Group labels, in the order they are listed in the overlay.
GLOBAL_GROUP = "Global"
SCREEN_GROUP = "This screen"
FOCUS_GROUP = "Focused widget"
GROUP_ORDER = (GLOBAL_GROUP, SCREEN_GROUP, FOCUS_GROUP)


@dataclass(frozen=True)
class HelpRow:
    """One line of the overlay: the key as displayed, and what it does."""

    key: str
    description: str
    group: str = GLOBAL_GROUP


def binding_rows(app: App[Any]) -> list[HelpRow]:
    """Collect the app's live bindings as help rows, grouped by where they live.

    Bindings Textual owns internally (``system=True``), disabled bindings and
    bindings without a description are left out — they are not things a user
    can act on. Everything else comes straight from ``active_bindings``, the
    same table that dispatches the keypress, so the overlay cannot lie.
    """
    rows: list[HelpRow] = []
    seen: set[tuple[str, str]] = set()
    screen = app.screen if app.screen_stack else None
    for active in app.active_bindings.values():
        binding = active.binding
        if binding.system or not active.enabled or not binding.description:
            continue
        if active.node is app:
            group = GLOBAL_GROUP
        elif active.node is screen:
            group = SCREEN_GROUP
        else:
            group = FOCUS_GROUP
        row = HelpRow(app.get_key_display(binding), binding.description, group)
        token = (row.key, row.description)
        if token in seen:
            continue
        seen.add(token)
        rows.append(row)
    rows.sort(key=lambda row: GROUP_ORDER.index(row.group))
    return rows


class HelpTable(Static):
    """Key/description rows, re-flowed to whatever width is available."""

    def __init__(self, rows: list[HelpRow], **kwargs: str) -> None:
        super().__init__("", **kwargs)
        self.rows = rows

    def on_mount(self) -> None:
        self._reflow(self.size.width)

    def on_resize(self, event: Resize) -> None:
        self._reflow(event.size.width)

    def _reflow(self, width: int) -> None:
        if width <= 0:
            width = self.app.size.width
        if not self.rows:
            self.update(fit("No bindings on this screen.", width))
            return
        key_width = key_column_width(width)
        gap = 2 if width >= key_width + 4 else 1
        text_width = width - key_width - gap
        lines: list[str] = []
        group = ""
        for row in self.rows:
            if row.group != group:
                group = row.group
                if lines:
                    lines.append("")
                lines.append(fit(group, width))
            key = fit(row.key, key_width)
            lines.append(f"{key:>{key_width}}{' ' * gap}{fit(row.description, text_width)}")
        self.update("\n".join(lines))


class HelpOverlay(ResponsiveLayout, ModalScreen[None]):
    """Modal listing the bindings that were live on the screen underneath."""

    BINDINGS: ClassVar[list[BindingType]] = [
        Binding("escape", "close", "Close help"),
        Binding("question_mark", "close", "Close help", key_display="?"),
        Binding("q", "close", "Close help"),
    ]

    DEFAULT_CSS = """
    HelpOverlay {
        align: center middle;
    }

    HelpOverlay #help-dialog {
        width: 80%;
        max-width: 64;
        height: auto;
        max-height: 100%;
        padding: 1 2;
        border: round $accent;
        background: $surface;
    }

    HelpOverlay.-compact #help-dialog {
        width: 100%;
        max-width: 100%;
        padding: 0 1;
    }

    HelpOverlay.-tiny #help-dialog {
        padding: 0;
        border: none;
    }

    HelpOverlay #help-title {
        text-style: bold;
        color: $accent;
    }

    HelpOverlay #help-hint {
        color: $text-muted;
    }

    HelpOverlay.-tiny #help-title,
    HelpOverlay.-tiny #help-hint {
        display: none;
    }
    """

    def __init__(self, rows: list[HelpRow], *, title: str = "Keys") -> None:
        super().__init__(id="help-overlay")
        self.rows = rows
        self.heading = title

    def compose(self) -> ComposeResult:
        with Vertical(id="help-dialog"):
            yield Static(self.heading, id="help-title")
            with VerticalScroll(id="help-body"):
                yield HelpTable(self.rows, id="help-rows")
            yield Static("Esc or ? to close", id="help-hint")

    def action_close(self) -> None:
        self.dismiss(None)
