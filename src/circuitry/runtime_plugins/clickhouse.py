"""ClickHouse B-prime persistence runtime plugin.

Optional dep: ``clickhouse-connect``. Install with
``pip install circuitry-cof[clickhouse]``.

ClickHouse uses MergeTree engines and lacks DBAPI cursor semantics —
``clickhouse-connect`` exposes a ``Client`` with ``command()`` / ``insert()`` /
``query()`` methods rather than ``cursor.execute()``. This plugin
therefore does not extend :class:`SqlPersistenceBase` and instead
implements its own lifecycle hooks against the Client interface.

Schema notes:
  * Both tables use ``ENGINE = MergeTree()``.
  * ``runs`` is ordered by ``run_id``; ``effect_results`` by
    ``(run_id, id)`` for fast filtering by run.
  * ``id`` is a plugin-managed monotonic counter (per-run) since
    ClickHouse has no native auto-increment.
  * JSON values are stored as ``String`` (text-encoded JSON) for
    portability — ClickHouse 24.x has a native JSON type, but the
    String approach works on every version with no settings.

Configuration:
  * Env var ``CLICKHOUSE_URL`` — connection URL
    (e.g. ``http://default:password@host:8123/database``).
  * Or ``CLICKHOUSE_HOST`` + ``CLICKHOUSE_USER`` + ``CLICKHOUSE_PASSWORD``
    + ``CLICKHOUSE_DATABASE`` + ``CLICKHOUSE_PORT``.
  * Or ``runtime.runtime_plugins.clickhouse.{url,host,...}`` in
    config.json.
"""

from __future__ import annotations

import importlib.util
import json
import logging
import os
import threading
from datetime import datetime, timezone
from typing import Any
from urllib.parse import urlparse

from ..preflight import CheckResult
from ._sql_persistence import (
    coerce_bool,
    extract_inputs,
    infer_effect_type,
    resolve_environment,
)
from ._sql_schema import default_store_raw, parse_effect_path

logger = logging.getLogger(__name__)


_RUNS_DDL = (
    "CREATE TABLE IF NOT EXISTS runs ("
    "  run_id String,"
    "  orchestration_path String,"
    "  status String,"
    "  started_at DateTime64(3),"
    "  ended_at Nullable(DateTime64(3)),"
    "  error Nullable(String),"
    "  inputs String"
    ") ENGINE = ReplacingMergeTree() ORDER BY run_id"
)


_EFFECT_RESULTS_DDL = (
    "CREATE TABLE IF NOT EXISTS effect_results ("
    "  id UInt64,"
    "  run_id String,"
    "  state_path String,"
    "  effect_name String,"
    "  effect_type String,"
    "  parent_path Nullable(String),"
    "  iteration_index Nullable(Int32),"
    "  value Nullable(String),"
    "  raw Nullable(String),"
    "  tokens_sent Nullable(Int32),"
    "  tokens_received Nullable(Int32),"
    "  started_at DateTime64(3),"
    "  ended_at DateTime64(3),"
    "  status String,"
    "  error Nullable(String)"
    ") ENGINE = MergeTree() ORDER BY (run_id, id)"
)


def _now() -> datetime:
    return datetime.now(timezone.utc)


def _resolve_client_kwargs(runtime_config: dict[str, Any]) -> dict[str, Any]:
    cfg_section = (
        (runtime_config or {}).get("runtime_plugins", {}).get("clickhouse", {})
    )
    url = os.environ.get("CLICKHOUSE_URL") or cfg_section.get("url")
    if url:
        parsed = urlparse(url)
        kwargs: dict[str, Any] = {}
        if parsed.hostname:
            kwargs["host"] = parsed.hostname
        if parsed.port:
            kwargs["port"] = parsed.port
        if parsed.username:
            kwargs["username"] = parsed.username
        if parsed.password:
            kwargs["password"] = parsed.password
        if parsed.path and parsed.path != "/":
            kwargs["database"] = parsed.path.lstrip("/")
        if parsed.scheme.startswith("https"):
            kwargs["secure"] = True
        return kwargs

    kwargs = {}
    for env_key, kw in (
        ("CLICKHOUSE_HOST", "host"),
        ("CLICKHOUSE_USER", "username"),
        ("CLICKHOUSE_PASSWORD", "password"),
        ("CLICKHOUSE_DATABASE", "database"),
        ("CLICKHOUSE_PORT", "port"),
    ):
        if os.environ.get(env_key):
            kwargs[kw] = os.environ[env_key]
    for k in ("host", "username", "password", "database", "port"):
        if cfg_section.get(k) is not None and k not in kwargs:
            kwargs[k] = cfg_section[k]
    if "port" in kwargs:
        kwargs["port"] = int(kwargs["port"])
    return kwargs


