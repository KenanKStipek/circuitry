"""End-to-end coverage for profile-selected persistence backends.

jsonl-file and sqlite are exercised for real (stdlib only); mongodb runs
against a fake ``pymongo`` module installed into ``sys.modules``, matching
the seam used by the storage runtime-plugin tests.
"""

from __future__ import annotations

import json
import sqlite3
import sys
import types
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run


def _write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content.strip() + "\n", encoding="utf-8")


def _write_orch(tmp_path: Path) -> Path:
    orch_path = tmp_path / "recipe.yml"
    _write(
        orch_path,
        """
effects:
  - type: prompt
    name: summarize
    template: "summarize {{topic}}"
""",
    )
    return orch_path


@dataclass(frozen=True)
class RecordingAdapter:
    name: str = "primary"
    calls: list = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append((model, prompt))
        return GenerateResult(text=f"summary of {prompt}", raw={})


def _run(orch_path: Path, *, profile: str | None, cfg: CircuitryConfig | None = None):
    return run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=cfg or CircuitryConfig(),
            adapter=RecordingAdapter(),
            profile_name=profile,
        )
    )


# ----------------------------------------------------------------------
# jsonl-file / sqlite round trips
# ----------------------------------------------------------------------


def test_profile_jsonl_file_round_trip(tmp_path: Path) -> None:
    orch_path = _write_orch(tmp_path)
    log_path = tmp_path / "state" / "runs.jsonl"
    _write(
        orch_path.parent / "profiles" / "thorough.yml",
        f"""
inputs:
  topic: "widgets"
persistence:
  backend: jsonl-file
  path: {log_path}
""",
    )

    first = _run(orch_path, profile="thorough")
    assert first.ok is True, first.error
    persistence_node = first.state["runtime"]["persistence"]
    assert persistence_node["backend"] == "jsonl-file"
    assert persistence_node["status"] == "persisted"
    assert persistence_node["persisted"] is True
    assert persistence_node["loaded_from_persistence"] is False
    assert log_path.exists()

    # Second run hydrates from the log written by the first.
    second = _run(orch_path, profile="thorough")
    assert second.ok is True, second.error
    assert second.state["runtime"]["persistence"]["loaded_from_persistence"] is True
    assert (
        second.state["runtime"]["last_run"]["run_id"]
        != first.state["runtime"]["last_run"]["run_id"]
    )

    records = [
        json.loads(line)
        for line in log_path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    assert len(records) == 2
    assert records[-1]["state"]["prime"]["summarize"]["value"] == (
        "summary of summarize widgets"
    )


def test_profile_sqlite_round_trip(tmp_path: Path) -> None:
    orch_path = _write_orch(tmp_path)
    db_path = tmp_path / "state" / "runs.db"
    _write(
        orch_path.parent / "profiles" / "thorough.yml",
        f"""
inputs:
  topic: "widgets"
persistence:
  backend: sqlite
  path: {db_path}
""",
    )

    first = _run(orch_path, profile="thorough")
    assert first.ok is True, first.error
    assert first.state["runtime"]["persistence"]["backend"] == "sqlite"
    assert first.state["runtime"]["persistence"]["persisted"] is True

    second = _run(orch_path, profile="thorough")
    assert second.ok is True, second.error
    assert second.state["runtime"]["persistence"]["loaded_from_persistence"] is True
    # The resumed run sees the previous run's effect output.
    assert second.state["prime"]["summarize"]["value"] == "summary of summarize widgets"

    with sqlite3.connect(db_path) as conn:
        rows = conn.execute("SELECT COUNT(*) FROM circuitry_runs").fetchone()
    assert rows[0] == 2


def test_profile_inputs_still_apply_when_state_is_hydrated(tmp_path: Path) -> None:
    """Profile inputs stay the lowest layer: they fill gaps in resumed state."""
    orch_path = _write_orch(tmp_path)
    db_path = tmp_path / "runs.db"
    profile_path = orch_path.parent / "profiles" / "thorough.yml"
    _write(
        profile_path,
        f"""
inputs:
  topic: "widgets"
persistence:
  backend: sqlite
  db_path: {db_path}
""",
    )
    assert _run(orch_path, profile="thorough").ok is True

    # A key added to the profile after the first run is present on resume.
    _write(
        profile_path,
        f"""
inputs:
  topic: "widgets"
  audience: "engineers"
persistence:
  backend: sqlite
  db_path: {db_path}
""",
    )
    second = _run(orch_path, profile="thorough")
    assert second.ok is True, second.error
    assert second.state["runtime"]["persistence"]["loaded_from_persistence"] is True
    assert second.state["audience"] == "engineers"
    assert second.state["topic"] == "widgets"


# ----------------------------------------------------------------------
# Precedence / opt-out
# ----------------------------------------------------------------------


def test_profile_persistence_wins_over_project_config(tmp_path: Path) -> None:
    orch_path = _write_orch(tmp_path)
    profile_log = tmp_path / "profile-runs.jsonl"
    config_log = tmp_path / "config-runs.jsonl"
    _write(
        orch_path.parent / "profiles" / "thorough.yml",
        f"""
persistence:
  backend: jsonl-file
  path: {profile_log}
""",
    )
    cfg = CircuitryConfig(
        runtime={
            "persistence": {
                "enabled": True,
                "backend": "jsonl-file",
                "path": str(config_log),
            }
        }
    )

    result = _run(orch_path, profile="thorough", cfg=cfg)
    assert result.ok is True, result.error
    assert result.state["runtime"]["persistence"]["path"] == str(profile_log)
    assert result.state["runtime"]["effective_settings"]["sources"]["persistence"] == (
        "profile"
    )
    assert profile_log.exists()
    assert not config_log.exists()


def test_profile_without_persistence_leaves_config_backend_intact(
    tmp_path: Path,
) -> None:
    orch_path = _write_orch(tmp_path)
    config_log = tmp_path / "config-runs.jsonl"
    _write(orch_path.parent / "profiles" / "plain.yml", "inputs:\n  topic: widgets")
    cfg = CircuitryConfig(
        runtime={
            "persistence": {
                "enabled": True,
                "backend": "jsonl-file",
                "path": str(config_log),
            }
        }
    )

    result = _run(orch_path, profile="plain", cfg=cfg)
    assert result.ok is True, result.error
    assert result.state["runtime"]["persistence"]["path"] == str(config_log)
    assert result.state["runtime"]["effective_settings"]["sources"]["persistence"] == (
        "config"
    )


def test_no_persistence_anywhere_is_unchanged(tmp_path: Path) -> None:
    orch_path = _write_orch(tmp_path)
    _write(orch_path.parent / "profiles" / "plain.yml", "inputs:\n  topic: widgets")

    with_profile = _run(orch_path, profile="plain")
    assert with_profile.ok is True, with_profile.error
    assert "persistence" not in with_profile.state["runtime"]
    assert "persistence" not in (
        with_profile.state["runtime"]["effective_settings"]["sources"]
    )
    assert "persistence" not in (
        with_profile.state["runtime"]["effective_settings"]["runtime"]
    )


def test_profile_can_disable_persistence(tmp_path: Path) -> None:
    orch_path = _write_orch(tmp_path)
    config_log = tmp_path / "config-runs.jsonl"
    _write(
        orch_path.parent / "profiles" / "ephemeral.yml",
        """
persistence:
  backend: jsonl-file
  enabled: false
""",
    )
    cfg = CircuitryConfig(
        runtime={
            "persistence": {
                "enabled": True,
                "backend": "jsonl-file",
                "path": str(config_log),
            }
        }
    )

    result = _run(orch_path, profile="ephemeral", cfg=cfg)
    assert result.ok is True, result.error
    assert "persistence" not in result.state["runtime"]
    assert not config_log.exists()


# ----------------------------------------------------------------------
# mongodb (faked SDK)
# ----------------------------------------------------------------------


class _FakeCollection:
    def __init__(self) -> None:
        self.docs: list[dict[str, Any]] = []

    def replace_one(
        self, filt: dict[str, Any], doc: dict[str, Any], upsert: bool = False
    ) -> None:
        del upsert
        self.docs = [d for d in self.docs if d.get("_id") != filt.get("_id")]
        self.docs.append(dict(doc))

    def find_one(
        self, filt: dict[str, Any], sort: list[tuple[str, int]] | None = None
    ) -> dict[str, Any] | None:
        matches = [
            d
            for d in self.docs
            if d.get("orchestration_path") == filt.get("orchestration_path")
        ]
        if sort:
            key, direction = sort[0]
            matches.sort(key=lambda d: str(d.get(key) or ""), reverse=direction < 0)
        return matches[0] if matches else None


class _FakeDatabase:
    def __init__(self) -> None:
        self.collections: dict[str, _FakeCollection] = {}

    def __getitem__(self, name: str) -> _FakeCollection:
        return self.collections.setdefault(name, _FakeCollection())


class _FakeMongoClient:
    def __init__(self, uri: str, shared: dict[str, _FakeDatabase]) -> None:
        self.uri = uri
        self.databases = shared

    def __getitem__(self, name: str) -> _FakeDatabase:
        return self.databases.setdefault(name, _FakeDatabase())

    def close(self) -> None:
        return None


def _install_fake_pymongo(
    monkeypatch: pytest.MonkeyPatch,
) -> tuple[dict[str, _FakeDatabase], list[str]]:
    shared: dict[str, _FakeDatabase] = {}
    seen_uris: list[str] = []

    def make_client(uri: str, *args: Any, **kwargs: Any) -> _FakeMongoClient:
        del args, kwargs
        seen_uris.append(uri)
        return _FakeMongoClient(uri, shared)

    fake_mod = types.ModuleType("pymongo")
    fake_mod.MongoClient = make_client  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "pymongo", fake_mod)
    return shared, seen_uris


