"""Execution-path coverage for ``PostgresStatePersistence`` via a fake ``psycopg``.

``tests/core/test_postgres_persistence_config.py`` covers ``from_config``'s
validation (DSN, sslmode, table-name injection guard). This file is the I/O
counterpart: ``_connect``/``_ensure_schema``/``load_latest_state``/
``save_run_snapshot`` against a tiny in-memory fake standing in for
``psycopg`` — there's no ``mongomock``-style fake for it in the repo, and
``psycopg`` itself isn't installed in the standard test environment (#266).

See ``tests/integration/test_postgres_persistence_integration.py`` for the
real-database counterpart, opt-in via ``CIRCUITRY_POSTGRES_TEST_DSN``.
"""

from __future__ import annotations

import json
import sys
import types
from dataclasses import dataclass, field
from typing import Any

import pytest

from circuitry.core.store.postgres import PostgresStatePersistence


class FakeIdentifier:
    def __init__(self, name: str) -> None:
        self.name = name

    def __str__(self) -> str:
        return self.name


class FakeSQL:
    """Stands in for ``psycopg.sql.SQL`` — ``.format()`` just does ``{}``
    substitution, since the fake cursor only pattern-matches on substrings."""

    def __init__(self, text: str) -> None:
        self.text = text

    def format(self, *args: Any) -> str:
        result = self.text
        for arg in args:
            result = result.replace("{}", str(arg), 1)
        return result


@dataclass
class FakeRow:
    run_id: str
    orchestration_path: str
    created_at: int
    ok: bool
    error: str | None
    state_json: str


@dataclass
class FakeDatabase:
    rows: list[FakeRow] = field(default_factory=list)
    _counter: int = 0
    raise_on_execute: Exception | None = None

    def next_created_at(self) -> int:
        self._counter += 1
        return self._counter


class FakeCursor:
    def __init__(self, db: FakeDatabase) -> None:
        self._db = db
        self._result: tuple[Any, ...] | None = None

    def __enter__(self) -> FakeCursor:
        return self

    def __exit__(self, *exc_info: object) -> None:
        return None

    def execute(self, query: str, params: tuple[Any, ...] = ()) -> None:
        if self._db.raise_on_execute is not None:
            raise self._db.raise_on_execute
        if "CREATE TABLE" in query or "CREATE INDEX" in query:
            return
        if query.strip().startswith("INSERT INTO"):
            run_id, orchestration_path, ok, error, state_json = params
            self._db.rows.append(
                FakeRow(
                    run_id=run_id,
                    orchestration_path=orchestration_path,
                    created_at=self._db.next_created_at(),
                    ok=ok,
                    error=error,
                    state_json=state_json,
                )
            )
            return
        if query.strip().startswith("SELECT state_json"):
            (orchestration_path,) = params
            matches = sorted(
                (r for r in self._db.rows if r.orchestration_path == orchestration_path),
                key=lambda r: r.created_at,
                reverse=True,
            )
            self._result = (matches[0].state_json,) if matches else None
            return
        raise AssertionError(f"Unexpected query: {query!r}")

    def fetchone(self) -> tuple[Any, ...] | None:
        return self._result


class FakeConnection:
    def __init__(self, db: FakeDatabase) -> None:
        self._db = db

    def __enter__(self) -> FakeConnection:
        return self

    def __exit__(self, *exc_info: object) -> None:
        return None

    def cursor(self) -> FakeCursor:
        return FakeCursor(self._db)


@pytest.fixture
def fake_db(monkeypatch: pytest.MonkeyPatch) -> FakeDatabase:
    db = FakeDatabase()
    fake_psycopg = types.ModuleType("psycopg")
    fake_psycopg.sql = types.SimpleNamespace(SQL=FakeSQL, Identifier=FakeIdentifier)  # type: ignore[attr-defined]
    captured_conninfo: list[str] = []

    def fake_connect(conninfo: str, autocommit: bool = False) -> FakeConnection:
        captured_conninfo.append(conninfo)
        return FakeConnection(db)

    fake_psycopg.connect = fake_connect  # type: ignore[attr-defined]
    fake_psycopg._captured_conninfo = captured_conninfo  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "psycopg", fake_psycopg)
    return db


def _backend(**overrides: Any) -> PostgresStatePersistence:
    return PostgresStatePersistence.from_config({"dsn": "postgresql://demo/db", **overrides})


def test_backend_name_and_describe() -> None:
    backend = _backend(table="custom_runs", sslmode="verify-full")

    assert backend.backend_name == "postgres"
    assert backend.describe() == {
        "backend": "postgres",
        "table": "custom_runs",
        "sslmode": "verify-full",
    }


def test_save_then_load_round_trips_state(fake_db: FakeDatabase) -> None:
    backend = _backend()

    backend.save_run_snapshot(
        orchestration_path="orch.yml",
        run_id="run-1",
        ok=True,
        error=None,
        state={"input": {"name": "ada"}},
    )

    loaded = backend.load_latest_state(orchestration_path="orch.yml")

    assert loaded == {"input": {"name": "ada"}}


def test_load_latest_state_returns_none_when_nothing_persisted(fake_db: FakeDatabase) -> None:
    backend = _backend()

    assert backend.load_latest_state(orchestration_path="never-run.yml") is None


