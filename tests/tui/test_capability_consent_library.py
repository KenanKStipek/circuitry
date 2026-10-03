"""Capability consent (#275) for the TUI: the Library view's "run this
entry" hand-off from a remote (refreshable, e.g. GitHub) source carries the
same whole-document gate `cof run hub/entry` applies (#334). The TUI never
prompts for it — unlike `cof run`'s interactive y/N, nothing here can block
mid-launch for an answer without freezing the rest of the UI — so an
unconsented document refuses with the same message a script or CI gets.
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

import pytest
import yaml

pytest.importorskip("textual")

from textual.pilot import Pilot

from circuitry.cli.config import trust_store_path
from circuitry.cli.document_consent import document_digest, record_consent
from circuitry.cli.library_sources import Entry, LibraryRegistry
from circuitry.tui import library as library_module
from circuitry.tui.library import LibraryScreen
from circuitry.tui.run_view import RunScreen

# A document whose only `shell` use is in `finally:` (#332 review: the
# static walk must cover `finally:`, not just `effects:`).
_SHELL_IN_FINALLY: dict[str, Any] = {
    "effects": [],
    "finally": [
        {"type": "tool", "name": "cleanup", "provider": "shell", "params": {"command": "echo"}}
    ],
}


class _FakeRemoteSource:
    """A minimal `LibrarySource`, refreshable like the real `github` one."""

    REFRESHABLE = True

    def __init__(self, name: str, entries: list[Entry]) -> None:
        self.name = name
        self._entries = entries

    def list_entries(self) -> list[Entry]:
        return self._entries

    def resolve(self, ref: str) -> Path | None:
        for entry in self._entries:
            if entry.name == ref:
                return entry.path
        return None


def _write_yaml(path: Path, orch: dict[str, Any]) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(yaml.safe_dump(orch), encoding="utf-8")
    return path


def _wire_registry(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    entry_path = _write_yaml(tmp_path / "hub" / "pipeline.yml", _SHELL_IN_FINALLY)
    registry = LibraryRegistry(
        [_FakeRemoteSource("hub", [Entry(name="pipeline", category="hub", path=entry_path, source="hub")])]
    )
    monkeypatch.setattr(library_module, "library_registry", lambda: (registry, ""))
    return entry_path


async def _open_and_launch(pilot: Pilot[Any]) -> RunScreen:
    await pilot.press("1")
    await pilot.pause()
    screen = pilot.app.screen
    assert isinstance(screen, LibraryScreen)
    assert screen.selected_entry is not None
    await pilot.press("enter")
    await pilot.pause()
    run_screen = pilot.app.base_screen()
    assert isinstance(run_screen, RunScreen)
    run_screen.action_launch()
    deadline = asyncio.get_event_loop().time() + 15.0
    while asyncio.get_event_loop().time() < deadline and run_screen.last_result is None:
        await pilot.pause()
        await asyncio.sleep(0.01)
    return run_screen


def test_running_a_remote_library_entry_refuses_without_consent(
    run_app: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _wire_registry(tmp_path, monkeypatch)

    screen = run_app(_open_and_launch)

    assert screen.last_result is not None
    assert screen.last_result.ok is False
    assert "shell" in (screen.last_result.error or "")
    assert "cof trust" in (screen.last_result.error or "")


def test_running_a_remote_library_entry_succeeds_once_trusted(
    run_app: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    entry_path = _wire_registry(tmp_path, monkeypatch)
    digest = document_digest(entry_path.read_bytes())
    record_consent(digest, frozenset({"shell"}), store_path=trust_store_path())

    screen = run_app(_open_and_launch)

    assert screen.last_result is not None
    assert screen.last_result.ok is True, screen.last_result.error