def test_profile_mongodb_round_trip_and_credential_redaction(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    shared, seen_uris = _install_fake_pymongo(monkeypatch)
    orch_path = _write_orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "thorough.yml",
        """
inputs:
  topic: "widgets"
persistence:
  backend: mongodb
  uri: "mongodb://svc:sup3rsecret@cluster.example:27017"
  database: circuitry
  collection: runs
""",
    )

    first = _run(orch_path, profile="thorough")
    assert first.ok is True, first.error
    assert first.state["runtime"]["persistence"]["backend"] == "mongodb"
    assert first.state["runtime"]["persistence"]["persisted"] is True
    # Live credentials reach the driver...
    assert seen_uris and all(u.startswith("mongodb://svc:sup3rsecret@") for u in seen_uris)
    # ...but never the recorded state.
    serialized = json.dumps(first.state)
    assert "sup3rsecret" not in serialized
    assert first.state["runtime"]["persistence"]["uri"] == (
        "mongodb://***REDACTED***@cluster.example:27017"
    )
    recorded_runtime = first.state["runtime"]["effective_settings"]["runtime"]
    assert "sup3rsecret" not in json.dumps(recorded_runtime)
    profile_record = first.state["runtime"]["effective_settings"]["profile"]["content"]
    assert "sup3rsecret" not in json.dumps(profile_record)

    assert len(shared["circuitry"]["runs"].docs) == 1

    second = _run(orch_path, profile="thorough")
    assert second.ok is True, second.error
    assert second.state["runtime"]["persistence"]["loaded_from_persistence"] is True
    assert second.state["prime"]["summarize"]["value"] == "summary of summarize widgets"


def test_profile_mongodb_connection_failure_is_actionable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    def exploding_client(uri: str, *args: Any, **kwargs: Any) -> Any:
        raise OSError("connection refused")

    fake_mod = types.ModuleType("pymongo")
    fake_mod.MongoClient = exploding_client  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "pymongo", fake_mod)

    orch_path = _write_orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "thorough.yml",
        """
persistence:
  backend: mongodb
  uri: "mongodb://unreachable:27017"
""",
    )

    result = _run(orch_path, profile="thorough")
    assert result.ok is False
    assert result.error is not None
    # Same shape the runtime-config path produces for a dead backend.
    assert "Failed to load persisted state" in result.error
    assert "MongoDB state load failed" in result.error
    assert result.state["runtime"]["persistence"]["status"] == "load_failed"
