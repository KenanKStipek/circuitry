"""Library view: browse the curation tree, search it, read it, eject from it.

The pure filter/render helpers are tested directly (they are where the logic
is); the pilot tests cover the wiring — that a keypress reaches the widget,
that the widget's state reaches the screen, and that ``e`` puts bytes on
disk identical to the ones ``cof eject`` writes.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("textual")

from textual.pilot import Pilot
from textual.widgets import Input, OptionList, Static, Tree
from typer.testing import CliRunner

from circuitry.cli.app import app as cli_app
from circuitry.cli.registry import eject_text, load_index
from circuitry.tui.library import (
    ConfirmOverwrite,
    LibraryScreen,
    categories,
    detail_lines,
    empty_state_lines,
    load_entries,
    match_rank,
    search,
)

MANIFEST_NAMES = [str(entry["name"]) for entry in load_index()]


# ── helpers ──────────────────────────────────────────────────────────────────


async def open_library(pilot: Pilot[Any]) -> LibraryScreen:
    """Press the library hotkey and hand back the mounted screen."""
    await pilot.press("1")
    await pilot.pause()
    screen = pilot.app.screen
    assert isinstance(screen, LibraryScreen)
    return screen


async def type_search(pilot: Pilot[Any], text: str) -> None:
    """Open the search box with ``/`` and type into it."""
    await pilot.press("slash")
    await pilot.pause()
    for char in text:
        await pilot.press(char)
    await pilot.pause()


def widget_text(screen: LibraryScreen, selector: str) -> str:
    """The plain text a Static in this screen is currently showing."""
    return str(screen.query_one(selector, Static).content)


# ── the filter, on its own ───────────────────────────────────────────────────


def test_load_entries_covers_the_whole_manifest() -> None:
    assert [entry.name for entry in load_entries()] == MANIFEST_NAMES


def test_categories_are_counted_in_manifest_order() -> None:
    counted = categories(load_entries())
    assert [name for name, _ in counted] == ["learn", "utilities", "patterns", "recipes", "agents"]
    assert sum(count for _, count in counted) == len(MANIFEST_NAMES)


def test_a_blank_query_keeps_every_entry_in_manifest_order() -> None:
    entries = load_entries()
    assert [entry.name for entry in search(entries, "  ")] == MANIFEST_NAMES


@pytest.mark.parametrize("name", MANIFEST_NAMES)
def test_every_entry_is_findable_by_its_own_name(name: str) -> None:
    """Search reaches everything the manifest holds, and ranks it first."""
    found = search(load_entries(), name)
    assert found and found[0].name == name


@pytest.mark.parametrize("category", ["learn", "utilities", "patterns", "recipes", "agents"])
def test_category_filter_returns_exactly_that_category(category: str) -> None:
    found = search(load_entries(), "", category)
    assert found
    assert {entry.category for entry in found} == {category}


def test_search_matches_intent_text() -> None:
    """"creative" is only in learn/hello's intent, never in a name."""
    names = [entry.name for entry in search(load_entries(), "creative")]
    assert "learn/hello" in names


def test_search_matches_tags() -> None:
    names = [entry.name for entry in search(load_entries(), "cyberdiner")]
    assert "learn/cyberdiner_hello" in names


def test_the_named_entry_outranks_incidental_matches() -> None:
    """"hello" means learn/hello, not learn/cyberdiner_hello."""
    assert search(load_entries(), "hello")[0].name == "learn/hello"


def test_fuzzy_matching_tolerates_gaps() -> None:
    """Subsequence hits still land, one rank below the literal ones."""
    entry = next(e for e in load_entries() if e.name == "utilities/summarize")
    assert match_rank("smrz", entry) is not None
    assert match_rank("smrz", entry) > (match_rank("summarize", entry) or 0)


def test_a_query_matching_nothing_returns_nothing() -> None:
    assert search(load_entries(), "zzzzq") == []


# ── the detail pane, on its own ──────────────────────────────────────────────


@pytest.mark.parametrize("name", MANIFEST_NAMES)
def test_detail_lines_render_for_every_entry(name: str) -> None:
    entry = next(e for e in load_entries() if e.name == name)
    text = "\n".join(detail_lines(entry))
    assert entry.name in text
    for label in ("Category", "File", "Difficulty", "Primitives", "Backends", "Inputs", "Outputs"):
        assert label in text


def test_detail_lines_render_every_manifest_field() -> None:
    """One entry that has every optional field, checked value by value."""
    entry = next(e for e in load_entries() if e.name == "learn/cyberdiner_hello")
    text = "\n".join(detail_lines(entry))
    assert entry.intent in text
    assert str(entry.raw["when_to_use"])[:40] in text
    assert "beginner" in text
    assert "prompt" in text
    assert "cyberdiner" in text  # tag
    assert "question" in text  # input name
    assert "answer" in text  # output name
    assert str(entry.raw["example"]) in text
    assert "learn/cyberdiner_hello.yml" in text


