"""DuckDB B-prime persistence runtime plugin.

Optional dep: ``duckdb``. Install with ``pip install circuitry-cof[duckdb]``.

Embedded analytical database — single-file storage like sqlite, but
column-oriented and ANSI-SQL-richer. Useful for in-place analytics on
the persisted ``runs`` / ``effect_results`` tables.

Configuration:
  1. Env var ``CIRCUITRY_DUCKDB_PATH``.
  2. ``runtime.runtime_plugins.duckdb.db_path`` in config.json.
  3. Default: ``./circuitry-runs.duckdb``.

Auto-increment for ``effect_results.id`` is implemented via a DuckDB
``CREATE SEQUENCE`` (in ``DUCKDB.pre_ddl``) referenced by the column's
``DEFAULT nextval(...)``.
"""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path
from typing import Any

from ._sql_persistence import SqlPersistenceBase
from ._sql_schema import DUCKDB


def _resolve_path(runtime_config: dict[str, Any]) -> Path:
    cfg_section = (
        (runtime_config or {}).get("runtime_plugins", {}).get("duckdb", {})
    )
    raw = (
        os.environ.get("CIRCUITRY_DUCKDB_PATH")
        or cfg_section.get("db_path")
        or "./circuitry-runs.duckdb"
    )
    return Path(raw).expanduser()


class DuckdbPlugin(SqlPersistenceBase):
    name: str = "duckdb"
    dialect = DUCKDB

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("duckdb") is None:
            return False, ["library:duckdb"]
        return True, []

    def _open_connection(self, runtime_config: dict[str, Any]) -> Any:
        import duckdb  # type: ignore[import-not-found]

        db_path = _resolve_path(runtime_config)
        db_path.parent.mkdir(parents=True, exist_ok=True)
        return duckdb.connect(str(db_path))


def plugin() -> DuckdbPlugin:
    return DuckdbPlugin()
