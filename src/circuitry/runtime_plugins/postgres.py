"""PostgreSQL B-prime persistence runtime plugin.

Optional dep: ``psycopg2-binary``. Install with
``pip install circuitry-cof[postgres]``.

Configuration sources, in priority order:
  1. Env var ``DATABASE_URL`` (libpq connection URI).
  2. Env var ``CIRCUITRY_POSTGRES_DSN`` (alternative connection URI).
  3. Standard libpq vars: ``PGHOST``, ``PGUSER``, ``PGPASSWORD``,
     ``PGDATABASE``, ``PGPORT``.
  4. ``runtime.runtime_plugins.postgres.dsn`` in config.json.
"""

from __future__ import annotations

import importlib.util
import os
from typing import Any

from ._sql_persistence import SqlPersistenceBase
from ._sql_schema import POSTGRES


def _resolve_dsn(runtime_config: dict[str, Any]) -> str | None:
    cfg_section = (
        (runtime_config or {}).get("runtime_plugins", {}).get("postgres", {})
    )
    return (
        os.environ.get("DATABASE_URL")
        or os.environ.get("CIRCUITRY_POSTGRES_DSN")
        or cfg_section.get("dsn")
    )


class PostgresPlugin(SqlPersistenceBase):
    name: str = "postgres"
    dialect = POSTGRES

    def _check_dep(self) -> tuple[bool, list[str]]:
        # psycopg2 import name; psycopg2-binary is the wheel package.
        if importlib.util.find_spec("psycopg2") is None:
            return False, ["library:psycopg2-binary"]
        return True, []

    def _open_connection(self, runtime_config: dict[str, Any]) -> Any:
        import psycopg2  # type: ignore[import-not-found]

        dsn = _resolve_dsn(runtime_config)
        if dsn:
            return psycopg2.connect(dsn)
        # Fall back to libpq-style env discovery (PGHOST etc.).
        return psycopg2.connect("")


def plugin() -> PostgresPlugin:
    return PostgresPlugin()
