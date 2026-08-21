"""Unit coverage for the jsonl-file and mongodb persistence backends.

The mongodb backend is exercised against a fake ``pymongo`` module
installed into ``sys.modules`` — the same seam the storage runtime-plugin
tests use (``tests/runtime_plugins/test_storage_plugins.py``).
"""

from __future__ import annotations

import json
import re
import sys
import types
from pathlib import Path
from typing import Any, ClassVar

import pytest

from circuitry.core.store import (
    JsonlFileStatePersistence,
    MongodbStatePersistence,
    build_persistence_backend,
)
from circuitry.core.store.mongodb import sanitize_uri

# ----------------------------------------------------------------------
# Fake pymongo
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
    instances: ClassVar[list[_FakeMongoClient]] = []

    def __init__(self, uri: str, *args: Any, **kwargs: Any) -> None:
        del args, kwargs
        self.uri = uri
        self.databases: dict[str, _FakeDatabase] = {}
        self.closed = False
        _FakeMongoClient.instances.append(self)

    def __getitem__(self, name: str) -> _FakeDatabase:
        return self.databases.setdefault(name, _FakeDatabase())

    def close(self) -> None:
        self.closed = True


@pytest.fixture
def fake_pymongo(monkeypatch: pytest.MonkeyPatch) -> dict[str, _FakeDatabase]:
    """Install a fake ``pymongo`` whose data survives client churn."""
    shared: dict[str, _FakeDatabase] = {}
    _FakeMongoClient.instances = []

    def make_client(uri: str, *args: Any, **kwargs: Any) -> _FakeMongoClient:
        client = _FakeMongoClient(uri, *args, **kwargs)
        client.databases = shared
        return client

    fake_mod = types.ModuleType("pymongo")
    fake_mod.MongoClient = make_client  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "pymongo", fake_mod)
    return shared


# ----------------------------------------------------------------------
# jsonl-file
# ----------------------------------------------------------------------


def test_jsonl_requires_path() -> None:
    with pytest.raises(ValueError, match=re.escape("requires runtime.persistence.path")):
        JsonlFileStatePersistence.from_config({})


def test_jsonl_round_trip_returns_latest_record(tmp_path: Path) -> None:
    backend = JsonlFileStatePersistence.from_config(
        {"path": str(tmp_path / "logs" / "runs.jsonl")}
    )
    assert backend.load_latest_state(orchestration_path="orch.yml") is None

    backend.save_run_snapshot(
        orchestration_path="orch.yml",
        run_id="r1",
        ok=True,
        error=None,
        state={"n": 1},
    )
    backend.save_run_snapshot(
        orchestration_path="other.yml",
        run_id="r2",
        ok=True,
        error=None,
        state={"n": 99},
    )
    backend.save_run_snapshot(
        orchestration_path="orch.yml",
        run_id="r3",
        ok=False,
        error="boom",
        state={"n": 2},
    )

    assert backend.load_latest_state(orchestration_path="orch.yml") == {"n": 2}
    assert backend.load_latest_state(orchestration_path="other.yml") == {"n": 99}
    assert backend.load_latest_state(orchestration_path="missing.yml") is None

    lines = (tmp_path / "logs" / "runs.jsonl").read_text(encoding="utf-8").splitlines()
    assert len(lines) == 3
    last = json.loads(lines[-1])
    assert last["run_id"] == "r3"
    assert last["ok"] is False
    assert last["error"] == "boom"
    assert last["created_at"]


def test_jsonl_skips_malformed_lines(tmp_path: Path) -> None:
    log = tmp_path / "runs.jsonl"
    backend = JsonlFileStatePersistence.from_config({"path": str(log)})
    backend.save_run_snapshot(
        orchestration_path="orch.yml", run_id="r1", ok=True, error=None, state={"n": 1}
    )
    with log.open("a", encoding="utf-8") as fh:
        fh.write('{"truncated": \n')

    assert backend.load_latest_state(orchestration_path="orch.yml") == {"n": 1}


def test_jsonl_describe_exposes_path(tmp_path: Path) -> None:
    backend = JsonlFileStatePersistence.from_config({"path": str(tmp_path / "r.jsonl")})
    assert backend.describe() == {
        "backend": "jsonl-file",
        "path": str(tmp_path / "r.jsonl"),
    }


# ----------------------------------------------------------------------
# mongodb
# ----------------------------------------------------------------------


