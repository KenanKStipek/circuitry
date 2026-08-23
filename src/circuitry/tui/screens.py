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

from .layout import ResponsiveLayout, fit

if TYPE_CHECKING:  # pragma: no cover - typing only
    from textual.events import Mount, Resize

__all__ = [
    "HOME_ROW_BUDGET",
    "VIEWS",
    "CircuitryScreen",
    "HomeScreen",
    "PlaceholderScreen",
    "ViewRow",
    "ViewScreen",
    "ViewSpec",
    "home_row",
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
        # The footer truncates from the right, so every cell it spends on a
        # key nobody asked for is a cell taken off "? Help" — which is the
        # one row that leads to all the others. Textual's command palette
        # hint costs twelve of them and is still listed in the overlay.
        yield Footer(id="chrome-footer", show_command_palette=False)

    def compose_body(self) -> ComposeResult:
        """Yield the widgets that make up this screen's body."""
        return iter(())

    def confirm_leave(self, proceed: Callable[[], None]) -> None:
        """Guard navigation away from this screen; call ``proceed`` when free.

        The default is to leave immediately, which is what every screen
        without unsaved work wants. A screen holding an unsaved edit (the
        Profile view) overrides this to ask first, and calls ``proceed`` only
        if the answer is yes — so one hook covers the number keys, Tab, and
        ``q``/``Esc`` alike.
        """
        proceed()


class ViewScreen(CircuitryScreen):
    """A screen that belongs to a registered view."""

    def __init__(
        self,
        spec: ViewSpec,
        *,
        name: str | None = None,
        id: str | None = None,  # shadows the builtin: Textual's parameter name
        classes: str | None = None,
    ) -> None:
        super().__init__(name=name, id=id or f"screen-{spec.slug}", classes=classes)
        self.spec = spec

    def _on_mount(self, event: Mount) -> None:
        super()._on_mount(event)
        self.sub_title = self.spec.name


class PlaceholderScreen(ViewScreen):
    """Stand-in body for a view that has not been built yet."""

    #: What a placeholder says instead of nothing. "Later story" is our word
    #: for it, not the reader's — they want to know what to press *now*.
    NOTE = "Not built yet. Until it is, 7 Validate covers most of this ground."

    def compose_body(self) -> ComposeResult:
        yield Static(self.spec.name, classes="view-title")
        yield Static(self.spec.blurb, classes="view-blurb")
        yield Static(self.NOTE, classes="view-note")


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


def _runs(spec: ViewSpec) -> CircuitryScreen:
    """Imported lazily: runs_view imports this module for its base class."""
    from .runs_view import RunsScreen

    return RunsScreen(spec)


def _doctor(spec: ViewSpec) -> CircuitryScreen:
    from .doctor import DoctorScreen

    return DoctorScreen(spec)


def _settings(spec: ViewSpec) -> CircuitryScreen:
    from .doctor import SettingsScreen

    return SettingsScreen(spec)


def _validate(spec: ViewSpec) -> CircuitryScreen:
    from .validate import ValidateScreen

    return ValidateScreen(spec)


def _chat(spec: ViewSpec) -> CircuitryScreen:
    from .chat import ChatScreen

    return ChatScreen(spec)


def _profiles(spec: ViewSpec) -> CircuitryScreen:
    from .profile_view import ProfileScreen

    return ProfileScreen(spec)


#: Every view the shell knows about, in navigation (and number key) order.
#:
#: Blurbs are kept short enough that ``"<key>  <name> — <blurb>"`` fits an
#: 80-column home list whole — see :data:`HOME_ROW_BUDGET`, which is checked.
#: Narrower than that and :class:`ViewRow` ellipsises them; wider and they
#: simply read.
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
        "Run an orchestration and watch the effects land",
        "2",
        factory=_build_run_screen,
    ),
    ViewSpec(
        "inspect",
        "Inspect",
        "Orchestration metadata, schema validation, and warnings",
        "3",
    ),
    ViewSpec(
        "runs",
        "Runs",
        "Walk a run's state as a tree, open a saved one, replay the last",
        "4",
        factory=_runs,
    ),
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
        "Check a file for schema, compile, cycle and preflight errors",
        "7",
        factory=_validate,
    ),
    ViewSpec(
        "chat",
        "Chat",
        "Describe a pipeline; the wizard writes the orchestration",
        "8",
        factory=_chat,
    ),
    ViewSpec(
        "profiles",
        "Profiles",
        "Per-effect models, toggles and inputs, saved under a name",
        "9",
        factory=_profiles,
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


#: Cells a home row gets in an 80-column terminal: the screen, less the body's
#: horizontal padding and the list's border. Every row is written to fit it,
#: because an 80-column terminal is the one everybody has.
HOME_ROW_BUDGET = 80 - 4 - 2


def home_row(spec: ViewSpec) -> str:
    """The home list's line for ``spec`` — the key, the name, the blurb."""
    return f"{spec.key}  {spec.name} — {spec.blurb}"


class ViewRow(Label):
    """One home row, ellipsised to the width it actually gets.

    A ``Label`` sizes itself to its text and then lets the list clip whatever
    hangs over the edge, which reads as a sentence that stopped mid-word. The
    row is stretched to the list's width in CSS so ``size.width`` is the real
    budget, and :func:`~circuitry.tui.layout.fit` spends it.
    """

    def __init__(self, spec: ViewSpec) -> None:
        super().__init__(id=f"row-{spec.slug}")
        self.line = home_row(spec)

    def on_mount(self) -> None:
        self._reflow(self.size.width)

    def on_resize(self, event: Resize) -> None:
        self._reflow(event.size.width)

    def _reflow(self, width: int) -> None:
        self.update(fit(self.line, width) if width > 0 else self.line)


class HomeScreen(CircuitryScreen):
    """Landing screen: the view list, one row per registered view."""

    def compose_body(self) -> ComposeResult:
        yield Static("Circuitry", id="home-title")
        yield Static(
            "Nine views, no mouse. Pick one, or press ? and the app will "
            "tell you every key it knows.",
            id="home-tagline",
        )
        yield ListView(
            *(ListItem(ViewRow(spec), id=f"view-{spec.slug}") for spec in VIEWS),
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
