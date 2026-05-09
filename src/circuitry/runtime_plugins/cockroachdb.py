"""CockroachDB B-prime persistence runtime plugin.

CockroachDB is wire-compatible with PostgreSQL and uses the same
Python driver (``psycopg2-binary``). The plugin reuses the
:class:`PostgresPlugin` base behaviour with the COCKROACHDB dialect
(an alias of POSTGRES).

Optional dep: ``psycopg2-binary``. Install with
``pip install circuitry-cof[cockroachdb]``.

Configuration sources, in priority order:
  1. Env var ``CRDB_URL``.
  2. Env var ``DATABASE_URL`` (libpq connection URI).
  3. ``runtime.runtime_plugins.cockroachdb.dsn`` in config.json.
"""

from __future__ import annotations

import importlib.util
import os
from typing import Any

from ._sql_persistence import SqlPersistenceBase
from ._sql_schema import COCKROACHDB


def _resolve_dsn(runtime_config: dict[str, Any]) -> str | None:
    cfg_section = (
        (runtime_config or {}).get("runtime_plugins", {}).get("cockroachdb", {})
    )
    return (
        os.environ.get("CRDB_URL")
        or os.environ.get("DATABASE_URL")
        or cfg_section.get("dsn")
    )


class CockroachDBPlugin(SqlPersistenceBase):
    name: str = "cockroachdb"
    dialect = COCKROACHDB

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("psycopg2") is None:
            return False, ["library:psycopg2-binary"]
        return True, []

    def _open_connection(self, runtime_config: dict[str, Any]) -> Any:
        import psycopg2  # type: ignore[import-not-found]

        dsn = _resolve_dsn(runtime_config)
        if not dsn:
            raise RuntimeError(
                "cockroachdb: connection string not set. "
                "Set CRDB_URL or runtime.runtime_plugins.cockroachdb.dsn."
            )
        return psycopg2.connect(dsn)


def plugin() -> CockroachDBPlugin:
    return CockroachDBPlugin()