def test_mongodb_requires_uri() -> None:
    with pytest.raises(ValueError, match=re.escape("requires runtime.persistence.uri")):
        MongodbStatePersistence.from_config({"database": "db"})


def test_mongodb_round_trip(fake_pymongo: dict[str, _FakeDatabase]) -> None:
    backend = MongodbStatePersistence.from_config(
        {"uri": "mongodb://localhost:27017", "database": "db", "collection": "runs"}
    )
    assert backend.load_latest_state(orchestration_path="orch.yml") is None

    backend.save_run_snapshot(
        orchestration_path="orch.yml", run_id="r1", ok=True, error=None, state={"n": 1}
    )
    assert backend.load_latest_state(orchestration_path="orch.yml") == {"n": 1}

    doc = fake_pymongo["db"]["runs"].docs[0]
    assert doc["_id"] == "r1"
    assert doc["ok"] is True
    # Clients are closed after every operation.
    assert all(c.closed for c in _FakeMongoClient.instances)


def test_mongodb_describe_redacts_uri_credentials(
    fake_pymongo: dict[str, _FakeDatabase],
) -> None:
    backend = MongodbStatePersistence.from_config(
        {"uri": "mongodb://user:sup3rsecret@cluster.example:27017/?tls=true"}
    )
    described = backend.describe()
    assert "sup3rsecret" not in json.dumps(described)
    assert described["uri"] == (
        "mongodb://***REDACTED***@cluster.example:27017/?tls=true"
    )
    assert described["backend"] == "mongodb"
    assert described["database"] == "circuitry"
    assert described["collection"] == "circuitry_runs"


def test_sanitize_uri_leaves_credential_free_uris_alone() -> None:
    assert sanitize_uri("mongodb://host:27017") == "mongodb://host:27017"


def test_mongodb_connection_failure_is_actionable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def exploding_client(uri: str, *args: Any, **kwargs: Any) -> Any:
        raise OSError("connection refused")

    fake_mod = types.ModuleType("pymongo")
    fake_mod.MongoClient = exploding_client  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "pymongo", fake_mod)

    backend = MongodbStatePersistence.from_config({"uri": "mongodb://nope:27017"})

    with pytest.raises(RuntimeError, match="MongoDB state load failed"):
        backend.load_latest_state(orchestration_path="orch.yml")
    with pytest.raises(RuntimeError, match="MongoDB state save failed"):
        backend.save_run_snapshot(
            orchestration_path="orch.yml",
            run_id="r1",
            ok=True,
            error=None,
            state={},
        )


def test_mongodb_missing_pymongo_reports_install_hint(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setitem(sys.modules, "pymongo", None)
    backend = MongodbStatePersistence.from_config({"uri": "mongodb://host:27017"})
    with pytest.raises(RuntimeError, match="pip install pymongo"):
        backend.load_latest_state(orchestration_path="orch.yml")


# ----------------------------------------------------------------------
# Factory dispatch
# ----------------------------------------------------------------------


@pytest.mark.parametrize("alias", ["jsonl-file", "jsonl_file", "jsonl"])
def test_factory_builds_jsonl_backend(alias: str, tmp_path: Path) -> None:
    backend = build_persistence_backend(
        {
            "persistence": {
                "enabled": True,
                "backend": alias,
                "path": str(tmp_path / "r.jsonl"),
            }
        }
    )
    assert isinstance(backend, JsonlFileStatePersistence)


@pytest.mark.parametrize("alias", ["mongodb", "mongo"])
def test_factory_builds_mongodb_backend(alias: str) -> None:
    backend = build_persistence_backend(
        {"persistence": {"enabled": True, "backend": alias, "uri": "mongodb://h"}}
    )
    assert isinstance(backend, MongodbStatePersistence)


def test_factory_rejects_unknown_backend() -> None:
    with pytest.raises(ValueError, match="Supported backends: jsonl-file, mongodb"):
        build_persistence_backend(
            {"persistence": {"enabled": True, "backend": "carrier-pigeon"}}
        )


def test_factory_disabled_block_returns_none(tmp_path: Path) -> None:
    assert (
        build_persistence_backend(
            {
                "persistence": {
                    "enabled": False,
                    "backend": "jsonl-file",
                    "path": str(tmp_path / "r.jsonl"),
                }
            }
        )
        is None
    )
