"""mongodb persistence exercised against ``mongomock``, not a hand-rolled fake.

``tests/core/test_jsonl_mongodb_persistence_config.py`` covers the mongodb
backend against a fake ``pymongo`` built by hand -- a stand-in written from
the same understanding as the code under test, so it can only fail the ways
its author already anticipated. It stays as the fast baseline layer.

This module adds a second layer against ``mongomock``, which implements
``pymongo``'s actual API surface (``MongoClient(uri)``, ``replace_one``,
``find_one(filter, sort=...)``, its ``OperationFailure``/``PyMongoError``
hierarchy). Wrong call signatures, wrong document shape, and mis-caught
error classes can fail here in ways a hand-written fake cannot expose.
``mongomock`` never talks to a network, so this stays out of
``@pytest.mark.integration`` and needs no running mongod.
"""

from __future__ import annotations

import json
import sys
import types
from typing import Any

import mongomock
import pytest
from mongomock.store import ServerStore

from circuitry.core.store import MongodbStatePersistence

# ----------------------------------------------------------------------
# Fixture: install mongomock as ``pymongo`` in sys.modules
# ----------------------------------------------------------------------


class _MongomockHarness:
    """Gives tests a driver-shaped client sharing the backend's data.

    A fresh ``mongomock.MongoClient()`` starts with its own empty in-memory
    ``ServerStore`` -- it does not share data with other instances unless
    given the same store explicitly. The backend opens (and closes) a new
    client per call, so without this the round trip would never see its
    own writes even though a real mongod, where state lives on the server
    rather than the client, would.
    """

    def __init__(self, store: ServerStore) -> None:
        self._store = store

    def client(self) -> Any:
        return mongomock.MongoClient("mongodb://localhost:27017", _store=self._store)


@pytest.fixture
def mongomock_pymongo(monkeypatch: pytest.MonkeyPatch) -> _MongomockHarness:
    store = ServerStore()

    def make_client(uri: str, *args: Any, **kwargs: Any) -> Any:
        del args, kwargs
        return mongomock.MongoClient(uri, _store=store)

    fake_mod = types.ModuleType("pymongo")
    fake_mod.MongoClient = make_client  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "pymongo", fake_mod)
    return _MongomockHarness(store)


# ----------------------------------------------------------------------
# Round trip + document shape
# ----------------------------------------------------------------------


def test_mongodb_round_trip_via_mongomock(mongomock_pymongo: _MongomockHarness) -> None:
    backend = MongodbStatePersistence.from_config(
        {"uri": "mongodb://localhost:27017", "database": "db", "collection": "runs"}
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

    assert backend.load_latest_state(orchestration_path="orch.yml") == {"n": 1}
    assert backend.load_latest_state(orchestration_path="other.yml") == {"n": 99}
    assert backend.load_latest_state(orchestration_path="missing.yml") is None


def test_mongodb_document_shape_matches_jsonl_and_sqlite(
    mongomock_pymongo: _MongomockHarness,
) -> None:
    """Same fields the jsonl/sqlite round-trip tests assert on their
    persisted record, read back here through an independent driver
    connection -- not the backend under test -- so the backends are
    demonstrably interchangeable rather than merely all present.
    """
    backend = MongodbStatePersistence.from_config(
        {"uri": "mongodb://localhost:27017", "database": "db", "collection": "runs"}
    )
    backend.save_run_snapshot(
        orchestration_path="orch.yml",
        run_id="r3",
        ok=False,
        error="boom",
        state={"n": 2},
    )

    doc = mongomock_pymongo.client()["db"]["runs"].find_one({"_id": "r3"})
    assert doc is not None
    assert doc["run_id"] == "r3"
    assert doc["orchestration_path"] == "orch.yml"
    assert doc["ok"] is False
    assert doc["error"] == "boom"
    assert doc["state"] == {"n": 2}
    assert doc["created_at"]


def test_mongodb_load_latest_picks_most_recent_by_created_at(
    mongomock_pymongo: _MongomockHarness,
) -> None:
    """Exercises the backend's ``find_one(..., sort=[("created_at", -1)])``
    call against mongomock's real sort implementation. The hand-rolled fake
    never had more than one record per orchestration in its mongodb test,
    so this ordering behaviour was previously unverified for mongodb even
    though the jsonl backend's equivalent path is covered.
    """
    backend = MongodbStatePersistence.from_config(
        {"uri": "mongodb://localhost:27017", "database": "db", "collection": "runs"}
    )
    raw = mongomock_pymongo.client()["db"]["runs"]
    raw.insert_one(
        {
            "_id": "r1",
            "run_id": "r1",
            "orchestration_path": "orch.yml",
            "created_at": "2026-01-01T00:00:00+00:00",
            "ok": True,
            "error": None,
            "state": {"n": 1},
        }
    )
    raw.insert_one(
        {
            "_id": "r2",
            "run_id": "r2",
            "orchestration_path": "orch.yml",
            "created_at": "2026-01-02T00:00:00+00:00",
            "ok": True,
            "error": None,
            "state": {"n": 2},
        }
    )

    assert backend.load_latest_state(orchestration_path="orch.yml") == {"n": 2}


# ----------------------------------------------------------------------
# Redaction: the connection string must never reach a persisted document
# ----------------------------------------------------------------------


def test_mongodb_round_trip_never_persists_connection_string(
    mongomock_pymongo: _MongomockHarness,
) -> None:
    backend = MongodbStatePersistence.from_config(
        {
            "uri": "mongodb://user:sup3rsecret@cluster.example:27017/?tls=true",
            "database": "db",
            "collection": "runs",
        }
    )
    backend.save_run_snapshot(
        orchestration_path="orch.yml",
        run_id="r1",
        ok=True,
        error=None,
        state={"n": 1},
    )

    raw_doc = mongomock_pymongo.client()["db"]["runs"].find_one({"_id": "r1"})
    assert "sup3rsecret" not in json.dumps(raw_doc)

    described = backend.describe()
    assert "sup3rsecret" not in json.dumps(described)
    assert described["uri"] == "mongodb://***REDACTED***@cluster.example:27017/?tls=true"


# ----------------------------------------------------------------------
# Failure case: auth rejection surfaces the way the other backends surface
# theirs (wrapped in RuntimeError, original exception chained), not
# swallowed.
# ----------------------------------------------------------------------


def test_mongodb_auth_failure_surfaces_as_runtime_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Raises ``mongomock.OperationFailure`` -- the class the real driver
    raises for auth rejections and other server-side command failures --
    rather than a generic stand-in exception, so the assertion is about how
    the backend handles a driver-shaped error, not an invented one.
    """

    def exploding_client(uri: str, *args: Any, **kwargs: Any) -> Any:
        del uri, args, kwargs
        raise mongomock.OperationFailure("Authentication failed.")

    fake_mod = types.ModuleType("pymongo")
    fake_mod.MongoClient = exploding_client  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "pymongo", fake_mod)

    backend = MongodbStatePersistence.from_config(
        {"uri": "mongodb://user:pass@unreachable.invalid:27017"}
    )

    with pytest.raises(RuntimeError, match="MongoDB state load failed") as load_exc:
        backend.load_latest_state(orchestration_path="orch.yml")
    assert isinstance(load_exc.value.__cause__, mongomock.OperationFailure)

    with pytest.raises(RuntimeError, match="MongoDB state save failed") as save_exc:
        backend.save_run_snapshot(
            orchestration_path="orch.yml",
            run_id="r1",
            ok=True,
            error=None,
            state={},
        )
    assert isinstance(save_exc.value.__cause__, mongomock.OperationFailure)
