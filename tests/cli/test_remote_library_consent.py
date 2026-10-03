"""Capability consent (#275) for a remote (refreshable, e.g. `github`-type)
library source run by bare name — `cof run hub/entry` — not just through
the dedicated `cof run-library`/`run_shared_orchestration` path.
"""

from __future__ import annotations

from pathlib import Path

import yaml

from circuitry.cli.app import _is_remote_library_source
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.library_sources import Entry, LibraryRegistry
from circuitry.cli.runtime_shim import RunRequest, run


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


def _write_yaml(path: Path, orch: dict) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(yaml.safe_dump(orch), encoding="utf-8")
    return path


def test_is_remote_library_source_true_for_a_refreshable_source(tmp_path: Path) -> None:
    entry_path = _write_yaml(tmp_path / "hub" / "pipeline.yml", {"effects": []})
    registry = LibraryRegistry(
        [_FakeRemoteSource("hub", [Entry(name="pipeline", category="hub", path=entry_path, source="hub")])]
    )

    assert _is_remote_library_source("pipeline", registry) is True


def test_is_remote_library_source_false_for_a_local_file() -> None:
    registry = LibraryRegistry([])
    assert _is_remote_library_source(__file__, registry) is False


def test_is_remote_library_source_false_when_nothing_resolves() -> None:
    registry = LibraryRegistry([])
    assert _is_remote_library_source("no-such-entry", registry) is False


_SHELL_TOOL_EFFECT = {
    "type": "tool",
    "name": "t",
    "provider": "shell",
    "params": {"command": "echo", "args": ["hi"]},
}


def test_run_gates_a_shell_tool_document_when_remote_library_source_is_set(
    tmp_path: Path,
) -> None:
    """The other half of `gate_whole_document`: a bare-name run resolved
    from a remote library source must go through the whole-document
    capability gate exactly like a `cof fetch`/`cof run-library` asset does.
    """
    orch_path = _write_yaml(
        tmp_path / "pipeline.yml",
        {"effects": [_SHELL_TOOL_EFFECT]},
    )

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
            remote_library_source=True,
            allow_capabilities=None,
            capability_prompt=None,
        )
    )

    assert result.ok is False
    assert "shell" in (result.error or "")
    assert "cof trust" in (result.error or "")


def test_run_does_not_gate_the_same_document_without_remote_library_source(
    tmp_path: Path,
) -> None:
    orch_path = _write_yaml(
        tmp_path / "pipeline.yml",
        {"effects": [_SHELL_TOOL_EFFECT]},
    )

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
            remote_library_source=False,
            allow_capabilities=None,
            capability_prompt=None,
        )
    )

    assert result.ok is True, result.error
    assert result.state["prime"]["t"]["meta"]["error"] is None
