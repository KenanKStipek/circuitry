"""Shared base class for pub/sub runtime plugins.

Pub/sub plugins (kafka, rabbitmq, nats) publish lifecycle events as
JSON-encoded payloads to a topic / queue / subject. Unlike the
snapshot stores, they don't accumulate state on the broker — every
event is fire-and-forget.

Each event has a stable shape:

    {
        "event": "run_start" | "effect_complete" | "run_success" | "run_failure",
        "run_id": str,
        "orchestration_path": str,
        "timestamp": iso_8601_str,
        # plus event-specific fields:
        # effect_complete → effect_path, value, error
        # run_failure     → error
    }

Subclasses provide:
  * ``name`` (str)
  * ``_check_dep(self) -> tuple[bool, list[str]]``
  * ``_open_publisher(self, runtime_config) -> None`` — connect / set up
  * ``_publish(self, topic, payload_bytes) -> None``
  * ``_close_publisher(self) -> None``

Configuration:
  * ``runtime_plugins.<name>.topic`` (default ``"circuitry-runs"``)
  * ``runtime_plugins.<name>.publish_per_effect`` (bool, default True)
"""

from __future__ import annotations

import json
import logging
import threading
from datetime import datetime, timezone
from typing import Any

from ..preflight import CheckResult

logger = logging.getLogger(__name__)


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


class PubSubBase:
    """Base class for pub/sub lifecycle event emitters."""

    name: str = ""
    default_topic: str = "circuitry-runs"

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._topic: str = self.default_topic
        self._publish_per_effect: bool = True

    # ------------------------------------------------------------------
    # Subclass hooks
    # ------------------------------------------------------------------

    def _check_dep(self) -> tuple[bool, list[str]]:
        raise NotImplementedError

    def _open_publisher(self, runtime_config: dict[str, Any]) -> None:
        raise NotImplementedError

    def _publish(self, topic: str, payload: bytes) -> None:
        raise NotImplementedError

    def _close_publisher(self) -> None:
        return None

    # ------------------------------------------------------------------
    # Configuration
    # ------------------------------------------------------------------

    def _resolve_config(self, runtime_config: dict[str, Any]) -> None:
        cfg = (
            (runtime_config or {}).get("runtime_plugins", {}).get(self.name, {})
        )
        self._topic = str(cfg.get("topic") or self.default_topic)
        self._publish_per_effect = bool(cfg.get("publish_per_effect", True))

    # ------------------------------------------------------------------
    # Lifecycle hooks
    # ------------------------------------------------------------------

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        del state
        with self._lock:
            self._resolve_config(context.runtime_config or {})
            self._open_publisher(context.runtime_config or {})
            self._emit({
                "event": "run_start",
                "run_id": context.run_id,
                "orchestration_path": str(context.orchestration_path),
                "timestamp": _now_iso(),
            })

    def on_effect_complete(
        self,
        *,
        state: dict[str, Any],
        context: Any,
        effect_path: str,
        effect_result: dict[str, Any],
    ) -> None:
        del state
        if not self._publish_per_effect:
            return
        with self._lock:
            meta = effect_result.get("meta") if isinstance(effect_result, dict) else {}
            meta = meta if isinstance(meta, dict) else {}
            self._emit({
                "event": "effect_complete",
                "run_id": context.run_id,
                "effect_path": effect_path,
                "value": effect_result.get("value") if isinstance(effect_result, dict) else None,
                "error": meta.get("error"),
                "timestamp": _now_iso(),
            })

    def on_run_success(self, *, state: dict[str, Any], context: Any) -> None:
        del state
        with self._lock:
            self._emit({
                "event": "run_success",
                "run_id": context.run_id,
                "timestamp": _now_iso(),
            })
            self._close_publisher()

    def on_run_failure(
        self, *, state: dict[str, Any], context: Any, error: str
    ) -> None:
        del state
        with self._lock:
            self._emit({
                "event": "run_failure",
                "run_id": context.run_id,
                "error": error,
                "timestamp": _now_iso(),
            })
            self._close_publisher()

    def check(self) -> CheckResult:
        ok, missing = self._check_dep()
        if not ok:
            return CheckResult(ok=False, missing=missing)
        return CheckResult(ok=True, missing=[])

    # ------------------------------------------------------------------

    def _emit(self, payload: dict[str, Any]) -> None:
        body = json.dumps(payload, default=str).encode("utf-8")
        self._publish(self._topic, body)
