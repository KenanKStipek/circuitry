"""Shared base class for snapshot-style persistence runtime plugins.

Document stores (mongodb, couchdb, firestore, dynamodb, elasticsearch,
opensearch), key-value stores (redis, memcached), and object stores
(s3, gcs, azure-blob, r2) all follow the same pattern: serialise the
entire run state as a single record keyed by ``run_id``, write it on
lifecycle events (``on_run_start`` / ``on_run_success`` /
``on_run_failure``), and optionally also on each
``on_effect_complete`` for live-monitoring use cases.

Each subclass supplies:
  * ``name`` (str class attribute)
  * ``_check_dep(self) -> tuple[bool, list[str]]``
  * ``_init_run(self, context: PluginContext) -> None`` — open the
    client / set up handles. Called once at ``on_run_start``.
  * ``_upsert_snapshot(self, run_id: str, snapshot: dict) -> None``
  * ``_teardown(self) -> None`` — release handles. Called at
    ``on_run_success`` / ``on_run_failure``.

Per-plugin config is read from
``runtime.runtime_plugins.<name>.<key>``; the standard knobs are:

  * ``update_per_effect`` (bool, default False): when True, also
    upsert the snapshot on every ``on_effect_complete``. Useful for
    live-monitoring at the cost of write volume.
"""

from __future__ import annotations

import logging
import threading
from datetime import datetime, timezone
from typing import Any

from ..preflight import CheckResult

logger = logging.getLogger(__name__)


def now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


class SnapshotPersistenceBase:
    """Base class for full-state-snapshot persistence plugins."""

    name: str = ""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._started_at: str | None = None
        self._update_per_effect: bool = False

    # ------------------------------------------------------------------
    # Subclass hooks
    # ------------------------------------------------------------------

    def _check_dep(self) -> tuple[bool, list[str]]:
        raise NotImplementedError

    def _init_run(self, context: Any) -> None:
        """Open any per-run handles. Called from on_run_start before the
        first upsert."""

    def _upsert_snapshot(self, run_id: str, snapshot: dict[str, Any]) -> None:
        raise NotImplementedError

    def _teardown(self) -> None:
        """Release handles. Called once at run completion."""

    # ------------------------------------------------------------------
    # Configuration
    # ------------------------------------------------------------------

    def _resolve_update_per_effect(self, runtime_config: dict[str, Any]) -> bool:
        cfg = (
            (runtime_config or {}).get("runtime_plugins", {}).get(self.name, {})
        )
        return bool(cfg.get("update_per_effect", False))

    # ------------------------------------------------------------------
    # Lifecycle hooks
    # ------------------------------------------------------------------

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        with self._lock:
            self._started_at = now_iso()
            self._update_per_effect = self._resolve_update_per_effect(
                context.runtime_config or {}
            )
            self._init_run(context)
            snapshot = self._build_snapshot(
                state=state, context=context, status="running",
                ended_at=None, error=None,
            )
            self._upsert_snapshot(context.run_id, snapshot)

    def on_effect_complete(
        self,
        *,
        state: dict[str, Any],
        context: Any,
        effect_path: str,
        effect_result: dict[str, Any],
    ) -> None:
        del effect_path, effect_result
        if not self._update_per_effect:
            return
        with self._lock:
            snapshot = self._build_snapshot(
                state=state, context=context, status="running",
                ended_at=None, error=None,
            )
            self._upsert_snapshot(context.run_id, snapshot)

    def on_run_success(self, *, state: dict[str, Any], context: Any) -> None:
        with self._lock:
            snapshot = self._build_snapshot(
                state=state, context=context, status="success",
                ended_at=now_iso(), error=None,
            )
            try:
                self._upsert_snapshot(context.run_id, snapshot)
            finally:
                self._teardown()

    def on_run_failure(
        self, *, state: dict[str, Any], context: Any, error: str
    ) -> None:
        with self._lock:
            snapshot = self._build_snapshot(
                state=state, context=context, status="failed",
                ended_at=now_iso(), error=error,
            )
            try:
                self._upsert_snapshot(context.run_id, snapshot)
            finally:
                self._teardown()

    def check(self) -> CheckResult:
        ok, missing = self._check_dep()
        if not ok:
            return CheckResult(ok=False, missing=missing)
        return CheckResult(ok=True, missing=[])

    # ------------------------------------------------------------------
    # Helpers
    # ------------------------------------------------------------------

    def _build_snapshot(
        self,
        *,
        state: dict[str, Any],
        context: Any,
        status: str,
        ended_at: str | None,
        error: str | None,
    ) -> dict[str, Any]:
        return {
            "run_id": context.run_id,
            "orchestration_path": str(context.orchestration_path),
            "status": status,
            "started_at": self._started_at or now_iso(),
            "ended_at": ended_at,
            "error": error,
            "state": state,
        }
