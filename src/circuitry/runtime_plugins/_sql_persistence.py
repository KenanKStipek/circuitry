"""Shared base class for B-prime SQL persistence runtime plugins.

DBAPI-compatible drivers (sqlite3, psycopg2, mysql.connector, duckdb,
pyodbc) inherit from :class:`SqlPersistenceBase` and provide three
overrides: a dependency check, a connection-opening method, and an
optional connection-setup hook (pragmas, isolation level, etc).

The base class handles:
  * Schema bootstrap via :func:`runs_ddl` / :func:`effect_results_ddl`
    rendered against the subclass's :class:`SqlDialect`.
  * The lifecycle hooks (``on_run_start`` / ``on_effect_complete`` /
    ``on_run_success`` / ``on_run_failure``).
  * Threading — ``on_effect_complete`` may fire from parallel workers
    in tree-flow execution, so all connection access goes through a
    per-instance lock.
  * ``effect_path`` decomposition into
    ``(effect_name, parent_path, iteration_index)`` via
    :func:`parse_effect_path`.
  * ``store_raw`` resolution (env > config > environment-default).

ClickHouse uses a different driver model (clickhouse-connect's Client
doesn't follow DBAPI cursor semantics) and has its own plugin file.
"""

from __future__ import annotations

import json
import logging
import os
import threading
from datetime import datetime, timezone
from typing import Any

from ..preflight import CheckResult
from ._sql_schema import (
    INDEX_DDL,
    SqlDialect,
    default_store_raw,
    effect_results_ddl,
    parse_effect_path,
    runs_ddl,
)

logger = logging.getLogger(__name__)
_TRUTHY = ("1", "true", "yes", "y", "on")


def now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def coerce_bool(val: Any, default: bool) -> bool:
    if val is None:
        return default
    if isinstance(val, bool):
        return val
    if isinstance(val, str):
        return val.strip().lower() in _TRUTHY
    return bool(val)


def resolve_environment() -> str:
    return (
        os.environ.get("CIRCUITRY_ENV")
        or os.environ.get("CIRCUITRY_ENVIRONMENT")
        or "dev"
    )


def extract_inputs(state: dict[str, Any]) -> dict[str, Any]:
    """Skip framework namespaces, return user-supplied seed values."""
    skip = {"prime", "runtime", "_run_id", "_timestamp"}
    return {k: v for k, v in state.items() if k not in skip}


def infer_effect_type(meta: dict[str, Any], node: dict[str, Any]) -> str:
    if "prompt_type" in meta or "prompt_sent" in meta:
        return "prompt"
    if "stdout" in meta or "exit_code" in meta:
        return "tool"
    if "flow" in meta:
        return "dynamic"
    if isinstance(node.get("value"), dict) and "iterations" in node["value"]:
        return "loop"
    return "effect"


