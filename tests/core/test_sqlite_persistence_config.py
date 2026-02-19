from __future__ import annotations

import pytest

from circuitry.core.store.sqlite import SQLiteStatePersistence


def test_sqlite_persistence_requires_db_path() -> None:
    with pytest.raises(ValueError) as exc:
        SQLiteStatePersistence.from_config({"backend": "sqlite"})
    assert "runtime.persistence.db_path" in str(exc.value)


def test_sqlite_persistence_rejects_invalid_table_name() -> None:
    with pytest.raises(ValueError) as exc:
        SQLiteStatePersistence.from_config(
            {"backend": "sqlite", "db_path": "/tmp/a.db", "table": "bad-name"}
        )
    assert "[A-Za-z_][A-Za-z0-9_]*" in str(exc.value)


def test_sqlite_persistence_accepts_valid_config() -> None:
    backend = SQLiteStatePersistence.from_config(
        {"backend": "sqlite", "db_path": "/tmp/a.db", "table": "runs_table"}
    )
    assert backend.backend_name == "sqlite"
    assert backend.table == "runs_table"
