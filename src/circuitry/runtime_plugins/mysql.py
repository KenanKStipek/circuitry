"""MySQL B-prime persistence runtime plugin.

Optional dep: ``mysql-connector-python``. Install with
``pip install circuitry-cof[mysql]``.

Configuration sources, in priority order:
  1. Env var ``MYSQL_URL`` (DSN parsed by mysql-connector).
  2. Standard MYSQL env vars: ``MYSQL_HOST``, ``MYSQL_USER``,
     ``MYSQL_PASSWORD``, ``MYSQL_DATABASE``, ``MYSQL_PORT``.
  3. ``runtime.runtime_plugins.mysql.{dsn,host,user,password,database,port}``
     in config.json.
"""

from __future__ import annotations

import importlib.util
import os
from typing import Any
from urllib.parse import urlparse

from ._sql_persistence import SqlPersistenceBase
from ._sql_schema import MYSQL


def _parse_dsn(dsn: str) -> dict[str, Any]:
    """Parse a ``mysql://user:pass@host:port/db`` URI into kwargs."""
    parsed = urlparse(dsn)
    out: dict[str, Any] = {}
    if parsed.hostname:
        out["host"] = parsed.hostname
    if parsed.port:
        out["port"] = parsed.port
    if parsed.username:
        out["user"] = parsed.username
    if parsed.password:
        out["password"] = parsed.password
    if parsed.path and parsed.path != "/":
        out["database"] = parsed.path.lstrip("/")
    return out


def _resolve_kwargs(runtime_config: dict[str, Any]) -> dict[str, Any]:
    cfg_section = (
        (runtime_config or {}).get("runtime_plugins", {}).get("mysql", {})
    )
    dsn = os.environ.get("MYSQL_URL") or cfg_section.get("dsn")
    if dsn:
        return _parse_dsn(dsn)
    kwargs: dict[str, Any] = {}
    for env_key, kw in (
        ("MYSQL_HOST", "host"),
        ("MYSQL_USER", "user"),
        ("MYSQL_PASSWORD", "password"),
        ("MYSQL_DATABASE", "database"),
        ("MYSQL_PORT", "port"),
    ):
        if os.environ.get(env_key):
            kwargs[kw] = os.environ[env_key]
    for key in ("host", "user", "password", "database", "port"):
        if cfg_section.get(key) is not None and key not in kwargs:
            kwargs[key] = cfg_section[key]
    if "port" in kwargs:
        kwargs["port"] = int(kwargs["port"])
    return kwargs


class MysqlPlugin(SqlPersistenceBase):
    name: str = "mysql"
    dialect = MYSQL

    def _check_dep(self) -> tuple[bool, list[str]]:
        # find_spec raises ModuleNotFoundError when the parent
        # package is absent — wrap defensively.
        try:
            present = importlib.util.find_spec("mysql.connector") is not None
        except ModuleNotFoundError:
            present = False
        if not present:
            return False, ["library:mysql-connector-python"]
        return True, []

    def _open_connection(self, runtime_config: dict[str, Any]) -> Any:
        import mysql.connector  # type: ignore[import-not-found]

        kwargs = _resolve_kwargs(runtime_config)
        if not kwargs:
            raise RuntimeError(
                "mysql: connection params not set. Set MYSQL_URL or "
                "MYSQL_HOST/USER/PASSWORD/DATABASE."
            )
        return mysql.connector.connect(**kwargs)


def plugin() -> MysqlPlugin:
    return MysqlPlugin()
