"""Live-database integration test for ``PostgresStatePersistence``.

``tests/core/test_postgres_persistence_io.py`` covers the I/O path against a
fake ``psycopg`` (connect/schema/load/save), reaching 100% coverage of
``core/store/postgres.py`` without a real database. This file is the real
counterpart: a genuine round trip against a live Postgres, so a bad SQL
statement, driver-version incompatibility, or JSON encode/decode mismatch
that a fake can't catch still has one place that would.

Opt-in: set ``CIRCUITRY_RUN_INTEGRATION=1`` and point
``CIRCUITRY_POSTGRES_TEST_DSN`` at a disposable Postgres database (its
``circuitry_runs`` table is dropped and recreated per test run). Requires
``psycopg[binary]`` installed — not part of the standard test environment.

    docker run --rm -p 5432:5432 -e POSTGRES_PASSWORD=postgres postgres:16
    export CIRCUITRY_RUN_INTEGRATION=1
    export CIRCUITRY_POSTGRES_TEST_DSN="postgresql://postgres:postgres@localhost:5432/postgres"
    pip install "psycopg[binary]"
    pytest -m integration tests/integration/test_postgres_persistence_integration.py -q

In CI this runs against a Postgres service container; see
``.github/workflows/postgres-integration.yml``.
"""

from __future__ import annotations

import importlib.util
import os
import uuid

import pytest

from circuitry.core.store.postgres import PostgresStatePersistence

if os.getenv("CIRCUITRY_RUN_INTEGRATION") != "1":
    pytest.skip(
        "Integration tests disabled. Set CIRCUITRY_RUN_INTEGRATION=1 to enable.",
        allow_module_level=True,
    )

_DSN = (os.getenv("CIRCUITRY_POSTGRES_TEST_DSN") or "").strip()
if not _DSN:
    pytest.skip(
        "No test database configured. Set CIRCUITRY_POSTGRES_TEST_DSN to a "
        "disposable Postgres DSN to run this test.",
        allow_module_level=True,
    )

if importlib.util.find_spec("psycopg") is None:
    pytest.skip(
        'psycopg not installed. `pip install "psycopg[binary]"`.',
        allow_module_level=True,
    )

pytestmark = pytest.mark.integration


@pytest.fixture
def backend() -> PostgresStatePersistence:
    """A fresh, disposable table per test so runs don't collide."""
    table = f"circuitry_runs_test_{uuid.uuid4().hex[:8]}"
    persistence = PostgresStatePersistence.from_config(
        {"dsn": _DSN, "table": table, "sslmode": "disable", "allow_insecure": True}
    )
    yield persistence

    import psycopg
    from psycopg import sql

    with psycopg.connect(_DSN, autocommit=True) as conn, conn.cursor() as cur:
        cur.execute(sql.SQL("DROP TABLE IF EXISTS {}").format(sql.Identifier(table)))


def test_load_latest_state_returns_none_before_any_save(
    backend: PostgresStatePersistence,
) -> None:
    assert backend.load_latest_state(orchestration_path="orch.yml") is None


def test_save_then_load_round_trips_nested_state(backend: PostgresStatePersistence) -> None:
    state = {"input": {"name": "ada"}, "prime": {"greet": {"value": "hi ada"}}}

    backend.save_run_snapshot(
        orchestration_path="orch.yml", run_id="run-1", ok=True, error=None, state=state
    )
    loaded = backend.load_latest_state(orchestration_path="orch.yml")

    assert loaded == state


def test_load_latest_state_returns_most_recent_of_several_saves(
    backend: PostgresStatePersistence,
) -> None:
    for i in range(3):
        backend.save_run_snapshot(
            orchestration_path="orch.yml",
            run_id=f"run-{i}",
            ok=True,
            error=None,
            state={"n": i},
        )

    assert backend.load_latest_state(orchestration_path="orch.yml") == {"n": 2}


def test_load_latest_state_scopes_by_orchestration_path(
    backend: PostgresStatePersistence,
) -> None:
    backend.save_run_snapshot(
        orchestration_path="a.yml", run_id="run-a", ok=True, error=None, state={"which": "a"}
    )
    backend.save_run_snapshot(
        orchestration_path="b.yml", run_id="run-b", ok=True, error=None, state={"which": "b"}
    )

    assert backend.load_latest_state(orchestration_path="a.yml") == {"which": "a"}
    assert backend.load_latest_state(orchestration_path="b.yml") == {"which": "b"}


def test_save_run_snapshot_persists_failure_with_error_message(
    backend: PostgresStatePersistence,
) -> None:
    backend.save_run_snapshot(
        orchestration_path="orch.yml",
        run_id="run-failed",
        ok=False,
        error="adapter timed out",
        state={"partial": True},
    )

    # load_latest_state only returns state_json; the ok/error columns are
    # exercised via a raw query to confirm they actually landed in the row.
    import psycopg
    from psycopg import sql

    with psycopg.connect(_DSN, autocommit=True) as conn, conn.cursor() as cur:
        cur.execute(
            sql.SQL("SELECT ok, error FROM {} WHERE run_id = %s").format(
                sql.Identifier(backend.table)
            ),
            ("run-failed",),
        )
        ok, error = cur.fetchone()

    assert ok is False
    assert error == "adapter timed out"


def test_ensure_schema_is_idempotent_across_calls(backend: PostgresStatePersistence) -> None:
    """Calling save twice (two schema-ensure passes) must not raise."""
    backend.save_run_snapshot(
        orchestration_path="orch.yml", run_id="run-1", ok=True, error=None, state={}
    )
    backend.save_run_snapshot(
        orchestration_path="orch.yml", run_id="run-2", ok=True, error=None, state={}
    )

    assert backend.load_latest_state(orchestration_path="orch.yml") == {}
