from __future__ import annotations

from pathlib import Path

import pytest

from circuitry.core.store.sqlite import SQLiteStatePersistence


def test_sqlite_persistence_requires_db_path() -> None:
    with pytest.raises(ValueError) as exc:
        SQLiteStatePersistence.from_config({"backend": "sqlite"})
    assert "runtime.persistence.db_path" in str(exc.value)


def test_sqlite_persistence_accepts_path_alias() -> None:
    backend = SQLiteStatePersistence.from_config(
        {"backend": "sqlite", "path": "/tmp/a.db"}
    )
    assert backend.db_path == "/tmp/a.db"


def test_sqlite_persistence_prefers_db_path_over_path_alias() -> None:
    backend = SQLiteStatePersistence.from_config(
        {"backend": "sqlite", "db_path": "/tmp/canonical.db", "path": "/tmp/alias.db"}
    )
    assert backend.db_path == "/tmp/canonical.db"


def test_sqlite_persistence_rejects_invalid_table_name() -> None:
    with pytest.raises(ValueError):
        SQLiteStatePersistence.from_config(
            {"backend": "sqlite", "db_path": "/tmp/a.db", "table": "bad-name"}
        )


def test_sqlite_persistence_rejects_sql_injection() -> None:
    with pytest.raises(ValueError):
        SQLiteStatePersistence.from_config(
            {"backend": "sqlite", "db_path": "/tmp/a.db", "table": "x; DROP TABLE y --"}
        )


def test_sqlite_persistence_rejects_over_length_name() -> None:
    with pytest.raises(ValueError):
        SQLiteStatePersistence.from_config(
            {"backend": "sqlite", "db_path": "/tmp/a.db", "table": "a" * 64}
        )


def test_sqlite_persistence_accepts_valid_config() -> None:
    backend = SQLiteStatePersistence.from_config(
        {"backend": "sqlite", "db_path": "/tmp/a.db", "table": "runs_table"}
    )
    assert backend.backend_name == "sqlite"
    assert backend.table == "runs_table"


def test_sqlite_load_latest_state_skips_failed_runs_but_load_run_finds_them(
    tmp_path: Path,
) -> None:
    """`load_latest_state` (plain persistence carryover's own source) must
    not resurrect a failed run's state as the next run's starting point —
    only `load_run` (what `--resume <run-id>` uses) should ever return one
    (#270 F1)."""
    backend = SQLiteStatePersistence.from_config(
        {"backend": "sqlite", "db_path": str(tmp_path / "runs.db")}
    )

    backend.save_run_snapshot(
        orchestration_path="orch.yml", run_id="r1", ok=True, error=None, state={"n": 1}
    )
    backend.save_run_snapshot(
        orchestration_path="orch.yml", run_id="r2", ok=False, error="boom", state={"n": 2}
    )

    assert backend.load_latest_state(orchestration_path="orch.yml") == {"n": 1}
    assert backend.load_run(orchestration_path="orch.yml", run_id="r2") == {"n": 2}
    assert backend.load_run(orchestration_path="orch.yml", run_id="r1") == {"n": 1}
