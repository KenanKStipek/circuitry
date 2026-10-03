"""Capability consent (#275) for the MCP surface: resolving an orchestration
by bare name from a remote (refreshable, e.g. GitHub) library source must go
through the same whole-document gate `cof run hub/entry` applies (#334), and
nothing a caller sends over the wire can grant capabilities itself.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.cli.app import _is_remote_library_source
from circuitry.cli.config import CircuitryConfig, trust_store_path
from circuitry.cli.document_consent import document_digest, record_consent
from circuitry.cli.library_sources import Entry, LibraryRegistry
from circuitry.mcp import server as srv
from circuitry.mcp.runs import RunManager


@pytest.fixture(autouse=True)
def fresh_manager(monkeypatch: pytest.MonkeyPatch) -> RunManager:
    mgr = RunManager(
        quiesce_max_wait_seconds=2.0,
        cancel_join_timeout=2.0,
        worker_poll_interval=0.05,
    )
    monkeypatch.setattr(srv, "_manager", mgr)
    yield mgr
    for run_id in list(mgr._runs):
        run = mgr._runs[run_id]
        if not run.status.is_terminal:
            try:
                mgr.cancel_run(run_id)
            except KeyError:
                pass


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


# A document whose only `shell` use is in `finally:` (#332 security review:
# the static walk must cover `finally:`, not just `effects:`).
_SHELL_IN_FINALLY: dict[str, Any] = {
    "effects": [],
    "finally": [
        {"type": "tool", "name": "cleanup", "provider": "shell", "params": {"command": "echo"}}
    ],
}


def _fake_registry(tmp_path: Path, orch: dict[str, Any]) -> tuple[LibraryRegistry, Path]:
    entry_path = _write_yaml(tmp_path / "hub" / "pipeline.yml", orch)
    registry = LibraryRegistry(
        [_FakeRemoteSource("hub", [Entry(name="pipeline", category="hub", path=entry_path, source="hub")])]
    )
    return registry, entry_path


def test_a_bare_name_from_a_remote_source_is_remote_library_source(tmp_path: Path) -> None:
    registry, _ = _fake_registry(tmp_path, _SHELL_IN_FINALLY)
    assert _is_remote_library_source("pipeline", registry) is True


def test_run_orchestration_refuses_a_remote_shell_document_without_consent(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    registry, _ = _fake_registry(tmp_path, _SHELL_IN_FINALLY)
    monkeypatch.setattr(srv, "_run_registry", lambda cfg: registry)

    resp = srv._run_orchestration_impl(orchestration="pipeline")

    assert resp["status"] == "failed"
    assert "shell" in (resp["error"] or "")
    assert "cof trust" in (resp["error"] or "")


def test_run_orchestration_ignores_a_caller_supplied_capability_grant(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The `run_orchestration` tool has no field a caller can use to grant
    capabilities \u2014 only `initial_state` is caller-controlled, and nothing
    there is ever read for consent purposes."""
    registry, _ = _fake_registry(tmp_path, _SHELL_IN_FINALLY)
    monkeypatch.setattr(srv, "_run_registry", lambda cfg: registry)

    resp = srv._run_orchestration_impl(
        orchestration="pipeline",
        initial_state={"allow_capabilities": ["shell"], "runtime": {"allow_capabilities": ["shell"]}},
    )

    assert resp["status"] == "failed"
    assert "shell" in (resp["error"] or "")


def test_run_orchestration_tool_has_no_capability_granting_field() -> None:
    """Checks the wire schema itself, not just that `initial_state` is
    ignored \u2014 a future field named e.g. `allow_capabilities` would still
    pass the test above as long as the impl never reads it, but it would be
    a grant-shaped field on an untrusted surface. Assert none exists."""
    server = srv._build_server()
    tools = {t.name: t for t in server._tool_manager.list_tools()}
    properties = set(tools["run_orchestration"].parameters["properties"])
    assert properties == {"orchestration", "initial_state", "override_model", "override_to"}


def test_run_orchestration_succeeds_once_the_document_is_trusted(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    registry, entry_path = _fake_registry(tmp_path, _SHELL_IN_FINALLY)
    monkeypatch.setattr(srv, "_run_registry", lambda cfg: registry)

    digest = document_digest(entry_path.read_bytes())
    record_consent(digest, frozenset({"shell"}), store_path=trust_store_path())

    resp = srv._run_orchestration_impl(orchestration="pipeline")

    assert resp["status"] == "completed"
    assert resp["error"] is None


def test_run_registry_falls_back_to_default_on_a_malformed_sources_config(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A malformed `runtime.library.sources` must not make bundled names
    (or plain file paths) unreachable over MCP — `build_registry` raises
    `LibrarySourceError` for e.g. an empty sources list, and `cof run`
    already falls back to the bundled registry rather than letting that
    escape (`cli/app.py:_resolve_orchestration`). `run_orchestration` must
    do the same instead of raising out of `_run_orchestration_impl`."""
    cfg = CircuitryConfig(runtime={"library": {"sources": []}})
    monkeypatch.setattr(srv, "resolve_config", lambda: cfg)

    resp = srv._run_orchestration_impl(
        orchestration="learn/hello", initial_state={"name": "World"}, override_model=True
    )

    # host_claude pauses for the prompt effect rather than failing — proof
    # the registry fallback let resolution and the run itself proceed.
    assert resp["status"] == "paused"
    assert resp["error"] is None
