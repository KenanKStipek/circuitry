"""SQLite persistence runtime plugin.

Persists each run as a ``runs`` row + one ``effect_results`` row per
effect, using the shared B-prime schema. Stdlib ``sqlite3`` only —
zero external dependencies. Suitable for single-machine development /
small deployments; multi-process workloads should reach for postgres.

Loaded via ``circuitry.runtime_plugins.sqlite`` (uses the module-level
``plugin`` factory) or the explicit ``circuitry.runtime_plugins.sqlite:plugin``.

Configuration sources, in priority order:
  1. Env var ``CIRCUITRY_SQLITE_PATH`` for the database file.
  2. Env var ``CIRCUITRY_SQLITE_STORE_RAW`` (truthy values: ``1``,
     ``true``, ``yes``).
  3. ``runtime.runtime_plugins.sqlite.{db_path,store_raw}`` in
     config.json (read from ``PluginContext.runtime_config``).
  4. Defaults: ``./circuitry-runs.db`` and ``store_raw=true`` in
     ``CIRCUITRY_ENV=dev``, else ``false``.

Hook failure semantics: any database error inside a hook is logged
WARNING and re-raised, matching the existing
:func:`invoke_plugins` policy that catches and records as a non-fatal
event. The orchestration run continues regardless.
"""

from __future__ import annotations

import json
import logging
import os
import sqlite3
import threading
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from ._sql_schema import (
    INDEX_DDL,
    SQLITE,
    default_store_raw,
    effect_results_ddl,
    parse_effect_path,
    runs_ddl,
)

logger = logging.getLogger(__name__)

_TRUTHY = ("1", "true", "yes", "y", "on")


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _coerce_bool(val: Any, default: bool) -> bool:
    if val is None:
        return default
    if isinstance(val, bool):
        return val
    if isinstance(val, str):
        return val.strip().lower() in _TRUTHY
    return bool(val)


def _resolve_environment() -> str:
    return (
        os.environ.get("CIRCUITRY_ENV")
        or os.environ.get("CIRCUITRY_ENVIRONMENT")
        or "dev"
    )


def _extract_inputs(state: dict[str, Any]) -> dict[str, Any]:
    """Return only top-level keys that look like user inputs.

    The orchestration's ``prime`` namespace and the ``runtime``
    bookkeeping namespace are excluded — what's left are the
    ``{{user_*}}`` keys plus any caller-supplied seed values.
    """
    skip = {"prime", "runtime", "_run_id", "_timestamp"}
    return {k: v for k, v in state.items() if k not in skip}