def test_detail_lines_have_copy_for_no_selection() -> None:
    assert "Nothing selected" in "\n".join(detail_lines(None))


def test_empty_state_names_the_query_and_the_way_out() -> None:
    lines = empty_state_lines("  zzz  ")
    assert '"zzz"' in lines[0]
    assert any("Esc" in line for line in lines)


# ── browsing ─────────────────────────────────────────────────────────────────


def test_the_library_hotkey_opens_the_real_view(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[str, int]:
        screen = await open_library(pilot)
        return type(screen).__name__, len(screen.matches)

    assert run_app(scenario) == (LibraryScreen.__name__, len(MANIFEST_NAMES))


def test_walking_the_category_tree_reaches_every_entry(run_app: Any) -> None:
    """Every manifest entry shows up under one of the tree's categories."""

    async def scenario(pilot: Pilot[Any]) -> list[str]:
        screen = await open_library(pilot)
        tree = screen.query_one("#library-tree", Tree)
        seen: list[str] = []
        for line in range(1, len(categories(screen.entries)) + 1):
            tree.cursor_line = line
            await pilot.pause()
            seen.extend(entry.name for entry in screen.matches)
        return seen

    assert sorted(run_app(scenario)) == sorted(MANIFEST_NAMES)


def test_the_list_shows_the_entries_for_the_selected_category(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[str | None, int]:
        screen = await open_library(pilot)
        tree = screen.query_one("#library-tree", Tree)
        tree.cursor_line = 1  # first category
        await pilot.pause()
        options = screen.query_one("#library-list", OptionList)
        return screen.category, options.option_count

    category, count = run_app(scenario)
    assert category == "learn"
    assert count == len([n for n in MANIFEST_NAMES if n.startswith("learn/")])


def test_highlighting_a_row_updates_the_detail_pane(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[str, str]:
        screen = await open_library(pilot)
        await pilot.press("down")
        await pilot.pause()
        entry = screen.selected_entry
        assert entry is not None
        return entry.name, widget_text(screen, "#library-detail")

    name, detail = run_app(scenario)
    assert name == MANIFEST_NAMES[1]
    assert detail == "\n".join(detail_lines(next(e for e in load_entries() if e.name == name)))


# ── searching ────────────────────────────────────────────────────────────────


def test_slash_focuses_the_search_box(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> bool:
        screen = await open_library(pilot)
        await pilot.press("slash")
        await pilot.pause()
        return bool(screen.query_one("#library-search", Input).has_focus)

    assert run_app(scenario) is True


def test_typing_filters_the_list_live(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> list[str]:
        screen = await open_library(pilot)
        await type_search(pilot, "hello")
        return [entry.name for entry in screen.matches]

    names = run_app(scenario)
    assert names[0] == "learn/hello"
    assert "learn/cyberdiner_hello" in names


def test_searching_leaves_the_category_filter_so_no_match_is_hidden(run_app: Any) -> None:
    """A search inside `learn` would hide `utilities` hits, so it widens."""

    async def scenario(pilot: Pilot[Any]) -> tuple[str | None, list[str]]:
        screen = await open_library(pilot)
        tree = screen.query_one("#library-tree", Tree)
        tree.cursor_line = 1  # learn
        await pilot.pause()
        await type_search(pilot, "summarize")
        return screen.category, [entry.name for entry in screen.matches]

    category, names = run_app(scenario)
    assert category is None
    assert "utilities/summarize" in names


def test_an_empty_result_shows_the_empty_state_not_a_blank_panel(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[bool, bool, str, str]:
        screen = await open_library(pilot)
        await type_search(pilot, "zzzz")
        options = screen.query_one("#library-list", OptionList)
        empty = screen.query_one("#library-empty", Static)
        return (
            bool(options.display),
            bool(empty.display),
            widget_text(screen, "#library-empty"),
            widget_text(screen, "#library-detail"),
        )

    list_shown, empty_shown, empty_text, detail_text = run_app(scenario)
    assert (list_shown, empty_shown) == (False, True)
    assert '"zzzz"' in empty_text
    assert "Esc" in empty_text
    assert "Nothing selected" in detail_text


def test_escape_clears_the_search_and_keeps_the_view(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[str, int, str | None]:
        screen = await open_library(pilot)
        await type_search(pilot, "hello")
        await pilot.press("escape")
        await pilot.pause()
        app_screen = pilot.app.screen
        return (
            screen.query_one("#library-search", Input).value,
            len(screen.matches),
            type(app_screen).__name__,
        )

    assert run_app(scenario) == ("", len(MANIFEST_NAMES), LibraryScreen.__name__)


def test_escape_on_a_clean_view_still_goes_home(run_app: Any) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        await open_library(pilot)
        await pilot.press("escape")
        await pilot.pause()
        return type(pilot.app.screen).__name__

    assert run_app(scenario) == "HomeScreen"


def test_typing_q_searches_instead_of_quitting(run_app: Any) -> None:
    """The search box owns printable keys — otherwise it could not be used."""

    async def scenario(pilot: Pilot[Any]) -> tuple[str, bool]:
        screen = await open_library(pilot)
        await type_search(pilot, "q")
        return screen.query_one("#library-search", Input).value, bool(pilot.app.is_running)

    assert run_app(scenario) == ("q", True)


# ── ejecting ─────────────────────────────────────────────────────────────────


def cli_eject(name: str, cwd: Path) -> Path:
    """Run ``cof eject`` in ``cwd`` and return the file it wrote."""
    import os

    entry = next(e for e in load_entries() if e.name == name)
    cwd.mkdir(parents=True, exist_ok=True)
    previous = Path.cwd()
    os.chdir(cwd)
    try:
        result = CliRunner().invoke(cli_app, ["eject", name])
        assert result.exit_code == 0, result.output
    finally:
        os.chdir(previous)
    return cwd / entry.file


def test_eject_writes_the_bytes_cof_eject_writes(
    run_app: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC: the TUI's ``e`` and ``cof eject`` produce the same file."""
    from_cli = cli_eject("learn/hello", tmp_path / "cli")
    tui_cwd = tmp_path / "tui"
    tui_cwd.mkdir()
    monkeypatch.chdir(tui_cwd)

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await open_library(pilot)
        await pilot.press("e")
        await pilot.pause()
        return widget_text(screen, "#library-status")

    status = run_app(scenario)
    written = tui_cwd / "learn" / "hello.yml"
    assert written.read_bytes() == from_cli.read_bytes()
    assert written.read_text(encoding="utf-8") == eject_text(
        next(e for e in load_entries() if e.name == "learn/hello").raw
    )
    assert "Ejected" in status


def test_eject_of_a_highlighted_entry_lands_in_its_category_directory(
    run_app: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)

    async def scenario(pilot: Pilot[Any]) -> str:
        await open_library(pilot)
        await type_search(pilot, "critique")
        await pilot.press("enter")  # hand focus back to the list
        await pilot.pause()
        await pilot.press("e")
        await pilot.pause()
        return "ok"

    assert run_app(scenario) == "ok"
    assert (tmp_path / "utilities" / "critique.yml").exists()


def test_overwrite_needs_confirmation_and_declining_keeps_the_file(
    run_app: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    dest = tmp_path / "learn" / "hello.yml"
    dest.parent.mkdir(parents=True)
    dest.write_text("mine, not yours\n", encoding="utf-8")

    async def scenario(pilot: Pilot[Any]) -> tuple[str, str, str]:
        screen = await open_library(pilot)
        await pilot.press("e")
        await pilot.pause()
        modal = type(pilot.app.screen).__name__
        during = dest.read_text(encoding="utf-8")
        await pilot.press("n")
        await pilot.pause()
        return modal, during, widget_text(screen, "#library-status")

    modal, during, status = run_app(scenario)
    assert modal == ConfirmOverwrite.__name__
    assert during == "mine, not yours\n"
    assert dest.read_text(encoding="utf-8") == "mine, not yours\n"
    assert "Kept" in status


def test_confirming_the_overwrite_writes_the_curation_file(
    run_app: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    dest = tmp_path / "learn" / "hello.yml"
    dest.parent.mkdir(parents=True)
    dest.write_text("stale\n", encoding="utf-8")

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await open_library(pilot)
        await pilot.press("e")
        await pilot.pause()
        await pilot.press("y")
        await pilot.pause()
        return widget_text(screen, "#library-status")

    status = run_app(scenario)
    entry = next(e for e in load_entries() if e.name == "learn/hello")
    assert dest.read_text(encoding="utf-8") == eject_text(entry.raw)
    assert "Ejected" in status


def test_eject_with_nothing_matching_says_so(
    run_app: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await open_library(pilot)
        await type_search(pilot, "zzzz")
        await pilot.press("enter")
        await pilot.pause()
        await pilot.press("e")
        await pilot.pause()
        return widget_text(screen, "#library-status")

    assert "Nothing to eject" in run_app(scenario)
    assert list(tmp_path.iterdir()) == []


# ── resize safety ────────────────────────────────────────────────────────────


@pytest.mark.parametrize("size", [(80, 24), (40, 12), (24, 8), (10, 4)])
def test_the_browser_renders_at_every_size(render: Any, size: tuple[int, int]) -> None:
    frame = render(size=size, keys=["1"])
    assert frame.strip()
    assert MANIFEST_NAMES[0][:5] in frame  # the list survives every breakpoint
