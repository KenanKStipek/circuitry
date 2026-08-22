"""The library browser over a multi-source registry.

The registry under test carries all three source types: the real bundled
curation source, a real folder source over a tmp directory, and a github
source faked *at the source seam* — a stand-in that implements the same
protocol (``list_entries`` / ``resolve`` / ``refresh`` / ``notice`` /
``provenance``) and materialises its "download" onto disk, so everything
above it — the cache-shaped read, the SHA-pinned provenance, the un-fetched
notice, the failure path — is the real code with no sockets involved.

The pure helpers are tested directly; the pilot tests cover the wiring —
that ``s`` really re-filters, that ``r`` really runs off the UI thread, and
that a failed fetch leaves the cached entries on screen.
"""

from __future__ import annotations

import threading
from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("textual")

from textual.pilot import Pilot
from textual.widgets import OptionList, Select, Static

from circuitry.cli.library_sources import (
    CurationSource,
    Entry,
    FolderSource,
    LibraryFetchError,
    LibraryRegistry,
    RefreshResult,
)
from circuitry.tui import library as library_module
from circuitry.tui.library import (
    LibraryEntry,
    LibraryScreen,
    ambiguous_names,
    banner_text,
    empty_state_lines,
    entry_text,
    option_label,
    provenance_lines,
    refresh_summary,
    search,
    stale_banner,
)

# ── fixture content ──────────────────────────────────────────────────────────

HUB_SHA = "abc1234def5678901234567890123456789abcde"
HUB_FETCHED_AT = "2026-08-13T00:00:00+00:00"

LOCAL_PIPELINE = """\
# A local pipeline that lives next to the user.

effects:
  - type: prompt
    name: summarise
    template: "Summarise {{document}}."
"""

DUPE_LOCAL = """\
# The local copy of an ambiguous name.

effects:
  - type: prompt
    name: go
    template: "local"
"""

HUB_PIPELINE = """\
# A hub orchestration, published by PR.

effects:
  - type: prompt
    name: review
    template: "Review {{diff}}."
"""

DUPE_HUB = """\
# The hub copy of an ambiguous name.

effects:
  - type: prompt
    name: go
    template: "hub"
"""


class FakeHub:
    """A github source with the network cut out, and nothing else changed.

    ``refresh()`` writes the "downloaded" files into a SHA-named cache
    directory and flips the pin, exactly as :class:`GitHubSource` does; every
    read below it is a real :class:`FolderSource` over that directory.
    """

    #: The seam the browser reads to decide whether ``r`` has work to do.
    REFRESHABLE = True

    def __init__(self, cache_root: Path, *, name: str = "hub") -> None:
        self.name = name
        self.repo = "owner/hub"
        self.ref = "main"
        self.cache_root = cache_root
        self.sha: str | None = None
        self.fetched_at = ""
        #: Set to a message to make the next fetch fail.
        self.fail: str | None = None
        #: Set to hold the fetch open, so a test can drive the UI mid-refresh.
        self.gate: threading.Event | None = None
        self.calls = 0
        self.files = {"hub_review.yml": HUB_PIPELINE, "dupe.yml": DUPE_HUB}

    # -- LibrarySource protocol ---------------------------------------------

    def _folder(self) -> FolderSource | None:
        if self.sha is None:
            return None
        return FolderSource(self.name, self.cache_root / self.sha)

    def list_entries(self) -> list[Entry]:
        folder = self._folder()
        return folder.list_entries() if folder is not None else []

    def resolve(self, ref: str) -> Path | None:
        folder = self._folder()
        return folder.resolve(ref) if folder is not None else None

    def notice(self) -> str | None:
        if self.sha is not None:
            return None
        return (
            f"Library source {self.name!r} ({self.repo}@{self.ref}) has not been "
            f"fetched yet — run `cof library refresh {self.name}`."
        )

    def provenance(self) -> dict[str, str]:
        details = {"type": "github", "repo": self.repo, "ref": self.ref}
        if self.sha is None:
            details["status"] = "not fetched"
        else:
            details["sha"] = self.sha
            details["fetched_at"] = self.fetched_at
        return details

    def refresh(self) -> RefreshResult:
        self.calls += 1
        if self.gate is not None:
            self.gate.wait(timeout=10)
        if self.fail is not None:
            raise LibraryFetchError(f"Library source {self.name!r}: {self.fail}")
        directory = self.cache_root / HUB_SHA
        directory.mkdir(parents=True, exist_ok=True)
        for rel, text in self.files.items():
            (directory / rel).write_text(text, encoding="utf-8")
        self.sha = HUB_SHA
        self.fetched_at = HUB_FETCHED_AT
        return RefreshResult(
            source=self.name,
            status="updated",
            sha=HUB_SHA,
            detail=f"{len(self.files)} file(s) from {self.repo}@{self.ref}",
        )


