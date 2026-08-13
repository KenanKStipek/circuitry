"""Screen registry and the base screen every Circuitry view builds on.

One place declares which views exist, what they are called, which number key
reaches them and which class renders them. Later stories swap a view's
``factory`` from :class:`PlaceholderScreen` to the real thing without
touching navigation, the help overlay or the home screen.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import TYPE_CHECKING, ClassVar

from textual.app import ComposeResult
from textual.containers import VerticalScroll
from textual.screen import Screen
from textual.widget import Widget
from textual.widgets import Footer, Header, Label, ListItem, ListView, Static

from .layout import ResponsiveLayout

if TYPE_CHECKING:  # pragma: no cover - typing only
    from textual.events import Mount

__all__ = [
    "VIEWS",
    "CircuitryScreen",
    "HomeScreen",
    "PlaceholderScreen",
    "ViewScreen",
    "ViewSpec",
    "view_by_key",
    "view_index",
]


class CircuitryScreen(ResponsiveLayout, Screen[None]):
    """Base screen: header, footer, and size classes maintained for you.

    Subclasses implement :meth:`compose_body`; the chrome around it is
    supplied here so every view shares one layout and one set of breakpoints.
    The body is a scroll container, which is what keeps a full-size screen
    renderable inside a four-row terminal. A view that lays out its own
    panes (and scrolls inside them) overrides :attr:`BODY_CONTAINER`.
    """

    #: Container the body widgets are wrapped in.
    BODY_CONTAINER: ClassVar[type[Widget]] = VerticalScroll

    def compose(self) -> ComposeResult:
        yield Header(id="chrome-header")
        yield self.BODY_CONTAINER(*self.compose_body(), id="body")
        yield Footer(id="chrome-footer")

    def compose_body(self) -> ComposeResult:
        """Yield the widgets that make up this screen's body."""
        return iter(())


class ViewScreen(CircuitryScreen):
    """A screen that belongs to a registered view."""

    def __init__(
        self,
        spec: ViewSpec,
        *,
        name: str | None = None,
        id: str | None = None,  # noqa: A002 - Textual's parameter name
        classes: str | None = None,
    ) -> None:
        super().__init__(name=name, id=id or f"screen-{spec.slug}", classes=classes)
        self.spec = spec

    def _on_mount(self, event: Mount) -> None:
        super()._on_mount(event)
        self.sub_title = self.spec.name


class PlaceholderScreen(ViewScreen):
    """Stand-in body for a view that has not been built yet."""

    def compose_body(self) -> ComposeResult:
        yield Static(self.spec.name, classes="view-title")
        yield Static(self.spec.blurb, classes="view-blurb")
        yield Static(
            "Not built yet — this view lands in a later story.",
            classes="view-note",
        )


@dataclass(frozen=True)
class ViewSpec:
    """A registered view: its identity, its hotkey and how to build it."""

    slug: str
    name: str
    blurb: str
    key: str
    #: Overridden as each real view lands; ``None`` means "placeholder".
    factory: Callable[[ViewSpec], CircuitryScreen] | None = None

    def build(self) -> CircuitryScreen:
        """Instantiate this view's screen."""
        if self.factory is None:
            return PlaceholderScreen(self)
        return self.factory(self)


# Factories are imported inside the function so this module stays the single
# registry without depending on the screens that register themselves in it.


def _library_screen(spec: ViewSpec) -> CircuitryScreen:
    """Build the library view, imported late to keep the registry import-cheap."""
    from .library import LibraryScreen

    return LibraryScreen(spec)


def _build_run_screen(spec: ViewSpec) -> CircuitryScreen:
    """Imported lazily: run_view imports this module for its base class."""
    from .run_view import RunScreen

    return RunScreen(spec)


def _doctor(spec: ViewSpec) -> CircuitryScreen:
    from .doctor import DoctorScreen

    return DoctorScreen(spec)


def _settings(spec: ViewSpec) -> CircuitryScreen:
    from .doctor import SettingsScreen

    return SettingsScreen(spec)


def _validate(spec: ViewSpec) -> CircuitryScreen:
    from .validate import ValidateScreen

    return ValidateScreen(spec)


#: Every view the shell knows about, in navigation (and number key) order.
VIEWS: tuple[ViewSpec, ...] = (
    ViewSpec(
        "library",
        "Library",
        "Browse bundled and shared orchestrations",
        "1",
        factory=_library_screen,
    ),
    ViewSpec(
        "run",
        "Run",
        "Execute an orchestration and watch effects stream",
        "2",
        factory=_build_run_screen,
    ),
    ViewSpec(
        "inspect",
        "Inspect",
        "Orchestration metadata, schema validation, and warnings",
        "3",
    ),
    ViewSpec("runs", "Runs", "History of past runs and their final state", "4"),
    ViewSpec(
        "doctor",
        "Doctor",
        "Backend, config, and connectivity diagnostics",
        "5",
        factory=_doctor,
    ),
    ViewSpec(
        "settings",
        "Settings",
        "Effective configuration and where each value came from",
        "6",
        factory=_settings,
    ),
    ViewSpec(
        "validate",
        "Validate",
        "Check an orchestration file for schema, compile, cycle and preflight errors",
        "7",
        factory=_validate,
    ),
)


def view_by_key(key: str) -> ViewSpec | None:
    """Look up a view by the number key that reaches it."""
    for spec in VIEWS:
        if spec.key == key:
            return spec
    return None


def view_index(spec: ViewSpec) -> int:
    """Position of ``spec`` in :data:`VIEWS`."""
    return VIEWS.index(spec)


class HomeScreen(CircuitryScreen):
    """Landing screen: the view list, one row per registered view."""

    def compose_body(self) -> ComposeResult:
        yield Static("Circuitry", id="home-title")
        yield Static(
            "Cybernetic orchestration framework. Pick a view, or press ? for keys.",
            id="home-tagline",
        )
        yield ListView(
            *(
                ListItem(
                    Label(f"{spec.key}  {spec.name} — {spec.blurb}"),
                    id=f"view-{spec.slug}",
                )
                for spec in VIEWS
            ),
            id="home-views",
        )

    def _on_mount(self, event: Mount) -> None:
        super()._on_mount(event)
        self.sub_title = "Home"

    def on_list_view_selected(self, event: ListView.Selected) -> None:
        """Enter (or a click) on a row opens that view."""
        event.stop()
        slug = (event.item.id or "").removeprefix("view-")
        for spec in VIEWS:
            if spec.slug == slug:
                self.app.call_later(self._open, spec)
                return

    def _open(self, spec: ViewSpec) -> None:
        show_view = getattr(self.app, "show_view", None)
        if callable(show_view):
            show_view(spec)
