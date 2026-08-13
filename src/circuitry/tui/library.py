"""Library view — browse, search and eject the curation library.

Three panes: a category tree, the orchestrations in that category, and a
detail pane rendering the manifest metadata for whatever is highlighted.
``/`` searches every entry (name, intent, tags) and ``e`` ejects the YAML to
the current directory.

Every fact shown here comes from :func:`circuitry.cli.registry.load_index` —
the same reader behind ``cof list`` and ``cof info`` — and an eject goes
through :func:`circuitry.cli.registry.eject_text`, the same reader behind
``cof eject``. Nothing in this module parses the manifest or a curation file
itself, so the TUI cannot drift from the CLI.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, ClassVar, cast

from textual.app import ComposeResult
from textual.binding import Binding, BindingType
from textual.containers import Horizontal, Vertical, VerticalScroll
from textual.screen import ModalScreen
from textual.widget import Widget
from textual.widgets import Input, OptionList, Static, Tree

from ..cli.registry import eject_destination, eject_text, load_index, write_ejected
from .layout import ResponsiveLayout
from .screens import ViewScreen

if TYPE_CHECKING:  # pragma: no cover - typing only
    from collections.abc import Sequence
    from pathlib import Path

    from textual.events import Mount

    from .app import CircuitryApp

__all__ = [
    "ConfirmOverwrite",
    "LibraryEntry",
    "LibraryScreen",
    "categories",
    "detail_lines",
    "load_entries",
    "search",
]

#: Label of the tree root, which means "no category filter".
ALL_LABEL = "All"

#: Shown wherever the manifest has nothing to say for a field.
MISSING = "—"

#: Width of the label column in the detail pane.
LABEL_WIDTH = 12


# -- data ---------------------------------------------------------------------


@dataclass(frozen=True)
class LibraryEntry:
    """One curation manifest entry, in the shape this view needs.

    ``raw`` is the untouched index entry, so anything the detail pane or an
    eject needs is available without going back to the manifest.
    """

    name: str
    category: str
    file: str
    intent: str
    raw: dict[str, Any] = field(default_factory=dict, repr=False)

    @classmethod
    def from_index(cls, entry: dict[str, Any]) -> LibraryEntry:
        """Adapt a `cli.registry` index entry."""
        name = str(entry.get("name") or "")
        return cls(
            name=name,
            category=str(entry.get("category") or "") or (name.split("/")[0] if "/" in name else ""),
            file=str(entry.get("file") or ""),
            intent=str(entry.get("intent") or entry.get("description") or ""),
            raw=entry,
        )

    @property
    def search_text(self) -> str:
        """Everything ``/`` searches over: name, intent and tags, lowercased."""
        tags = _as_list(self.raw.get("tags"))
        return " ".join([self.name, self.intent, *tags]).lower()


def load_entries() -> list[LibraryEntry]:
    """Every manifest entry, in manifest order."""
    return [LibraryEntry.from_index(entry) for entry in load_index()]


def categories(entries: Sequence[LibraryEntry]) -> list[tuple[str, int]]:
    """``(category, count)`` pairs in first-seen order."""
    counts: dict[str, int] = {}
    for entry in entries:
        key = entry.category or "uncategorised"
        counts[key] = counts.get(key, 0) + 1
    return list(counts.items())


# -- search -------------------------------------------------------------------


def _is_subsequence(needle: str, haystack: str) -> bool:
    """True when every character of ``needle`` appears in order in ``haystack``."""
    position = 0
    for char in needle:
        position = haystack.find(char, position) + 1
        if position == 0:
            return False
    return True


def match_rank(query: str, entry: LibraryEntry) -> int | None:
    """How well ``entry`` matches ``query``; lower is better, ``None`` is no match.

    Exact-ish name hits beat body hits, and both beat a loose fuzzy hit, so
    typing "hello" puts ``learn/hello`` first rather than burying it under
    whatever else happens to contain those five letters in order.
    """
    needle = "".join(query.lower().split())
    if not needle:
        return 0
    name = entry.name.lower()
    tail = name.rsplit("/", 1)[-1]
    # An exact name wins outright, and the tail counts as a prefix too:
    # "critique" means utilities/critique, not patterns/critique_refine_loop.
    if needle in (name, tail):
        return 1
    if name.startswith(needle) or tail.startswith(needle):
        return 2
    if needle in name:
        return 3
    if needle in entry.search_text:
        return 4
    if _is_subsequence(needle, name):
        return 5
    if _is_subsequence(needle, entry.search_text):
        return 6
    return None


def search(
    entries: Sequence[LibraryEntry],
    query: str = "",
    category: str | None = None,
) -> list[LibraryEntry]:
    """Entries matching ``query``, best match first, optionally one category.

    A blank query keeps manifest order — browsing should not reshuffle under
    you — while a real query sorts by rank and then name for stability.
    """
    scoped = [e for e in entries if category is None or (e.category or "uncategorised") == category]
    if not "".join(query.split()):
        return list(scoped)
    ranked: list[tuple[int, str, LibraryEntry]] = []
    for entry in scoped:
        rank = match_rank(query, entry)
        if rank is not None:
            ranked.append((rank, entry.name, entry))
    ranked.sort(key=lambda row: (row[0], row[1]))
    return [entry for _, _, entry in ranked]


# -- detail rendering ---------------------------------------------------------


def _as_list(value: Any) -> list[str]:
    """Coerce a manifest field to a list of strings."""
    if value is None:
        return []
    if isinstance(value, str):
        return [value]
    if isinstance(value, dict):
        return [str(key) for key in value]
    if isinstance(value, (list, tuple)):
        return [str(item) for item in value]
    return [str(value)]


def _field(label: str, value: str) -> str:
    return f"{label:<{LABEL_WIDTH}}{value or MISSING}"


def _io_lines(value: Any) -> list[str]:
    """Rows for an inputs/outputs block: a signature line, then the prose.

    Two lines per field rather than one wide one, because the detail pane is
    the narrowest thing on screen and a wrapped description reads as garbage.
    """
    rows: list[dict[str, Any]] = []
    if isinstance(value, dict):
        rows = [
            {"name": key, **spec} if isinstance(spec, dict) else {"name": key}
            for key, spec in value.items()
        ]
    elif isinstance(value, (list, tuple)):
        rows = [item for item in value if isinstance(item, dict)]
    lines: list[str] = []
    for row in rows:
        name = str(row.get("name", "?"))
        kind = str(row.get("type", "") or "")
        required = " (required)" if row.get("required") else ""
        lines.append(f"  {name}{f' : {kind}' if kind else ''}{required}")
        description = str(row.get("description", "") or "")
        if description:
            lines.append(f"    {description}")
    return lines


def detail_lines(entry: LibraryEntry | None) -> list[str]:
    """The detail pane's text for ``entry`` — the manifest, rendered.

    Returns the empty-selection copy for ``None`` so the pane is never blank.
    """
    if entry is None:
        return [
            "Nothing selected",
            "",
            "Pick an orchestration on the left, or press / to search.",
        ]
    raw = entry.raw
    lines = [entry.name, ""]
    if entry.intent:
        lines += [entry.intent, ""]
    lines += [
        _field("Category", entry.category),
        _field("File", entry.file),
        _field("Difficulty", str(raw.get("difficulty") or "")),
        _field("Primitives", ", ".join(_as_list(raw.get("primitives")))),
        _field("Backends", ", ".join(_as_list(raw.get("backends")))),
        _field("Tags", ", ".join(_as_list(raw.get("tags")))),
    ]
    description = str(raw.get("description") or "")
    if description and description != entry.intent:
        lines += ["", "Description", f"  {description}"]
    when = str(raw.get("when_to_use") or "")
    if when:
        lines += ["", "When to use", f"  {when}"]
    inputs = _io_lines(raw.get("inputs"))
    lines += ["", "Inputs", *(inputs or [f"  {MISSING}"])]
    outputs = _io_lines(raw.get("outputs"))
    lines += ["", "Outputs", *(outputs or [f"  {MISSING}"])]
    example = str(raw.get("example") or "")
    if example:
        lines += ["", "Example", f"  {example}"]
    lines += ["", f"Press e to eject to ./{eject_destination(raw)}"]
    return lines


def empty_state_lines(query: str) -> list[str]:
    """Copy for the "nothing matched" panel — never an empty box."""
    return [
        f'No orchestration matches "{query.strip()}".',
        "",
        "Try fewer letters, or search an intent word or a tag.",
        "Esc clears the search and shows the whole library.",
    ]


# -- widgets ------------------------------------------------------------------


class ConfirmOverwrite(ResponsiveLayout, ModalScreen[bool]):
    """Yes/no gate in front of overwriting a file an eject would clobber."""

    BINDINGS: ClassVar[list[BindingType]] = [
        Binding("y", "confirm", "Overwrite"),
        Binding("enter", "confirm", "Overwrite", show=False),
        Binding("n", "cancel", "Keep file"),
        Binding("escape", "cancel", "Keep file", show=False),
        Binding("q", "cancel", "Keep file", show=False),
    ]

    DEFAULT_CSS = """
    ConfirmOverwrite {
        align: center middle;
    }

    ConfirmOverwrite #confirm-dialog {
        width: 80%;
        max-width: 56;
        height: auto;
        padding: 1 2;
        border: round $warning;
        background: $surface;
    }

    ConfirmOverwrite.-compact #confirm-dialog {
        width: 100%;
        max-width: 100%;
        padding: 0 1;
    }

    ConfirmOverwrite.-tiny #confirm-dialog {
        padding: 0;
        border: none;
    }

    ConfirmOverwrite #confirm-title {
        text-style: bold;
        color: $warning;
    }

    ConfirmOverwrite #confirm-hint {
        color: $text-muted;
    }

    ConfirmOverwrite.-tiny #confirm-title {
        display: none;
    }
    """

    def __init__(self, dest: Path) -> None:
        super().__init__(id="confirm-overwrite")
        self.dest = dest

    def compose(self) -> ComposeResult:
        with Vertical(id="confirm-dialog"):
            yield Static("File already exists", id="confirm-title")
            yield Static(f"{self.dest} — overwrite it?", id="confirm-path", markup=False)
            yield Static("y overwrite · n keep", id="confirm-hint")

    def action_confirm(self) -> None:
        self.dismiss(True)

    def action_cancel(self) -> None:
        self.dismiss(False)


class LibraryScreen(ViewScreen):
    """Browse, search and eject the curation library."""

    #: The panes scroll individually, so the body itself must not scroll.
    BODY_CONTAINER: ClassVar[type[Widget]] = Vertical

    BINDINGS: ClassVar[list[BindingType]] = [
        Binding("slash", "search", "Search"),
        Binding("e", "eject", "Eject"),
        Binding("escape", "clear_or_back", "Back", show=False),
    ]

    DEFAULT_CSS = """
    LibraryScreen #library-heading {
        text-style: bold;
        color: $accent;
    }

    LibraryScreen #library-search {
        height: 3;
        margin: 0;
    }

    LibraryScreen #library-panes {
        height: 1fr;
        min-height: 3;
    }

    LibraryScreen #library-tree {
        width: 22%;
        border: round $panel;
    }

    LibraryScreen #library-list-pane {
        width: 30%;
        border: round $panel;
    }

    LibraryScreen #library-list {
        height: 1fr;
        border: none;
        padding: 0;
        text-wrap: nowrap;
        text-overflow: ellipsis;
        scrollbar-size-horizontal: 0;
    }

    LibraryScreen #library-empty {
        height: 1fr;
        padding: 0 1;
        color: $text-muted;
    }

    LibraryScreen #library-detail-pane {
        width: 1fr;
        border: round $panel;
        padding: 0 1;
    }

    LibraryScreen #library-status {
        height: 1;
        color: $text-muted;
    }

    /* Narrow terminals drop the tree and stack the two remaining panes. */
    LibraryScreen.-compact #library-heading,
    LibraryScreen.-compact #library-tree {
        display: none;
    }

    LibraryScreen.-compact #library-panes {
        layout: vertical;
    }

    LibraryScreen.-compact #library-list-pane,
    LibraryScreen.-compact #library-detail-pane {
        width: 1fr;
        height: 1fr;
        border: none;
        padding: 0;
    }

    /* A four-row terminal keeps the list and nothing else. */
    LibraryScreen.-tiny #library-search,
    LibraryScreen.-tiny #library-detail-pane,
    LibraryScreen.-tiny #library-status {
        display: none;
    }
    """

    def __init__(self, spec: Any) -> None:
        super().__init__(spec)
        self.entries: list[LibraryEntry] = load_entries()
        self.matches: list[LibraryEntry] = list(self.entries)
        self.category: str | None = None
        self.term = ""
        self.notice = ""

    # -- composition ---------------------------------------------------------

    def compose_body(self) -> ComposeResult:
        # Built by construction rather than with ``with`` blocks: the base
        # screen unpacks this generator into its body container, which the
        # compose-stack the context managers rely on does not survive.
        yield Static(f"{self.spec.name} — {self.spec.blurb}", id="library-heading")
        yield Input(placeholder="/ to search — name, intent, tags", id="library-search")
        yield Horizontal(
            Tree(ALL_LABEL, id="library-tree"),
            Vertical(
                OptionList(id="library-list"),
                Static("", id="library-empty", markup=False),
                id="library-list-pane",
            ),
            VerticalScroll(
                Static("", id="library-detail", markup=False),
                id="library-detail-pane",
            ),
            id="library-panes",
        )
        yield Static("", id="library-status", markup=False)

    def _on_mount(self, event: Mount) -> None:
        super()._on_mount(event)
        self._build_tree()
        self._refresh_list()
        self._entry_list.focus()

    # -- widget handles ------------------------------------------------------

    @property
    def _categories(self) -> Tree[str]:
        return self.query_one("#library-tree", Tree)

    @property
    def _entry_list(self) -> OptionList:
        return self.query_one("#library-list", OptionList)

    @property
    def _search_box(self) -> Input:
        return self.query_one("#library-search", Input)

    @property
    def selected_entry(self) -> LibraryEntry | None:
        """The highlighted entry, or ``None`` when the list is empty."""
        index = self._entry_list.highlighted
        if index is None or not 0 <= index < len(self.matches):
            return None
        return self.matches[index]

    # -- population ----------------------------------------------------------

    def _build_tree(self) -> None:
        tree = self._categories
        tree.show_root = True
        tree.root.data = ""
        tree.root.label = f"{ALL_LABEL} ({len(self.entries)})"
        for name, count in categories(self.entries):
            tree.root.add_leaf(f"{name} ({count})", data=name)
        tree.root.expand()

    def _refresh_list(self) -> None:
        """Re-run the filter and repaint the list, detail pane and status."""
        self.matches = search(self.entries, self.term, self.category)
        options = self._entry_list
        options.clear_options()
        if self.matches:
            options.add_options([entry.name for entry in self.matches])
            options.highlighted = 0
        options.display = bool(self.matches)
        empty = self.query_one("#library-empty", Static)
        empty.display = not self.matches
        empty.update("\n".join(empty_state_lines(self.term)))
        self._refresh_detail()
        self._refresh_status()

    def _refresh_detail(self) -> None:
        detail = self.query_one("#library-detail", Static)
        detail.update("\n".join(detail_lines(self.selected_entry)))

    def _refresh_status(self) -> None:
        scope = self.category or "all"
        counted = f"{len(self.matches)}/{len(self.entries)} in {scope}"
        if self.term.strip():
            counted += f' · "{self.term.strip()}"'
        text = f"{counted} — {self.notice}" if self.notice else counted
        self.query_one("#library-status", Static).update(text)

    def _set_status(self, message: str) -> None:
        self.notice = message
        self._refresh_status()

    # -- events --------------------------------------------------------------

    def on_tree_node_highlighted(self, event: Tree.NodeHighlighted[str]) -> None:
        """Moving the tree cursor re-filters the list."""
        event.stop()
        self.category = event.node.data or None
        self._refresh_list()

    def on_option_list_option_highlighted(self, event: OptionList.OptionHighlighted) -> None:
        event.stop()
        self._refresh_detail()

    def on_option_list_option_selected(self, event: OptionList.OptionSelected) -> None:
        """Enter on a row is a shortcut for "show me this one"."""
        event.stop()
        self._refresh_detail()

    def on_input_changed(self, event: Input.Changed) -> None:
        event.stop()
        self.set_query(event.value)

    def on_input_submitted(self, event: Input.Submitted) -> None:
        """Enter hands the keyboard back to the results."""
        event.stop()
        self._entry_list.focus()

    # -- actions -------------------------------------------------------------

    def set_query(self, query: str) -> None:
        """Apply ``query``; a search always spans the whole library."""
        self.term = query
        if query.strip() and self.category is not None:
            # Searching inside one category would hide matches elsewhere, so
            # the tree snaps back to the root rather than lying about scope.
            self.category = None
            self._categories.cursor_line = 0
        self.notice = ""
        self._refresh_list()

    def action_search(self) -> None:
        """``/`` — jump into the search box."""
        self._search_box.focus()

    def action_clear_or_back(self) -> None:
        """Esc clears an active search, then falls back to the app's Back."""
        if self.term:
            self._search_box.value = ""
            self.set_query("")
            self._entry_list.focus()
            return
        if self._search_box.has_focus:
            self._entry_list.focus()
            return
        cast("CircuitryApp", self.app).action_back_or_quit()

    def action_eject(self) -> None:
        """``e`` — write the highlighted entry's YAML into the current directory."""
        entry = self.selected_entry
        if entry is None:
            self._set_status("Nothing to eject.")
            return
        payload = eject_text(entry.raw)
        if payload is None:
            self._set_status(f"No curation file found for {entry.name}.")
            return
        dest = eject_destination(entry.raw)
        if dest.exists():

            def confirmed(answer: bool | None) -> None:
                self._finish_eject(payload, dest, bool(answer))

            self.app.push_screen(ConfirmOverwrite(dest), confirmed)
            return
        self._finish_eject(payload, dest, True)

    def _finish_eject(self, payload: str, dest: Path, confirmed: bool) -> None:
        if not confirmed:
            self._set_status(f"Kept {dest} — nothing written.")
            return
        write_ejected(payload, dest)
        self._set_status(f"Ejected → {dest} (cof run {dest})")
