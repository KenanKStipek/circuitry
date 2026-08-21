"""Library view — browse, search, refresh and eject every configured source.

Three panes: a category tree, the orchestrations in that category, and a
detail pane rendering the manifest metadata for whatever is highlighted.
``/`` searches every entry (name, intent, tags), ``s`` cycles the source
filter, ``r`` refreshes the fetchable sources, ``enter`` hands an entry to the
Run view and ``e`` ejects the YAML to the current directory.

Every fact shown here comes from :class:`circuitry.cli.library_sources.LibraryRegistry`
— the same reader behind ``cof list``, ``cof info``, ``cof run`` and
``cof library refresh``. Entries, provenance, refresh outcomes and the
"not fetched yet" notice are all the registry's answers, so the TUI cannot
drift from the CLI, and a new source type shows up here for free.

Zero-config (curation only) looks exactly as it did before sources existed:
badges, the source filter and the provenance block appear only when more than
one source is configured, which is the same rule ``cof list`` uses for its
Source column.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, ClassVar, cast

from rich.text import Text
from textual.app import ComposeResult
from textual.binding import Binding, BindingType
from textual.containers import Horizontal, Vertical, VerticalScroll
from textual.screen import ModalScreen
from textual.widget import Widget
from textual.widgets import Input, OptionList, Static, Tree

from ..cli.library_sources import (
    Entry,
    LibraryFetchError,
    LibraryRegistry,
    LibrarySourceError,
    RefreshResult,
    build_registry,
)
from ..cli.registry import eject_destination, eject_text, write_ejected
from .layout import ResponsiveLayout
from .screens import ViewScreen

if TYPE_CHECKING:  # pragma: no cover - typing only
    from collections.abc import Iterable, Sequence
    from pathlib import Path

    from textual.events import Mount

    from .app import CircuitryApp

__all__ = [
    "ConfirmOverwrite",
    "LibraryEntry",
    "LibraryScreen",
    "ambiguous_names",
    "banner_text",
    "categories",
    "detail_lines",
    "empty_state_lines",
    "entry_text",
    "library_registry",
    "load_entries",
    "option_label",
    "provenance_lines",
    "refresh_summary",
    "search",
]

#: Label of the tree root, which means "no category filter".
ALL_LABEL = "All"

#: Label of the "no source filter" step in the ``s`` cycle.
ALL_SOURCES_LABEL = "all"

#: Shown wherever the manifest has nothing to say for a field.
MISSING = "—"

#: Width of the label column in the detail pane.
LABEL_WIDTH = 12

#: Width of the label column inside the provenance block (which is indented).
PROVENANCE_LABEL_WIDTH = 11

#: Nice names for the keys a source's ``provenance()`` returns. Anything not
#: listed is title-cased, so a new source type renders without a change here.
PROVENANCE_LABELS: dict[str, str] = {
    "type": "Type",
    "path": "Path",
    "repo": "Repo",
    "ref": "Ref",
    "sha": "SHA",
    "fetched_at": "Fetched",
    "status": "Status",
    "token_env": "Token env",
}


# -- data ---------------------------------------------------------------------


@dataclass(frozen=True)
class LibraryEntry:
    """One library entry, in the shape this view needs.

    ``raw`` is the untouched index-shaped metadata, so anything the detail
    pane or an eject needs is available without going back to the source.
    ``path`` is the file the entry was read from — the same path ``cof eject``
    and ``cof run`` use, whichever source it came from.
    """

    name: str
    category: str
    file: str
    intent: str
    raw: dict[str, Any] = field(default_factory=dict, repr=False)
    source: str = ""
    path: Path | None = None

    @classmethod
    def from_index(
        cls,
        entry: dict[str, Any],
        *,
        source: str = "",
        path: Path | None = None,
    ) -> LibraryEntry:
        """Adapt an index-shaped metadata dict."""
        name = str(entry.get("name") or "")
        return cls(
            name=name,
            category=str(entry.get("category") or "") or (name.split("/")[0] if "/" in name else ""),
            file=str(entry.get("file") or ""),
            intent=str(entry.get("intent") or entry.get("description") or ""),
            raw=entry,
            source=source,
            path=path,
        )

    @classmethod
    def from_source(cls, entry: Entry) -> LibraryEntry:
        """Adapt a :class:`~circuitry.cli.library_sources.Entry`."""
        return cls.from_index(entry.metadata, source=entry.source, path=entry.path)

    @property
    def qualified_name(self) -> str:
        """``"<source>:<name>"`` — always unambiguous, as the CLI spells it."""
        return f"{self.source}:{self.name}" if self.source else self.name

    @property
    def search_text(self) -> str:
        """Everything ``/`` searches over: name, intent and tags, lowercased."""
        tags = _as_list(self.raw.get("tags"))
        return " ".join([self.name, self.intent, *tags]).lower()


def library_registry() -> tuple[LibraryRegistry, str]:
    """The configured registry, plus a warning when config could not be read.

    A broken ``runtime.library.sources`` must not cost the user their library:
    the browser falls back to the bundled curation source and says why, rather
    than crashing the view on open.
    """
    try:
        return build_registry(), ""
    except (LibrarySourceError, ValueError, OSError) as exc:
        return LibraryRegistry.default(), f"Library sources ignored — {exc}"


def load_entries(registry: LibraryRegistry | None = None) -> list[LibraryEntry]:
    """Every entry the registry exposes, in source-precedence order."""
    if registry is None:
        registry, _ = library_registry()
    return [LibraryEntry.from_source(entry) for entry in registry.list_entries()]


def categories(entries: Sequence[LibraryEntry]) -> list[tuple[str, int]]:
    """``(category, count)`` pairs in first-seen order."""
    counts: dict[str, int] = {}
    for entry in entries:
        key = entry.category or "uncategorised"
        counts[key] = counts.get(key, 0) + 1
    return list(counts.items())


def ambiguous_names(entries: Sequence[LibraryEntry]) -> set[str]:
    """Bare names that more than one source claims.

    These are exactly the names ``cof run`` would warn about, so the browser
    shows them source-qualified rather than listing the same label twice.
    """
    by_name: dict[str, set[str]] = {}
    for entry in entries:
        by_name.setdefault(entry.name, set()).add(entry.source)
    return {name for name, sources in by_name.items() if len(sources) > 1}


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
    # A source-qualified query means that one entry and nothing else.
    if entry.source and needle == entry.qualified_name.lower():
        return 0
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
    source: str | None = None,
) -> list[LibraryEntry]:
    """Entries matching ``query``, best match first, optionally scoped.

    A blank query keeps source-precedence order — browsing should not reshuffle
    under you — while a real query sorts by rank and then name for stability.
    """
    scoped = [
        e
        for e in entries
        if (source is None or e.source == source)
        and (category is None or (e.category or "uncategorised") == category)
    ]
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


def provenance_lines(source: str, provenance: dict[str, str]) -> list[str]:
    """The "where did these bytes come from" block for the detail pane.

    Rendered straight from whatever the source's ``provenance()`` returned, so
    a github source shows repo/ref/pinned SHA/fetched-at and a folder source
    shows its path without this function knowing either type exists.
    """
    if not source and not provenance:
        return []
    rows = [("Source", source)] if source else []
    for key, value in provenance.items():
        label = PROVENANCE_LABELS.get(key, key.replace("_", " ").capitalize())
        rows.append((label, _short_sha(value) if key == "sha" else value))
    return ["", "Provenance", *(f"  {label:<{PROVENANCE_LABEL_WIDTH}}{value}" for label, value in rows)]


def _short_sha(sha: str) -> str:
    """A pinned SHA, abbreviated the way git and ``cof library refresh`` do."""
    return f"{sha[:7]} (pinned)" if len(sha) > 7 else sha


def detail_lines(
    entry: LibraryEntry | None,
    provenance: dict[str, str] | None = None,
) -> list[str]:
    """The detail pane's text for ``entry`` — the manifest, rendered.

    Returns the empty-selection copy for ``None`` so the pane is never blank.
    ``provenance`` is omitted entirely for a single-source library, where
    there is only one possible answer and the block would be noise.
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
    if provenance is not None:
        lines += provenance_lines(entry.source, provenance)
    lines += ["", f"Press enter to run · e to eject to ./{eject_destination(raw)}"]
    return lines