@pytest.fixture
def sources(tmp_path: Path) -> tuple[LibraryRegistry, FakeHub]:
    """A registry with all three source types, hub un-fetched."""
    folder = tmp_path / "orchestrations"
    folder.mkdir()
    (folder / "local_pipeline.yml").write_text(LOCAL_PIPELINE, encoding="utf-8")
    (folder / "dupe.yml").write_text(DUPE_LOCAL, encoding="utf-8")
    hub = FakeHub(tmp_path / "cache")
    registry = LibraryRegistry([CurationSource(), FolderSource("local", folder), hub])
    return registry, hub


@pytest.fixture
def multi(
    sources: tuple[LibraryRegistry, FakeHub], monkeypatch: pytest.MonkeyPatch
) -> tuple[LibraryRegistry, FakeHub]:
    """The multi-source registry, wired in as the one the browser builds."""
    registry, hub = sources
    monkeypatch.setattr(library_module, "library_registry", lambda: (registry, ""))
    return registry, hub


# ── helpers ──────────────────────────────────────────────────────────────────


async def open_library(pilot: Pilot[Any]) -> LibraryScreen:
    await pilot.press("1")
    await pilot.pause()
    screen = pilot.app.screen
    assert isinstance(screen, LibraryScreen)
    return screen


async def press_until_source(pilot: Pilot[Any], screen: LibraryScreen, source: str) -> None:
    """Cycle ``s`` until the filter lands on ``source`` (keyboard only)."""
    for _ in range(len(screen.registry.source_names) + 2):
        if screen.source == source:
            return
        await pilot.press("s")
        await pilot.pause()
    raise AssertionError(f"never reached source {source!r}")


async def refresh(pilot: Pilot[Any]) -> None:
    """Press ``r`` and wait for the worker to land."""
    await pilot.press("r")
    await pilot.pause()
    await pilot.app.workers.wait_for_complete()
    await pilot.pause()


def widget_text(screen: LibraryScreen, selector: str) -> str:
    return str(screen.query_one(selector, Static).content)


def option_texts(screen: LibraryScreen) -> list[str]:
    options = screen.query_one("#library-list", OptionList)
    return [str(options.get_option_at_index(i).prompt) for i in range(options.option_count)]


# ── pure helpers ─────────────────────────────────────────────────────────────


def test_entries_carry_their_source_and_path(sources: Any) -> None:
    registry, _ = sources
    entries = [LibraryEntry.from_source(e) for e in registry.list_entries()]
    assert {e.source for e in entries} == {"curation", "local"}  # hub not fetched
    local = next(e for e in entries if e.source == "local" and e.name == "local_pipeline")
    assert local.path is not None and local.path.exists()
    assert local.qualified_name == "local:local_pipeline"


def test_ambiguous_names_are_the_ones_more_than_one_source_claims() -> None:
    entries = [
        LibraryEntry(name="dupe", category="", file="dupe.yml", intent="", source="local"),
        LibraryEntry(name="dupe", category="", file="dupe.yml", intent="", source="hub"),
        LibraryEntry(name="solo", category="", file="solo.yml", intent="", source="hub"),
    ]
    assert ambiguous_names(entries) == {"dupe"}


def test_the_badge_is_only_drawn_for_a_multi_source_library() -> None:
    entry = LibraryEntry(name="one", category="", file="one.yml", intent="", source="hub")
    assert option_label(entry) == "one"
    assert str(option_label(entry, show_source=True)) == "[hub] one"


def test_an_ambiguous_row_is_shown_source_qualified() -> None:
    entry = LibraryEntry(name="dupe", category="", file="dupe.yml", intent="", source="hub")
    label = str(option_label(entry, show_source=True, ambiguous={"dupe"}))
    assert label == "[hub] hub:dupe"


