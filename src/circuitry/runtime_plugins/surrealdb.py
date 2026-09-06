"""SurrealDB B-prime persistence runtime plugin.

Optional dep: ``surrealdb``. Install with ``pip install circuitry-cof[surrealdb]``.

SurrealDB is schemaless and multi-model, so this plugin talks SurrealQL
through the official SDK's ``query()`` method rather than DBAPI cursor
semantics — same driver-model split as :mod:`.clickhouse`, so this does
not extend :class:`~circuitry.runtime_plugins._sql_persistence.SqlPersistenceBase`.

Tables ``runs`` and ``effects`` carry the same field names as the SQL
B-prime schema (:mod:`._sql_schema`) — ``run_id``, ``orchestration_path``,
``status``, ``started_at``, ``ended_at``, ``error``, ``inputs`` on
``runs``; ``run_id``, ``state_path``, ``effect_name``, ``effect_type``,
``parent_path``, ``iteration_index``, ``value``, ``raw``, ``tokens_sent``,
``tokens_received``, ``started_at``, ``ended_at``, ``status``, ``error``
on ``effects``. Unlike the SQL dialects, ``inputs`` / ``value`` / ``raw``
are stored as native SurrealDB objects rather than JSON-encoded text —
SurrealDB has no need for a JSON-as-string column. There is also no
``id`` column on ``effects``: SurrealDB assigns its own record id per
``CREATE``, so — unlike ClickHouse — this plugin does not manage a
monotonic counter.

Connection/auth conventions mirror the ``circuitry.plugins.surrealdb``
tool plugin:

Config sources (priority order), read from
``runtime.runtime_plugins.surrealdb.*`` in config.json when the env var
is unset:
  1. Env var ``SURREAL_URL`` / config ``url``
     (default ``"ws://localhost:8000/rpc"``).
  2. Env var ``SURREAL_NAMESPACE`` / config ``namespace``
     (default ``"circuitry"``).
  3. Env var ``SURREAL_DATABASE`` / config ``database``
     (default ``"circuitry"``).
  4. Env var ``SURREAL_TOKEN`` / config ``token`` — when set, authenticates
     via ``authenticate(token)`` and takes priority over user/pass.
  5. Env var ``SURREAL_USER`` + ``SURREAL_PASS`` / config ``user`` +
     ``password`` — root/namespace/database signin via ``signin(...)``.

``environment: prod`` (``CIRCUITRY_ENV=prod``) omits the ``raw`` field on
``effects`` — same ``store_raw`` cascade as the SQL plugins:
``CIRCUITRY_SURREALDB_STORE_RAW`` env > ``runtime_plugins.surrealdb.store_raw``
config > environment default (dev=true, else false).
"""

from __future__ import annotations

import importlib.util
import logging
import os
import threading
from typing import Any

from ..preflight import CheckResult
from ._sql_persistence import (
    coerce_bool,
    extract_inputs,
    infer_effect_type,
    now_iso,
    resolve_environment,
)
from ._sql_schema import default_store_raw, parse_effect_path

logger = logging.getLogger(__name__)


def _resolve_config(runtime_config: dict[str, Any]) -> dict[str, str | None]:
    cfg = (runtime_config or {}).get("runtime_plugins", {}).get("surrealdb", {})
    return {
        "url": (
            os.environ.get("SURREAL_URL") or cfg.get("url") or "ws://localhost:8000/rpc"
        ),
        "namespace": (
            os.environ.get("SURREAL_NAMESPACE") or cfg.get("namespace") or "circuitry"
        ),
        "database": (
            os.environ.get("SURREAL_DATABASE") or cfg.get("database") or "circuitry"
        ),
        "token": os.environ.get("SURREAL_TOKEN") or cfg.get("token"),
        "user": os.environ.get("SURREAL_USER") or cfg.get("user"),
        "password": os.environ.get("SURREAL_PASS") or cfg.get("password"),
    }


class SurrealdbPlugin:
    name: str = "surrealdb"

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._client: Any = None
        self._run_id: str | None = None
        self._store_raw: bool = False

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        with self._lock:
            self._client = self._open_client(context.runtime_config or {})
            self._ensure_schema()
            self._store_raw = self._resolve_store_raw(context.runtime_config or {})
            self._run_id = context.run_id
            self._client.query(
                "CREATE runs CONTENT $data",
                {
                    "data": {
                        "run_id": context.run_id,
                        "orchestration_path": str(context.orchestration_path),
                        "status": "running",
                        "started_at": now_iso(),
                        "ended_at": None,
                        "error": None,
                        "inputs": extract_inputs(state),
                    }
                },
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
            raw = meta.get("raw") if self._store_raw else None
            error = meta.get("error")
            status = "failed" if error else "success"
            etype = infer_effect_type(meta, effect_result)
            try:
                self._client.query(
                    "CREATE effects CONTENT $data",
                    {
                        "data": {
                            "run_id": context.run_id,
                            "state_path": effect_path,
                            "effect_name": effect_name,
                            "effect_type": etype,
                            "parent_path": parent_path,
                            "iteration_index": iter_index,
                            "value": value,
                            "raw": raw,
                            "tokens_sent": meta.get("tokens_sent"),
                            "tokens_received": meta.get("tokens_received"),
                            "started_at": meta.get("created_at") or now_iso(),
                            "ended_at": meta.get("completed_at") or now_iso(),
                            "status": status,
                            "error": error,
                        }
                    },
                )
            except Exception as exc:
                logger.warning(
                    "surrealdb plugin failed to record effect %r: %s",
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
        if importlib.util.find_spec("surrealdb") is None:
            return CheckResult(
                ok=False,
                missing=["library:surrealdb"],
                message="pip install surrealdb",
            )
        return CheckResult(ok=True, missing=[])

    # ------------------------------------------------------------------

    def _open_client(self, runtime_config: dict[str, Any]) -> Any:
        from surrealdb import Surreal  # type: ignore[import-not-found]

        cfg = _resolve_config(runtime_config)
        client = Surreal(cfg["url"])
        if cfg["token"]:
            client.authenticate(cfg["token"])
        elif cfg["user"] and cfg["password"]:
            client.signin({"username": cfg["user"], "password": cfg["password"]})
        client.use(cfg["namespace"], cfg["database"])
        return client

    def _ensure_schema(self) -> None:
        self._client.query("DEFINE TABLE IF NOT EXISTS runs SCHEMALESS")
        self._client.query("DEFINE TABLE IF NOT EXISTS effects SCHEMALESS")
        self._client.query(
            "DEFINE INDEX IF NOT EXISTS idx_runs_run_id ON TABLE runs COLUMNS run_id UNIQUE"
        )
        self._client.query(
            "DEFINE INDEX IF NOT EXISTS idx_effects_run_id ON TABLE effects COLUMNS run_id"
        )

    def _resolve_store_raw(self, runtime_config: dict[str, Any]) -> bool:
        env = resolve_environment()
        env_raw = os.environ.get("CIRCUITRY_SURREALDB_STORE_RAW")
        cfg_section = (
            (runtime_config or {}).get("runtime_plugins", {}).get("surrealdb", {})
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
                self._client.query(
                    "UPDATE runs SET status = $status, ended_at = $ended_at, "
                    "error = $error WHERE run_id = $run_id",
                    {
                        "status": status,
                        "ended_at": now_iso(),
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


def plugin() -> SurrealdbPlugin:
    return SurrealdbPlugin()