def option_label(
    entry: LibraryEntry,
    *,
    show_source: bool = False,
    ambiguous: set[str] | frozenset[str] = frozenset(),
) -> str | Text:
    """One row in the list: a source badge, then the name.

    The name is source-qualified exactly when it is ambiguous, so two entries
    called ``summarize`` never render as the same string. Single-source
    libraries get the bare name they have always had.
    """
    name = entry.qualified_name if entry.name in ambiguous else entry.name
    if not show_source or not entry.source:
        return name
    # A Rich Text, not a markup string: a source is user-named, and "[hub]"
    # in a markup string would be parsed as a (broken) style tag.
    return Text.assemble((f"[{entry.source}]", "dim"), " ", name)


def empty_state_lines(
    query: str,
    *,
    source: str | None = None,
    refreshable: bool = False,
) -> list[str]:
    """Copy for the "nothing to show" panel — never an empty box.

    Four designed states: no search hit, an un-fetched remote source, an empty
    local source, and an empty library.
    """
    if query.strip():
        scoped = [f"Only source {source!r} is in scope — s widens it."] if source else []
        return [
            f'No orchestration matches "{query.strip()}".',
            "",
            *scoped,
            "Try fewer letters, or search an intent word or a tag.",
            "Esc clears the search and shows the whole library.",
        ]
    if source is not None and refreshable:
        return [
            f"Nothing cached for source {source!r} yet.",
            "",
            "Press r to fetch it — the only thing here that touches the network.",
            "s cycles sources · Esc goes back.",
        ]
    if source is not None:
        return [
            f"Source {source!r} has no orchestrations.",
            "",
            "s cycles to the next source · Esc goes back.",
        ]
    return [
        "The library is empty.",
        "",
        "Configure runtime.library.sources, or reinstall the curation library.",
    ]