def test_a_source_qualified_query_finds_exactly_that_entry() -> None:
    entries = [
        LibraryEntry(name="dupe", category="", file="d.yml", intent="", source="local"),
        LibraryEntry(name="dupe", category="", file="d.yml", intent="", source="hub"),
    ]
    assert [e.source for e in search(entries, "hub:dupe")] == ["hub"]


def test_search_can_be_scoped_to_one_source(sources: Any) -> None:
    registry, _ = sources
    entries = [LibraryEntry.from_source(e) for e in registry.list_entries()]
    assert {e.source for e in search(entries, "", None, "local")} == {"local"}


def test_provenance_renders_the_pinned_sha_short() -> None:
    lines = provenance_lines(
        "hub",
        {"type": "github", "repo": "owner/hub", "ref": "main", "sha": HUB_SHA},
    )
    text = "\n".join(lines)
    assert "Provenance" in text
    assert "Source" in text and "hub" in text
    assert "owner/hub" in text
    assert f"{HUB_SHA[:7]} (pinned)" in text
    assert HUB_SHA not in text  # the full 40 characters would blow the pane


def test_provenance_of_an_unknown_key_still_renders() -> None:
    assert "Some key" in "\n".join(provenance_lines("x", {"some_key": "value"}))


def test_the_unfetched_empty_state_names_the_key_that_fixes_it() -> None:
    lines = empty_state_lines("", source="hub", refreshable=True)
    assert "hub" in lines[0]
    assert any("Press r" in line for line in lines)


def test_an_empty_local_source_does_not_offer_a_pointless_refresh() -> None:
    lines = empty_state_lines("", source="local", refreshable=False)
    assert "local" in lines[0]
    assert not any("Press r" in line for line in lines)


def test_a_search_miss_still_wins_over_the_source_copy_but_names_the_scope() -> None:
    lines = empty_state_lines("zzz", source="hub", refreshable=True)
    assert '"zzz"' in lines[0]
    assert any("Only source 'hub' is in scope" in line for line in lines)
    assert not any("Only source" in line for line in empty_state_lines("zzz"))


def test_a_failure_outranks_a_notice_in_the_banner() -> None:
    assert banner_text(["a notice"], stale_banner(["boom"])).startswith("Refresh failed")
    assert banner_text(["a notice"]) == "a notice"


def test_refresh_summary_counts_failures() -> None:
    outcome = RefreshResult(source="hub", status="updated", sha=HUB_SHA, detail="2 file(s)")
    assert "hub: updated" in refresh_summary([outcome], [])
    assert "1 failed" in refresh_summary([outcome], ["nope"])
    assert refresh_summary([], []) == "Nothing to refresh."


def test_entry_text_reads_the_sources_own_file(sources: Any) -> None:
    registry, _ = sources
    entry = next(
        LibraryEntry.from_source(e) for e in registry.list_entries() if e.source == "local"
    )
    assert entry_text(entry) == entry.path.read_text(encoding="utf-8")  # type: ignore[union-attr]


# ── browsing across sources ──────────────────────────────────────────────────


