"""SQLite persistence runtime plugin.

Persists each run as a ``runs`` row plus one ``effect_results`` row per
effect via :class:`SqlPersistenceBase`. Stdlib ``sqlite3`` only — zero
external dependencies. Suitable for single-machine development; multi-
process workloads should reach for postgres / mysql.

Loaded via ``circuitry.runtime_plugins.sqlite`` (uses the module-level
``plugin`` factory) or the explicit ``circuitry.runtime_plugins.sqlite:plugin``.

Configuration sources, in priority order:
  1. Env var ``CIRCUITRY_SQLITE_PATH`` for the database file.
  2. Env var ``CIRCUITRY_SQLITE_STORE_RAW`` (truthy: ``1``, ``true``, ``yes``).
  3. ``runtime.runtime_plugins.sqlite.{db_path,store_raw}`` in
     config.json (read from ``PluginContext.runtime_config``).
  4. Defaults: ``./circuitry-runs.db`` and ``store_raw=true`` in
     ``CIRCUITRY_ENV=dev``, else ``false``.
"""

from __future__ import annotations

import os
import sqlite3
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from ._sql_persistence import SqlPersistenceBase
from ._sql_schema import SQLITE


class SqlitePlugin(SqlPersistenceBase):
    name: str = "sqlite"
    dialect = SQLITE

    def _check_dep(self) -> tuple[bool, list[str]]:
        # sqlite3 ships with the Python stdlib.
        return True, []

    def _open_connection(self, runtime_config: dict[str, Any]) -> sqlite3.Connection:
        env_db = os.environ.get("CIRCUITRY_SQLITE_PATH")
        cfg_section = (runtime_config or {}).get("runtime_plugins", {}).get("sqlite", {})
        db_path = Path(
            env_db or cfg_section.get("db_path") or "./circuitry-runs.db"
        ).expanduser()
        db_path.parent.mkdir(parents=True, exist_ok=True)
        # check_same_thread=False — on_effect_complete may fire from
        # ThreadPoolExecutor workers in tree-flow execution. The
        # base class's lock serialises access.
        return sqlite3.connect(str(db_path), check_same_thread=False)

    def _setup_connection(self, conn: sqlite3.Connection) -> None:
        conn.execute("PRAGMA foreign_keys = ON")

    def check(self) -> CheckResult:
        return CheckResult(
            ok=True,
            missing=[],
            message="sqlite uses stdlib sqlite3; persistence path is "
                    "validated when the run starts.",
        )


def plugin() -> SqlitePlugin:
    return SqlitePlugin()