def banner_text(notices: Sequence[str], stale: str = "") -> str:
    """The line above the panes: a failed refresh, else any source notices.

    A failure outranks a notice because it is the newer, more surprising fact
    — and it is what explains why the entries below it may be out of date.
    """
    if stale:
        return stale
    return "\n".join(notices)


def stale_banner(failures: Sequence[str]) -> str:
    """Failure copy that keeps the cached entries' status honest."""
    if not failures:
        return ""
    return "\n".join(["Refresh failed — showing the last cached entries.", *failures])


def refresh_summary(outcomes: Sequence[RefreshResult], failures: Sequence[str]) -> str:
    """One status line for a finished refresh, in ``cof library refresh`` words."""
    parts = [outcome.summary() for outcome in outcomes]
    if failures:
        parts.append(f"{len(failures)} failed")
    return " · ".join(parts) if parts else "Nothing to refresh."


def entry_text(entry: LibraryEntry) -> str | None:
    """The bytes an eject writes for ``entry``; ``None`` if unresolvable.

    The source's own file, read exactly as ``cof eject`` reads it, so an eject
    is the same operation whichever source the entry came from. The curation
    reader is the fallback for a manifest entry with no resolved path.
    """
    if entry.path is not None and entry.path.exists():
        return entry.path.read_text(encoding="utf-8")
    return eject_text(entry.raw)


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
    """Browse, search, refresh and eject every configured library source."""

    #: The panes scroll individually, so the body itself must not scroll.
    BODY_CONTAINER: ClassVar[type[Widget]] = Vertical

    BINDINGS: ClassVar[list[BindingType]] = [
        Binding("slash", "search", "Search"),
        # Priority: the results list binds Enter to "select" and would
        # otherwise swallow it, leaving Run advertised in the footer only
        # while there is nothing to run.
        Binding("enter", "run_entry", "Run", priority=True),
        Binding("e", "eject", "Eject"),
        Binding("s", "cycle_source", "Source"),
        Binding("r", "refresh_sources", "Refresh"),
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

    LibraryScreen #library-banner {
        height: auto;
        max-height: 3;
        padding: 0 1;
        color: $warning;
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
    LibraryScreen.-tiny #library-banner,
    LibraryScreen.-tiny #library-detail-pane,
    LibraryScreen.-tiny #library-status {
        display: none;
    }
    """

    def __init__(self, spec: Any, *, registry: LibraryRegistry | None = None) -> None:
        super().__init__(spec)
        warning = ""
        if registry is None:
            registry, warning = library_registry()
        self.registry = registry
        self.entries: list[LibraryEntry] = load_entries(self.registry)
        self.matches: list[LibraryEntry] = list(self.entries)
        self.category: str | None = None
        self.source: str | None = None
        self.term = ""
        self.notice = warning
        self.stale = ""
        self.refreshing = False
        self._notices: list[tuple[str, str]] = []
        self._reload_notices()

    # -- composition ---------------------------------------------------------

    def compose_body(self) -> ComposeResult:
        # Built by construction rather than with ``with`` blocks: the base
        # screen unpacks this generator into its body container, which the
        # compose-stack the context managers rely on does not survive.
        yield Static(f"{self.spec.name} — {self.spec.blurb}", id="library-heading")
        yield Input(placeholder="/ to search — name, intent, tags", id="library-search")
        yield Static("", id="library-banner", markup=False)
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

    @property
    def show_source(self) -> bool:
        """Whether source badges, filter and provenance are worth the pixels."""
        return self.registry.is_multi_source

    # -- population ----------------------------------------------------------

    def _source_entries(self) -> list[LibraryEntry]:
        """Entries within the current source filter, ignoring category/query."""
        if self.source is None:
            return list(self.entries)
        return [entry for entry in self.entries if entry.source == self.source]

    def _refresh_targets(self) -> list[str]:
        """Sources ``r`` would fetch: the filtered one, or every fetchable one."""
        names = self.registry.refreshable_names
        if self.source is None:
            return names
        return [self.source] if self.source in names else []

    def _build_tree(self) -> None:
        tree = self._categories
        scoped = self._source_entries()
        tree.clear()
        tree.show_root = True
        tree.root.data = ""
        tree.root.label = f"{ALL_LABEL} ({len(scoped)})"
        for name, count in categories(scoped):
            tree.root.add_leaf(f"{name} ({count})", data=name)
        tree.root.expand()

    def _refresh_list(self) -> None:
        """Re-run the filter and repaint the list, detail pane and status."""
        self.matches = search(self.entries, self.term, self.category, self.source)
        ambiguous: set[str] = ambiguous_names(self.entries) if self.show_source else set()
        options = self._entry_list
        options.clear_options()
        if self.matches:
            options.add_options(
                [
                    option_label(entry, show_source=self.show_source, ambiguous=ambiguous)
                    for entry in self.matches
                ]
            )
            options.highlighted = 0
        options.display = bool(self.matches)
        empty = self.query_one("#library-empty", Static)
        empty.display = not self.matches
        empty.update(
            "\n".join(
                empty_state_lines(
                    self.term,
                    source=self.source,
                    refreshable=bool(self._refresh_targets()),
                )
            )
        )
        self._refresh_detail()
        self._refresh_banner()
        self._refresh_status()

    def _refresh_detail(self) -> None:
        detail = self.query_one("#library-detail", Static)
        entry = self.selected_entry
        provenance = (
            self.registry.provenance(entry.source) if (self.show_source and entry) else None
        )
        detail.update("\n".join(detail_lines(entry, provenance)))

    def _reload_notices(self) -> None:
        """Re-read the sources' notices, keyed by source.

        Cached rather than asked for on every repaint: a notice is answered
        from a source's cache index (a file read), and the banner is repainted
        on every keystroke. Only a refresh can change the answer.
        """
        self._notices = [
            (name, message)
            for name in self.registry.source_names
            for message in self.registry.notices(source=name)
        ]

    def _refresh_banner(self) -> None:
        banner = self.query_one("#library-banner", Static)
        notices = [
            message for name, message in self._notices if self.source in (None, name)
        ]
        text = banner_text(notices, self.stale)
        banner.display = bool(text)
        banner.update(text)

    def _refresh_status(self) -> None:
        scope = self.category or "all"
        counted = f"{len(self.matches)}/{len(self._source_entries())} in {scope}"
        if self.show_source:
            counted += f" · source {self.source or ALL_SOURCES_LABEL}"
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
        """Enter on a row runs it — the same hand-off from every source."""
        event.stop()
        self.action_run_entry()

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

    def action_cycle_source(self) -> None:
        """``s`` — step the source filter: all → first → … → all.

        Always live rather than greyed out: with one source it answers with
        what that source is, which is more use than a dead key.
        """
        if not self.registry.is_multi_source:
            self._set_status(f"One source configured: {', '.join(self.registry.source_names)}.")
            return
        order: list[str | None] = [None, *self.registry.source_names]
        index = order.index(self.source) if self.source in order else 0
        self.source = order[(index + 1) % len(order)]
        self.category = None
        self._build_tree()
        self._refresh_list()
        self._set_status(f"Source: {self.source or ALL_SOURCES_LABEL}")

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

    def action_run_entry(self) -> None:
        """``enter`` — hand the highlighted entry to the Run view.

        Uniform across sources: what travels is the resolved file path, which
        is what ``cof run`` would resolve the same name to. In the search box
        Enter keeps its older meaning — take me to the results — because
        committing a search and launching a run are different intentions.
        """
        if self._search_box.has_focus:
            self._entry_list.focus()
            return
        entry = self.selected_entry
        if entry is None:
            self._set_status("Nothing to run.")
            return
        if entry.path is None or not entry.path.exists():
            self._set_status(f"No orchestration file for {entry.qualified_name}.")
            return
        launch = getattr(self.app, "launch_run", None)
        if not callable(launch):  # pragma: no cover - only a bare App lacks it
            self._set_status("Run view unavailable.")
            return
        launch(entry.path)

    # -- refresh -------------------------------------------------------------

    def action_refresh_sources(self) -> None:
        """``r`` — fetch the fetchable sources on a worker thread.

        The UI thread only ever paints: the fetch itself runs off-thread and
        reports back through :meth:`_refresh_finished`, so the list stays
        scrollable and searchable while the network is slow or hanging.
        """
        if self.refreshing:
            self._set_status("Refresh already in flight…")
            return
        targets = self._refresh_targets()
        if not targets:
            self._set_status("Nothing to fetch — every source in scope is local.")
            return
        self.refreshing = True
        self.stale = ""
        self._refresh_banner()
        self._set_status(f"Refreshing {', '.join(targets)}…")
        self.run_worker(
            lambda: self._refresh_worker(tuple(targets)),
            thread=True,
            name="library-refresh",
            group="library-refresh",
        )

    def _refresh_worker(self, targets: tuple[str, ...]) -> None:
        """Worker-thread body: fetch each target, then post the result back."""
        outcomes: list[RefreshResult] = []
        failures: list[str] = []
        for name in targets:
            try:
                outcomes.extend(self.registry.refresh(source=name))
            except LibraryFetchError as exc:
                failures.append(str(exc))
            except Exception as exc:
                failures.append(f"{name}: {exc}")
        self.app.call_from_thread(self._refresh_finished, outcomes, failures)

    def _refresh_finished(
        self, outcomes: Iterable[RefreshResult], failures: Sequence[str]
    ) -> None:
        """Back on the UI thread: re-read the caches and repaint.

        Re-listing after a failure is deliberate — every source reads from its
        own cache, so a failed fetch leaves the previously fetched entries
        exactly where they were, under a banner that says they may be stale.
        """
        self.refreshing = False
        self.entries = load_entries(self.registry)
        self._reload_notices()
        self.stale = stale_banner(list(failures))
        # A fetch can add or remove categories, so the tree is rebuilt and its
        # cursor lands back on the root; the filter follows it rather than
        # holding a category that may no longer exist.
        self.category = None
        self._build_tree()
        self._refresh_list()
        self._set_status(refresh_summary(list(outcomes), list(failures)))

    # -- eject ---------------------------------------------------------------

    def action_eject(self) -> None:
        """``e`` — write the highlighted entry's YAML into the current directory."""
        entry = self.selected_entry
        if entry is None:
            self._set_status("Nothing to eject.")
            return
        payload = entry_text(entry)
        if payload is None:
            self._set_status(f"No orchestration file found for {entry.qualified_name}.")
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