class ClickhousePlugin:
    name: str = "clickhouse"

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._client: Any = None
        self._run_id: str | None = None
        self._store_raw: bool = False
        self._effect_id_counter: int = 0

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        with self._lock:
            self._client = self._open_client(context.runtime_config or {})
            self._ensure_schema()
            self._store_raw = self._resolve_store_raw(context.runtime_config or {})
            self._run_id = context.run_id
            self._effect_id_counter = 0
            self._client.insert(
                "runs",
                [
                    [
                        context.run_id,
                        str(context.orchestration_path),
                        "running",
                        _now(),
                        None,
                        None,
                        json.dumps(extract_inputs(state), default=str),
                    ]
                ],
                column_names=[
                    "run_id", "orchestration_path", "status", "started_at",
                    "ended_at", "error", "inputs",
                ],
            )

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
            if self._client is None or self._run_id is None:
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

            self._effect_id_counter += 1
            try:
                self._client.insert(
                    "effect_results",
                    [
                        [
                            self._effect_id_counter,
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
                            _now(),
                            _now(),
                            status,
                            error,
                        ]
                    ],
                    column_names=[
                        "id", "run_id", "state_path", "effect_name", "effect_type",
                        "parent_path", "iteration_index", "value", "raw",
                        "tokens_sent", "tokens_received", "started_at", "ended_at",
                        "status", "error",
                    ],
                )
            except Exception as exc:
                logger.warning(
                    "clickhouse plugin failed to record effect %r: %s",
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
        if importlib.util.find_spec("clickhouse_connect") is None:
            return CheckResult(
                ok=False,
                missing=["library:clickhouse-connect"],
                message="pip install clickhouse-connect",
            )
        return CheckResult(ok=True, missing=[])

    # ------------------------------------------------------------------

    def _open_client(self, runtime_config: dict[str, Any]) -> Any:
        import clickhouse_connect  # type: ignore[import-not-found]

        kwargs = _resolve_client_kwargs(runtime_config)
        if not kwargs:
            raise RuntimeError(
                "clickhouse: connection params not set. Set CLICKHOUSE_URL "
                "or CLICKHOUSE_HOST + auth env vars."
            )
        return clickhouse_connect.get_client(**kwargs)

    def _ensure_schema(self) -> None:
        self._client.command(_RUNS_DDL)
        self._client.command(_EFFECT_RESULTS_DDL)

    def _resolve_store_raw(self, runtime_config: dict[str, Any]) -> bool:
        env = resolve_environment()
        env_raw = os.environ.get("CIRCUITRY_CLICKHOUSE_STORE_RAW")
        cfg_section = (
            (runtime_config or {}).get("runtime_plugins", {}).get("clickhouse", {})
        )
        return coerce_bool(
            env_raw,
            coerce_bool(cfg_section.get("store_raw"), default_store_raw(env)),
        )

    def _finalize_run(
        self, *, run_id: str, status: str, error: str | None
    ) -> None:
        with self._lock:
            if self._client is None:
                return
            try:
                # ClickHouse mutations (ALTER ... UPDATE) propagate
                # asynchronously but are durable — the orchestration's
                # state will reflect the final status once the merge
                # completes. parameters embed the timestamp + escaped
                # error string; clickhouse-connect formats values via
                # parameters= dict.
                self._client.command(
                    "ALTER TABLE runs UPDATE "
                    "status = %(status)s, ended_at = %(ended_at)s, "
                    "error = %(error)s WHERE run_id = %(run_id)s",
                    parameters={
                        "status": status,
                        "ended_at": _now(),
                        "error": error,
                        "run_id": run_id,
                    },
                )
            finally:
                try:
                    self._client.close()
                except Exception:
                    pass
                self._client = None
                self._run_id = None


def plugin() -> ClickhousePlugin:
    return ClickhousePlugin()
