"""AWS CloudWatch Logs runtime plugin via boto3.

Pushes JSON-encoded lifecycle events as log lines to a configured
CloudWatch Logs log group / stream. The plugin lazily creates the log
stream within the existing log group; the group itself must be
pre-created (group lifecycle is an infrastructure concern).

Optional dep: ``boto3``. Install with ``pip install circuitry-cof[cloudwatch]``.

Auth: standard AWS credentials chain.

Required:
  - ``CLOUDWATCH_LOG_GROUP`` / ``runtime_plugins.cloudwatch.log_group``.

Optional:
  - ``CLOUDWATCH_REGION`` / ``runtime_plugins.cloudwatch.region``.
  - ``runtime_plugins.cloudwatch.log_stream`` (default
    ``"circuitry-{run_id}"``).
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


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


class CloudwatchPlugin:
    name: str = "cloudwatch"

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._client: Any = None
        self._log_group: str = ""
        self._log_stream: str = ""
        self._sequence_token: str | None = None

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("boto3") is None:
            return False, ["library:boto3"]
        return True, []

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        del state
        try:
            import boto3  # type: ignore[import-not-found]
        except ImportError:
            return
        cfg = (
            (context.runtime_config or {})
            .get("runtime_plugins", {})
            .get("cloudwatch", {})
        )
        log_group = (
            os.environ.get("CLOUDWATCH_LOG_GROUP") or cfg.get("log_group")
        )
        if not log_group:
            logger.info("cloudwatch: no log group configured; plugin is a no-op.")
            return
        kwargs: dict[str, Any] = {}
        region = os.environ.get("CLOUDWATCH_REGION") or cfg.get("region")
        if region:
            kwargs["region_name"] = region
        self._client = boto3.client("logs", **kwargs)
        self._log_group = log_group
        self._log_stream = (
            cfg.get("log_stream") or f"circuitry-{context.run_id}"
        )
        try:
            self._client.create_log_stream(
                logGroupName=self._log_group, logStreamName=self._log_stream
            )
        except Exception:
            # ResourceAlreadyExistsException — re-using an existing stream.
            pass

        self._put_event({
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
        if self._client is None:
            return
        meta = effect_result.get("meta") if isinstance(effect_result, dict) else {}
        meta = meta if isinstance(meta, dict) else {}
        self._put_event({
            "event": "effect_complete",
            "run_id": context.run_id,
            "effect_path": effect_path,
            "tokens_sent": meta.get("tokens_sent"),
            "tokens_received": meta.get("tokens_received"),
            "error": meta.get("error"),
            "timestamp": _now_iso(),
        })

    def on_run_success(self, *, state: dict[str, Any], context: Any) -> None:
        del state
        if self._client is None:
            return
        self._put_event({
            "event": "run_success",
            "run_id": context.run_id,
            "timestamp": _now_iso(),
        })
        self._client = None

    def on_run_failure(
        self, *, state: dict[str, Any], context: Any, error: str
    ) -> None:
        del state
        if self._client is None:
            return
        self._put_event({
            "event": "run_failure",
            "run_id": context.run_id,
            "error": error,
            "timestamp": _now_iso(),
        })
        self._client = None

    def check(self) -> CheckResult:
        ok, missing = self._check_dep()
        return CheckResult(ok=ok, missing=missing)

    def _put_event(self, payload: dict[str, Any]) -> None:
        with self._lock:
            event = {
                "timestamp": int(time.time() * 1000),
                "message": json.dumps(payload, default=str),
            }
            kwargs: dict[str, Any] = {
                "logGroupName": self._log_group,
                "logStreamName": self._log_stream,
                "logEvents": [event],
            }
            if self._sequence_token:
                kwargs["sequenceToken"] = self._sequence_token
            try:
                response = self._client.put_log_events(**kwargs)
                self._sequence_token = response.get("nextSequenceToken")
            except Exception as exc:
                logger.warning("cloudwatch: put_log_events failed: %s", exc)


def plugin() -> CloudwatchPlugin:
    return CloudwatchPlugin()
