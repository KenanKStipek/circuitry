"""SQL Server (MSSQL) B-prime persistence runtime plugin.

Optional dep: ``pyodbc`` plus an ODBC driver for SQL Server (e.g.
``msodbcsql18`` on Linux, the bundled driver on Windows). Install with
``pip install circuitry-cof[mssql]``.

Configuration sources, in priority order:
  1. Env var ``MSSQL_URL`` (full ODBC connection string).
  2. ``runtime.runtime_plugins.mssql.connection_string`` in config.json.

Schema notes:
  * SQL Server's autoincrement is ``BIGINT IDENTITY(1,1) PRIMARY KEY``.
  * JSON columns are ``NVARCHAR(MAX)`` with JSON validation handled by
    SQL Server functions; the plugin just stores text.
"""

from __future__ import annotations

import importlib.util
import os
from typing import Any

from ._sql_persistence import SqlPersistenceBase
from ._sql_schema import MSSQL


def _resolve_connection_string(runtime_config: dict[str, Any]) -> str | None:
    cfg_section = (
        (runtime_config or {}).get("runtime_plugins", {}).get("mssql", {})
    )
    return (
        os.environ.get("MSSQL_URL")
        or cfg_section.get("connection_string")
    )


class MssqlPlugin(SqlPersistenceBase):
    name: str = "mssql"
    dialect = MSSQL

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("pyodbc") is None:
            return False, ["library:pyodbc"]
        return True, []

    def _open_connection(self, runtime_config: dict[str, Any]) -> Any:
        import pyodbc  # type: ignore[import-not-found]

        cstr = _resolve_connection_string(runtime_config)
        if not cstr:
            raise RuntimeError(
                "mssql: connection string not set. Set MSSQL_URL or "
                "runtime.runtime_plugins.mssql.connection_string."
            )
        return pyodbc.connect(cstr)


def plugin() -> MssqlPlugin:
    return MssqlPlugin()