class SqlitePlugin:
    name = "sqlite"

    def __init__(self) -> None:
        # Per-run state. Threadsafe via a dedicated lock because
        # on_effect_complete may fire from parallel (tree-flow) workers.
        self._lock = threading.Lock()
        self._conn: sqlite3.Connection | None = None
        self._db_path: Path | None = None
        self._run_id: str | None = None
        self._effect_starts: dict[str, str] = {}  # path -> started_at iso
        self._store_raw: bool = False

    # ------------------------------------------------------------------
    # Configuration resolution
    # ------------------------------------------------------------------

    def _resolve_config(self, runtime_config: dict[str, Any]) -> tuple[Path, bool]:
        env = _resolve_environment()
        env_db = os.environ.get("CIRCUITRY_SQLITE_PATH")
        cfg_section = (runtime_config or {}).get("runtime_plugins", {}).get("sqlite", {})
        db_path = Path(
            env_db
            or cfg_section.get("db_path")
            or "./circuitry-runs.db"
        ).expanduser()
        store_raw = _coerce_bool(
            os.environ.get("CIRCUITRY_SQLITE_STORE_RAW"),
            _coerce_bool(cfg_section.get("store_raw"), default_store_raw(env)),
        )
        return (db_path, store_raw)

    # ------------------------------------------------------------------
    # Schema bootstrap
    # ------------------------------------------------------------------

    def _open(self, db_path: Path) -> sqlite3.Connection:
        db_path.parent.mkdir(parents=True, exist_ok=True)
        # check_same_thread=False is required because on_effect_complete
        # may be invoked from ThreadPoolExecutor workers in tree flow.
        # The plugin's own _lock serialises access.
        conn = sqlite3.connect(str(db_path), check_same_thread=False)
        conn.execute("PRAGMA foreign_keys = ON")
        return conn

    def _ensure_schema(self, conn: sqlite3.Connection) -> None:
        cur = conn.cursor()
        cur.execute(runs_ddl(SQLITE))
        cur.execute(effect_results_ddl(SQLITE))
        for ddl in INDEX_DDL:
            cur.execute(ddl)
        conn.commit()

    # ------------------------------------------------------------------
    # Lifecycle hooks
    # ------------------------------------------------------------------

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        with self._lock:
            db_path, store_raw = self._resolve_config(context.runtime_config or {})
            self._db_path = db_path
            self._store_raw = store_raw
            self._conn = self._open(db_path)
            self._ensure_schema(self._conn)
            self._run_id = context.run_id
            self._effect_starts.clear()
            inputs_json = json.dumps(_extract_inputs(state), default=str)
            self._conn.execute(
                "INSERT OR REPLACE INTO runs "
                "(run_id, orchestration_path, status, started_at, ended_at, error, inputs) "
                "VALUES (?, ?, 'running', ?, NULL, NULL, ?)",
                (
                    context.run_id,
                    str(context.orchestration_path),
                    _now_iso(),
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
        del state  # everything we need is on effect_result
        with self._lock:
            if self._conn is None or self._run_id is None:
                # Ordering quirk — should not happen, but stay defensive.
                return

            effect_name, parent_path, iter_index, _ = parse_effect_path(effect_path)
            meta = effect_result.get("meta") if isinstance(effect_result, dict) else {}
            meta = meta if isinstance(meta, dict) else {}

            value = effect_result.get("value") if isinstance(effect_result, dict) else None
            value_json = json.dumps(value, default=str) if value is not None else None

            raw_json: str | None = None
            if self._store_raw:
                raw = meta.get("raw")
                if raw is not None:
                    raw_json = json.dumps(raw, default=str)

            error = meta.get("error")
            status = "failed" if error else "success"
            effect_type = self._infer_effect_type(meta, effect_result)

            started_at = (
                self._effect_starts.pop(effect_path, None)
                or meta.get("created_at")
                or _now_iso()
            )
            ended_at = meta.get("completed_at") or _now_iso()

            try:
                self._conn.execute(
                    "INSERT INTO effect_results "
                    "(run_id, state_path, effect_name, effect_type, parent_path, "
                    "iteration_index, value, raw, tokens_sent, tokens_received, "
                    "started_at, ended_at, status, error) "
                    "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    (
                        context.run_id,
                        effect_path,
                        effect_name,
                        effect_type,
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
            except sqlite3.Error as exc:
                logger.warning(
                    "sqlite plugin failed to record effect %r: %s",
                    effect_path, exc, exc_info=True,
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
        # sqlite3 is stdlib, so the only dep risk is filesystem-write
        # access — which is environment-specific and can't be probed at
        # plugin-construction time without picking a path. Defer to the
        # actual hook; report ok here.
        return CheckResult(
            ok=True,
            missing=[],
            message="sqlite uses stdlib sqlite3; persistence path is "
                    "validated when the run starts.",
        )

    # ------------------------------------------------------------------
    # Helpers
    # ------------------------------------------------------------------

    def _finalize_run(
        self, *, run_id: str, status: str, error: str | None
    ) -> None:
        with self._lock:
            if self._conn is None:
                return
            try:
                self._conn.execute(
                    "UPDATE runs SET status = ?, ended_at = ?, error = ? "
                    "WHERE run_id = ?",
                    (status, _now_iso(), error, run_id),
                )
                self._conn.commit()
            finally:
                self._conn.close()
                self._conn = None
                self._run_id = None

    @staticmethod
    def _infer_effect_type(meta: dict[str, Any], node: dict[str, Any]) -> str:
        """Best-effort effect type for the row.

        Concrete effect-type metadata isn't on the node today; we
        fall back to keys present on the node to disambiguate. The
        column is non-null per schema, so we always return a string.
        """
        if "prompt_type" in meta or "prompt_sent" in meta:
            return "prompt"
        if "stdout" in meta or "exit_code" in meta:
            return "tool"
        if "flow" in meta:
            return "dynamic"
        if "iterations" in (node.get("value") or {}) if isinstance(node.get("value"), dict) else False:
            return "loop"
        return "effect"


# Module-level factory used by load_plugins("circuitry.runtime_plugins.sqlite").
def plugin() -> SqlitePlugin:
    return SqlitePlugin()
