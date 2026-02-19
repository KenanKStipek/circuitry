from __future__ import annotations

import pytest

from circuitry.core.store.postgres import PostgresStatePersistence


def test_postgres_persistence_requires_dsn() -> None:
    with pytest.raises(ValueError) as exc:
        PostgresStatePersistence.from_config({"backend": "postgres"})
    assert "runtime.persistence.dsn" in str(exc.value)


def test_postgres_persistence_rejects_insecure_sslmode_by_default() -> None:
    with pytest.raises(ValueError) as exc:
        PostgresStatePersistence.from_config(
            {
                "dsn": "postgres://demo",
                "sslmode": "disable",
            }
        )
    assert "Insecure postgres sslmode" in str(exc.value)


def test_postgres_persistence_allows_insecure_sslmode_when_explicitly_enabled() -> None:
    backend = PostgresStatePersistence.from_config(
        {
            "dsn": "postgres://demo",
            "sslmode": "disable",
            "allow_insecure": True,
        }
    )
    assert backend.sslmode == "disable"