class SqlPersistenceBase:
    """Base class for B-prime SQL persistence runtime plugins.

    Subclasses must declare ``name`` and ``dialect`` and implement
    :meth:`_check_dep` and :meth:`_open_connection`. They may
    additionally override :meth:`_setup_connection` (per-connection
    pragmas / settings).
    """

    name: str = ""
    dialect: SqlDialect = None  # type: ignore[assignment]

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._conn: Any = None
        self._run_id: str | None = None
        self._store_raw: bool = False

    # ------------------------------------------------------------------
    # Subclass hooks
    # ------------------------------------------------------------------

    def _check_dep(self) -> tuple[bool, list[str]]:
        """Return ``(ok, missing)``. Override per-driver."""
        raise NotImplementedError

    def _open_connection(self, runtime_config: dict[str, Any]) -> Any:
        """Open and return a DBAPI connection. Override per-driver."""
        raise NotImplementedError

    def _setup_connection(self, conn: Any) -> None:
        """Per-connection setup (pragmas, isolation level, etc). Default no-op."""
        return

    # ------------------------------------------------------------------
    # Configuration
    # ------------------------------------------------------------------

    def _resolve_store_raw(self, runtime_config: dict[str, Any]) -> bool:
        """Resolve store_raw with the standard cascade:
        ``CIRCUITRY_<NAME>_STORE_RAW`` env > runtime_plugins.<name>.store_raw
        > environment default (dev=true, prod/test=false).
        """
        env = resolve_environment()
        env_var = f"CIRCUITRY_{self.name.upper()}_STORE_RAW"
        env_raw = os.environ.get(env_var)
        cfg_section = (
            (runtime_config or {}).get("runtime_plugins", {}).get(self.name, {})
        )
        return coerce_bool(
            env_raw,
            coerce_bool(cfg_section.get("store_raw"), default_store_raw(env)),
        )

    # ------------------------------------------------------------------
    # Lifecycle hooks
    # ------------------------------------------------------------------

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        with self._lock:
            self._conn = self._open_connection(context.runtime_config or {})
            self._setup_connection(self._conn)
            self._ensure_schema()
            self._store_raw = self._resolve_store_raw(context.runtime_config or {})
            self._run_id = context.run_id
            inputs_json = json.dumps(extract_inputs(state), default=str)
            ph = self.dialect.placeholder
            self._exec(
                f"INSERT INTO runs "
                f"(run_id, orchestration_path, status, started_at, ended_at, error, inputs) "
                f"VALUES ({ph}, {ph}, 'running', {ph}, NULL, NULL, {ph})",
                (
                    context.run_id,
                    str(context.orchestration_path),
                    now_iso(),
                    inputs_json,
                ),
            )
            self._conn.commit()

    def on_effect_complete(
        self,
        *,
        state: dict[str, Any],
        context: Any,
        effect_path: str,
        effect_result: dict[str, Any],
    ) -> None:
        del state
        with self._lock:
            if self._conn is None or self._run_id is None:
                return
            effect_name, parent_path, iter_index, _ = parse_effect_path(effect_path)
            meta = effect_result.get("meta") if isinstance(effect_result, dict) else {}
            meta = meta if isinstance(meta, dict) else {}
            value = (
                effect_result.get("value") if isinstance(effect_result, dict) else None
            )
            value_json = json.dumps(value, default=str) if value is not None else None
            raw_json: str | None = None
            if self._store_raw and meta.get("raw") is not None:
                raw_json = json.dumps(meta["raw"], default=str)
            error = meta.get("error")
            status = "failed" if error else "success"
            etype = infer_effect_type(meta, effect_result)
            started_at = meta.get("created_at") or now_iso()
            ended_at = meta.get("completed_at") or now_iso()
            ph = self.dialect.placeholder
            placeholders = ", ".join([ph] * 14)
            try:
                self._exec(
                    f"INSERT INTO effect_results "
                    f"(run_id, state_path, effect_name, effect_type, parent_path, "
                    f"iteration_index, value, raw, tokens_sent, tokens_received, "
                    f"started_at, ended_at, status, error) VALUES ({placeholders})",
                    (
                        context.run_id,
                        effect_path,
                        effect_name,
                        etype,
                        parent_path,
                        iter_index,
                        value_json,
                        raw_json,
                        meta.get("tokens_sent"),
                        meta.get("tokens_received"),
                        started_at,
                        ended_at,
                        status,
                        error,
                    ),
                )
                self._conn.commit()
            except Exception as exc:
                logger.warning(
                    "%s plugin failed to record effect %r: %s",
                    self.name, effect_path, exc, exc_info=True,
                )
                raise

    def on_run_success(self, *, state: dict[str, Any], context: Any) -> None:
        del state
        self._finalize_run(run_id=context.run_id, status="success", error=None)

    def on_run_failure(
        self, *, state: dict[str, Any], context: Any, error: str
    ) -> None:
        del state
        self._finalize_run(run_id=context.run_id, status="failed", error=error)

    def check(self) -> CheckResult:
        ok, missing = self._check_dep()
        if not ok:
            return CheckResult(ok=False, missing=missing)
        return CheckResult(ok=True, missing=[])

    # ------------------------------------------------------------------
    # Internals
    # ------------------------------------------------------------------

    def _exec(self, sql: str, params: tuple) -> None:
        cur = self._conn.cursor()
        try:
            cur.execute(sql, params)
        finally:
            try:
                cur.close()
            except Exception:
                pass

    def _ensure_schema(self) -> None:
        cur = self._conn.cursor()
        try:
            for ddl in self.dialect.pre_ddl:
                cur.execute(ddl)
            cur.execute(runs_ddl(self.dialect))
            cur.execute(effect_results_ddl(self.dialect))
            for ddl in INDEX_DDL:
                cur.execute(ddl)
        finally:
            cur.close()
        self._conn.commit()

    def _finalize_run(
        self, *, run_id: str, status: str, error: str | None
    ) -> None:
        with self._lock:
            if self._conn is None:
                return
            try:
                ph = self.dialect.placeholder
                self._exec(
                    f"UPDATE runs SET status = {ph}, ended_at = {ph}, "
                    f"error = {ph} WHERE run_id = {ph}",
                    (status, now_iso(), error, run_id),
                )
                self._conn.commit()
            finally:
                try:
                    self._conn.close()
                except Exception:
                    pass
                self._conn = None
                self._run_id = None