def test_the_browser_lists_every_source_with_a_badge(run_app: Any, multi: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[list[str], list[str]]:
        screen = await open_library(pilot)
        return [e.source for e in screen.matches], option_texts(screen)

    seen, labels = run_app(scenario)
    assert set(seen) == {"curation", "local"}
    assert any(label.startswith("[curation]") for label in labels)
    assert any(label.startswith("[local]") for label in labels)


def test_s_cycles_the_source_filter_and_re_scopes_everything(run_app: Any, multi: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> list[tuple[str | None, set[str]]]:
        screen = await open_library(pilot)
        seen = []
        for _ in range(4):
            await pilot.press("s")
            await pilot.pause()
            seen.append((screen.source, {e.source for e in screen.matches}))
        return seen

    steps = run_app(scenario)
    assert [source for source, _ in steps] == ["curation", "local", "hub", None]
    assert steps[0][1] == {"curation"}
    assert steps[1][1] == {"local"}
    assert steps[2][1] == set()  # hub is not fetched yet
    assert steps[3][1] == {"curation", "local"}


def test_the_status_line_names_the_source_scope(run_app: Any, multi: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await open_library(pilot)
        await press_until_source(pilot, screen, "local")
        return widget_text(screen, "#library-status")

    assert "source local" in run_app(scenario)


def test_searching_inside_a_source_filter_keeps_the_scope_and_says_so(
    run_app: Any, multi: Any
) -> None:
    """The source filter is a deliberate mode, so it says what it is hiding."""

    async def scenario(pilot: Pilot[Any]) -> tuple[str | None, int, str]:
        screen = await open_library(pilot)
        await press_until_source(pilot, screen, "local")
        await pilot.press("slash")
        await pilot.pause()
        for char in "zzzz":
            await pilot.press(char)
        await pilot.pause()
        return screen.source, len(screen.matches), widget_text(screen, "#library-empty")

    source, count, empty = run_app(scenario)
    assert source == "local"
    assert count == 0
    assert "Only source 'local' is in scope" in empty


def test_an_unfetched_github_source_renders_the_refresh_to_fetch_state(
    run_app: Any, multi: Any
) -> None:
    """AC: the empty un-fetched source is a designed state, not a blank box."""

    async def scenario(pilot: Pilot[Any]) -> tuple[str, str, bool]:
        screen = await open_library(pilot)
        await press_until_source(pilot, screen, "hub")
        options = screen.query_one("#library-list", OptionList)
        return (
            widget_text(screen, "#library-empty"),
            widget_text(screen, "#library-banner"),
            bool(options.display),
        )

    empty, banner, list_shown = run_app(scenario)
    assert list_shown is False
    assert "hub" in empty and "Press r to fetch" in empty
    assert "has not been fetched yet" in banner


def test_the_ambiguous_name_is_listed_once_per_source_and_qualified(
    run_app: Any, multi: Any
) -> None:
    async def scenario(pilot: Pilot[Any]) -> list[str]:
        screen = await open_library(pilot)
        await refresh(pilot)
        return option_texts(screen)

    labels = run_app(scenario)
    assert "[local] local:dupe" in labels
    assert "[hub] hub:dupe" in labels
    assert "[hub] hub_review" in labels  # unambiguous names stay bare


# ── refresh ──────────────────────────────────────────────────────────────────


def test_r_fetches_the_hub_and_the_new_entries_appear(run_app: Any, multi: Any) -> None:
    _, hub = multi

    async def scenario(pilot: Pilot[Any]) -> tuple[list[str], str]:
        screen = await open_library(pilot)
        await refresh(pilot)
        return (
            [e.name for e in screen.entries if e.source == "hub"],
            widget_text(screen, "#library-status"),
        )

    names, status = run_app(scenario)
    assert sorted(names) == ["dupe", "hub_review"]
    assert "hub: updated" in status
    assert hub.calls == 1


def test_refreshing_inside_a_source_filter_fetches_only_that_source(
    run_app: Any, multi: Any
) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[list[str], str]:
        screen = await open_library(pilot)
        await press_until_source(pilot, screen, "curation")
        targets = screen._refresh_targets()
        await pilot.press("r")
        await pilot.pause()
        return targets, widget_text(screen, "#library-status")

    targets, status = run_app(scenario)
    assert targets == []
    assert "Nothing to fetch" in status


def test_refresh_never_blocks_input(run_app: Any, multi: Any) -> None:
    """AC: the view stays usable while a fetch is in flight."""
    _, hub = multi
    hub.gate = threading.Event()

    async def scenario(pilot: Pilot[Any]) -> tuple[str, bool, str, str]:
        screen = await open_library(pilot)
        await pilot.press("r")
        await pilot.pause()
        in_flight = widget_text(screen, "#library-status")
        refreshing = screen.refreshing
        # The UI thread is free: search still filters while the fetch hangs.
        await pilot.press("slash")
        await pilot.pause()
        for char in "hello":
            await pilot.press(char)
        await pilot.pause()
        typed = screen.term
        hub.gate.set()
        await pilot.app.workers.wait_for_complete()
        await pilot.pause()
        return in_flight, refreshing, typed, widget_text(screen, "#library-status")

    in_flight, refreshing, typed, after = run_app(scenario)
    assert "Refreshing hub" in in_flight
    assert refreshing is True
    assert typed == "hello"
    assert "hub: updated" in after


def test_a_second_r_while_one_is_in_flight_says_so(run_app: Any, multi: Any) -> None:
    _, hub = multi
    hub.gate = threading.Event()

    async def scenario(pilot: Pilot[Any]) -> tuple[str, int]:
        screen = await open_library(pilot)
        await pilot.press("r")
        await pilot.pause()
        await pilot.press("r")
        await pilot.pause()
        status = widget_text(screen, "#library-status")
        hub.gate.set()
        await pilot.app.workers.wait_for_complete()
        await pilot.pause()
        return status, hub.calls

    status, calls = run_app(scenario)
    assert "already in flight" in status
    assert calls == 1


def test_a_failed_refresh_keeps_the_cached_entries_and_says_they_are_stale(
    run_app: Any, multi: Any
) -> None:
    """AC: failure is a designed state — cached entries stay, banner explains."""
    _, hub = multi

    async def scenario(pilot: Pilot[Any]) -> tuple[list[str], str, str]:
        screen = await open_library(pilot)
        await refresh(pilot)  # populate the cache
        hub.fail = "could not reach api.github.com (timed out)"
        await refresh(pilot)  # and now break it
        return (
            [e.name for e in screen.entries if e.source == "hub"],
            widget_text(screen, "#library-banner"),
            widget_text(screen, "#library-status"),
        )

    names, banner, status = run_app(scenario)
    assert sorted(names) == ["dupe", "hub_review"]  # cache still serves them
    assert "Refresh failed" in banner
    assert "timed out" in banner
    assert "1 failed" in status


def test_a_refresh_that_only_fails_leaves_the_empty_state_intact(
    run_app: Any, multi: Any
) -> None:
    _, hub = multi
    hub.fail = "404 — check 'repo', 'ref', and 'path'"

    async def scenario(pilot: Pilot[Any]) -> tuple[str, str, int]:
        screen = await open_library(pilot)
        await press_until_source(pilot, screen, "hub")
        await refresh(pilot)
        return (
            widget_text(screen, "#library-banner"),
            widget_text(screen, "#library-empty"),
            len(screen.matches),
        )

    banner, empty, count = run_app(scenario)
    assert "Refresh failed" in banner and "404" in banner
    assert "Press r to fetch" in empty
    assert count == 0


# ── provenance, run and eject ────────────────────────────────────────────────


def test_the_detail_pane_shows_github_provenance(run_app: Any, multi: Any) -> None:
    """AC: source, repo, ref, pinned SHA and fetched-at, from the cache index."""

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await open_library(pilot)
        await refresh(pilot)
        await press_until_source(pilot, screen, "hub")
        return widget_text(screen, "#library-detail")

    detail = run_app(scenario)
    assert "Provenance" in detail
    assert "hub" in detail
    assert "owner/hub" in detail
    assert "main" in detail
    assert f"{HUB_SHA[:7]} (pinned)" in detail
    assert HUB_FETCHED_AT in detail


def test_the_detail_pane_shows_a_folder_sources_path(run_app: Any, multi: Any) -> None:
    registry, _ = multi
    folder = registry.get_source("local")

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await open_library(pilot)
        await press_until_source(pilot, screen, "local")
        return widget_text(screen, "#library-detail")

    detail = run_app(scenario)
    assert "folder" in detail
    assert str(folder.path) in detail  # type: ignore[union-attr]


def test_enter_hands_the_entry_to_the_run_view_from_any_source(
    run_app: Any, multi: Any
) -> None:
    """AC: run is uniform — what travels is the resolved path, per source."""

    async def scenario(pilot: Pilot[Any]) -> tuple[Any, str, str]:
        screen = await open_library(pilot)
        await refresh(pilot)
        await press_until_source(pilot, screen, "hub")
        entry = screen.selected_entry
        assert entry is not None
        await pilot.press("enter")
        await pilot.pause()
        view = pilot.app.current_view()
        # The Run view consumes ``pending_run`` on mount, so what proves the
        # hand-off is the orchestration it opened on, not the cleared slot.
        run_screen = pilot.app.base_screen()
        choice = run_screen._choice_for(
            run_screen.query_one("#run-orchestration", Select).value
        )
        return choice.path if choice else None, entry.source, view.slug if view else ""

    opened, source, slug = run_app(scenario)
    assert source == "hub"
    assert slug == "run"
    assert opened is not None and opened.name in ("dupe.yml", "hub_review.yml")


def test_ejecting_a_github_entry_writes_the_cached_bytes(
    run_app: Any, multi: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC: eject is uniform — the source's own file, whichever source it is."""
    out = tmp_path / "out"
    out.mkdir()
    monkeypatch.chdir(out)

    async def scenario(pilot: Pilot[Any]) -> tuple[str, str]:
        screen = await open_library(pilot)
        await refresh(pilot)
        await press_until_source(pilot, screen, "hub")
        # dupe.yml sorts first in the cached folder.
        await pilot.press("e")
        await pilot.pause()
        entry = screen.selected_entry
        assert entry is not None
        return entry.name, widget_text(screen, "#library-status")

    name, status = run_app(scenario)
    assert "Ejected" in status
    assert (out / f"{name}.yml").read_text(encoding="utf-8") in (DUPE_HUB, HUB_PIPELINE)


def test_ejecting_a_folder_entry_writes_the_folder_bytes(
    run_app: Any, multi: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    out = tmp_path / "out-local"
    out.mkdir()
    monkeypatch.chdir(out)

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await open_library(pilot)
        await press_until_source(pilot, screen, "local")
        await pilot.press("e")
        await pilot.pause()
        return widget_text(screen, "#library-status")

    assert "Ejected" in run_app(scenario)
    assert (out / "dupe.yml").read_text(encoding="utf-8") == DUPE_LOCAL


# ── the single-source library keeps its old shape ────────────────────────────


def test_a_single_source_library_has_no_badges_and_no_provenance(run_app: Any) -> None:
    """Zero-config output is unchanged — the rule `cof list` already uses."""

    async def scenario(pilot: Pilot[Any]) -> tuple[list[str], str, bool]:
        screen = await open_library(pilot)
        return (
            option_texts(screen)[:1],
            widget_text(screen, "#library-detail"),
            bool(screen.query_one("#library-banner", Static).display),
        )

    labels, detail, banner_shown = run_app(scenario)
    assert not labels[0].startswith("[")
    assert "Provenance" not in detail
    assert banner_shown is False


def test_s_and_r_answer_instead_of_going_dead_on_a_single_source(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[str, str]:
        screen = await open_library(pilot)
        await pilot.press("s")
        await pilot.pause()
        cycled = widget_text(screen, "#library-status")
        await pilot.press("r")
        await pilot.pause()
        return cycled, widget_text(screen, "#library-status")

    cycled, refreshed = run_app(scenario)
    assert "One source configured: curation" in cycled
    assert "Nothing to fetch" in refreshed


# ── designed states, recorded ────────────────────────────────────────────────


def test_snapshot_of_a_fetched_github_source(
    run_app: Any, multi: Any, capture_frame: Any, snapshot: Any
) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await open_library(pilot)
        await refresh(pilot)
        await press_until_source(pilot, screen, "hub")
        return str(capture_frame(pilot.app))

    snapshot.assert_match(run_app(scenario), "library-hub-80x24")


def test_snapshot_of_an_unfetched_github_source(
    run_app: Any, multi: Any, capture_frame: Any, snapshot: Any
) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await open_library(pilot)
        await press_until_source(pilot, screen, "hub")
        return str(capture_frame(pilot.app))

    snapshot.assert_match(run_app(scenario), "library-unfetched-80x24")


def test_snapshot_of_a_failed_refresh(
    run_app: Any, multi: Any, capture_frame: Any, snapshot: Any
) -> None:
    _, hub = multi

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await open_library(pilot)
        await refresh(pilot)
        hub.fail = "could not reach api.github.com (timed out)"
        await press_until_source(pilot, screen, "hub")
        await refresh(pilot)
        return str(capture_frame(pilot.app))

    snapshot.assert_match(run_app(scenario), "library-stale-80x24")


def test_a_broken_sources_config_falls_back_to_curation_and_says_so(
    monkeypatch: pytest.MonkeyPatch, run_app: Any
) -> None:
    from circuitry.cli.library_sources import LibrarySourceError

    def explode() -> Any:
        raise LibrarySourceError("runtime.library.sources must be a non-empty list")

    monkeypatch.setattr(library_module, "build_registry", explode)

    async def scenario(pilot: Pilot[Any]) -> tuple[list[str], str]:
        screen = await open_library(pilot)
        return screen.registry.source_names, widget_text(screen, "#library-status")

    names, status = run_app(scenario)
    assert names == ["curation"]
    assert "Library sources ignored" in status
