"""Grafana Loki runtime plugin.

Pushes JSON-encoded lifecycle events to Loki's HTTP push endpoint at
``/loki/api/v1/push``. Each event becomes a log line tagged with
``run_id`` and ``event`` labels.

Optional dep: ``requests``. Install with ``pip install circuitry-cof[loki]``.

Required:
  - ``LOKI_URL`` env or ``runtime_plugins.loki.url`` (e.g.
    ``http://loki:3100``).

Optional:
  - ``LOKI_USER`` / ``LOKI_PASSWORD`` for basic auth.
  - ``runtime_plugins.loki.labels`` (dict) — additional static labels
    merged with the per-event labels.
"""

from __future__ import annotations

import importlib.util
import json
import logging
import os
import threading
import time
from datetime import datetime, timezone
from typing import Any

from ..preflight import CheckResult

logger = logging.getLogger(__name__)


def _now_ns() -> str:
    # Loki expects nanosecond-precision timestamps as strings.
    return str(int(time.time() * 1e9))


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


class LokiPlugin:
    name: str = "loki"

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._url: str = ""
        self._auth: tuple[str, str] | None = None
        self._static_labels: dict[str, str] = {}
        self._timeout_seconds: int = 5

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("requests") is None:
            return False, ["library:requests"]
        return True, []

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        del state
        cfg = (
            (context.runtime_config or {})
            .get("runtime_plugins", {})
            .get("loki", {})
        )
        base_url = (os.environ.get("LOKI_URL") or cfg.get("url") or "").rstrip("/")
        if not base_url:
            logger.info("loki: no LOKI_URL configured; plugin is a no-op.")
            self._url = ""
            return
        self._url = f"{base_url}/loki/api/v1/push"
        user = os.environ.get("LOKI_USER") or cfg.get("user")
        password = os.environ.get("LOKI_PASSWORD") or cfg.get("password")
        self._auth = (user, password) if user and password else None
        labels = cfg.get("labels") or {}
        if not isinstance(labels, dict):
            labels = {}
        self._static_labels = {str(k): str(v) for k, v in labels.items()}
        self._static_labels.setdefault("service", "circuitry")
        self._post(
            event="run_start",
            run_id=context.run_id,
            payload={
                "event": "run_start",
                "run_id": context.run_id,
                "orchestration_path": str(context.orchestration_path),
                "timestamp": _now_iso(),
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
        if not self._url:
            return
        meta = effect_result.get("meta") if isinstance(effect_result, dict) else {}
        meta = meta if isinstance(meta, dict) else {}
        self._post(
            event="effect_complete",
            run_id=context.run_id,
            payload={
                "event": "effect_complete",
                "run_id": context.run_id,
                "effect_path": effect_path,
                "tokens_sent": meta.get("tokens_sent"),
                "tokens_received": meta.get("tokens_received"),
                "error": meta.get("error"),
                "timestamp": _now_iso(),
            },
        )

    def on_run_success(self, *, state: dict[str, Any], context: Any) -> None:
        del state
        if not self._url:
            return
        self._post(
            event="run_success",
            run_id=context.run_id,
            payload={
                "event": "run_success",
                "run_id": context.run_id,
                "timestamp": _now_iso(),
            },
        )

    def on_run_failure(
        self, *, state: dict[str, Any], context: Any, error: str
    ) -> None:
        del state
        if not self._url:
            return
        self._post(
            event="run_failure",
            run_id=context.run_id,
            payload={
                "event": "run_failure",
                "run_id": context.run_id,
                "error": error,
                "timestamp": _now_iso(),
            },
        )

    def check(self) -> CheckResult:
        ok, missing = self._check_dep()
        return CheckResult(ok=ok, missing=missing)

    def _post(
        self, *, event: str, run_id: str, payload: dict[str, Any]
    ) -> None:
        with self._lock:
            try:
                import requests  # type: ignore[import-not-found]
            except ImportError:
                return
            labels = dict(self._static_labels)
            labels["event"] = event
            labels["run_id"] = run_id
            stream = {
                "stream": labels,
                "values": [[_now_ns(), json.dumps(payload, default=str)]],
            }
            try:
                kwargs: dict[str, Any] = {
                    "json": {"streams": [stream]},
                    "timeout": self._timeout_seconds,
                }
                if self._auth is not None:
                    kwargs["auth"] = self._auth
                requests.post(self._url, **kwargs)
            except Exception as exc:
                logger.warning("loki: push failed: %s", exc)


def plugin() -> LokiPlugin:
    return LokiPlugin()