def test_load_latest_state_returns_most_recent_snapshot(fake_db: FakeDatabase) -> None:
    backend = _backend()

    backend.save_run_snapshot(
        orchestration_path="orch.yml", run_id="run-1", ok=True, error=None, state={"n": 1}
    )
    backend.save_run_snapshot(
        orchestration_path="orch.yml", run_id="run-2", ok=True, error=None, state={"n": 2}
    )

    loaded = backend.load_latest_state(orchestration_path="orch.yml")

    assert loaded == {"n": 2}


def test_load_latest_state_scopes_by_orchestration_path(fake_db: FakeDatabase) -> None:
    backend = _backend()

    backend.save_run_snapshot(
        orchestration_path="a.yml", run_id="run-a", ok=True, error=None, state={"which": "a"}
    )
    backend.save_run_snapshot(
        orchestration_path="b.yml", run_id="run-b", ok=True, error=None, state={"which": "b"}
    )

    assert backend.load_latest_state(orchestration_path="a.yml") == {"which": "a"}
    assert backend.load_latest_state(orchestration_path="b.yml") == {"which": "b"}


def test_save_run_snapshot_persists_error_field(fake_db: FakeDatabase) -> None:
    backend = _backend()

    backend.save_run_snapshot(
        orchestration_path="orch.yml",
        run_id="run-1",
        ok=False,
        error="boom",
        state={"ok": False},
    )

    row = fake_db.rows[0]
    assert row.ok is False
    assert row.error == "boom"
    assert json.loads(row.state_json) == {"ok": False}


def test_load_latest_state_accepts_driver_decoded_dict_payload(fake_db: FakeDatabase) -> None:
    """A psycopg JSONB column can come back already decoded to a dict
    (not just the JSON string the fake cursor normally stores)."""
    backend = _backend()
    fake_db.rows.append(
        FakeRow(
            run_id="run-1",
            orchestration_path="orch.yml",
            created_at=fake_db.next_created_at(),
            ok=True,
            error=None,
            state_json={"already": "decoded"},  # type: ignore[arg-type]
        )
    )

    assert backend.load_latest_state(orchestration_path="orch.yml") == {"already": "decoded"}


def test_load_latest_state_rejects_unsupported_payload_type(fake_db: FakeDatabase) -> None:
    backend = _backend()
    fake_db.rows.append(
        FakeRow(
            run_id="run-1",
            orchestration_path="orch.yml",
            created_at=fake_db.next_created_at(),
            ok=True,
            error=None,
            state_json=12345,  # type: ignore[arg-type]
        )
    )

    with pytest.raises(RuntimeError, match="unsupported type"):
        backend.load_latest_state(orchestration_path="orch.yml")


def test_load_latest_state_rejects_non_dict_json_payload(fake_db: FakeDatabase) -> None:
    backend = _backend()
    fake_db.rows.append(
        FakeRow(
            run_id="run-1",
            orchestration_path="orch.yml",
            created_at=fake_db.next_created_at(),
            ok=True,
            error=None,
            state_json=json.dumps([1, 2, 3]),
        )
    )

    with pytest.raises(RuntimeError, match="not a JSON object"):
        backend.load_latest_state(orchestration_path="orch.yml")


def test_load_latest_state_wraps_execution_errors(fake_db: FakeDatabase) -> None:
    fake_db.raise_on_execute = RuntimeError("connection reset")
    backend = _backend()

    with pytest.raises(RuntimeError, match="Postgres state load failed"):
        backend.load_latest_state(orchestration_path="orch.yml")


def test_save_run_snapshot_wraps_execution_errors(fake_db: FakeDatabase) -> None:
    fake_db.raise_on_execute = RuntimeError("duplicate key")
    backend = _backend()

    with pytest.raises(RuntimeError, match="Postgres state save failed"):
        backend.save_run_snapshot(
            orchestration_path="orch.yml", run_id="run-1", ok=True, error=None, state={}
        )


def test_connect_appends_sslmode_when_dsn_lacks_one(fake_db: FakeDatabase) -> None:
    import psycopg  # the fake installed by the fixture

    backend = _backend(sslmode="verify-full")

    backend.save_run_snapshot(
        orchestration_path="orch.yml", run_id="run-1", ok=True, error=None, state={}
    )

    assert psycopg._captured_conninfo[-1].endswith("sslmode=verify-full")  # type: ignore[attr-defined]


def test_connect_does_not_duplicate_existing_sslmode(fake_db: FakeDatabase) -> None:
    import psycopg  # the fake installed by the fixture

    backend = PostgresStatePersistence.from_config(
        {"dsn": "postgresql://demo/db?sslmode=require"}
    )

    backend.save_run_snapshot(
        orchestration_path="orch.yml", run_id="run-1", ok=True, error=None, state={}
    )

    assert psycopg._captured_conninfo[-1].count("sslmode=") == 1  # type: ignore[attr-defined]


def test_connect_raises_actionable_error_when_psycopg_missing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """#266 — the install hint from ``_connect`` must survive, not a raw
    ``ModuleNotFoundError`` from a bare ``from psycopg import sql`` ahead of it."""
    monkeypatch.setitem(sys.modules, "psycopg", None)
    backend = _backend()

    with pytest.raises(RuntimeError, match="pip install psycopg\\[binary\\]"):
        backend.save_run_snapshot(
            orchestration_path="orch.yml", run_id="run-1", ok=True, error=None, state={}
        )
