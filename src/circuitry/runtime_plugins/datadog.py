"""Datadog runtime plugin via the ``datadog`` Python SDK.

Emits one count metric per lifecycle event:
  - ``circuitry.runs.started``
  - ``circuitry.effects.completed``
  - ``circuitry.runs.succeeded`` / ``circuitry.runs.failed``

Plus a histogram for token counts when present:
  - ``circuitry.tokens.sent`` / ``circuitry.tokens.received``

Optional dep: ``datadog``. Install with ``pip install circuitry-cof[datadog]``.

Auth: ``DD_API_KEY`` env (and optionally ``DD_APP_KEY``,
``DD_SITE`` — default ``datadoghq.com``).
"""

from __future__ import annotations

import importlib.util
import logging
import os
from typing import Any

from ..preflight import CheckResult

logger = logging.getLogger(__name__)


class DatadogPlugin:
    name: str = "datadog"

    def __init__(self) -> None:
        self._statsd: Any = None
        self._tags: list[str] = []

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("datadog") is None:
            return False, ["library:datadog"]
        if not os.environ.get("DD_API_KEY"):
            return False, ["env:DD_API_KEY"]
        return True, []

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        del state
        try:
            from datadog import initialize, statsd  # type: ignore[import-not-found]
        except ImportError:
            return
        api_key = os.environ.get("DD_API_KEY")
        if not api_key:
            logger.info("datadog: DD_API_KEY not set; plugin is a no-op.")
            return
        initialize(
            api_key=api_key,
            app_key=os.environ.get("DD_APP_KEY"),
        )
        self._statsd = statsd
        self._tags = [
            f"run_id:{context.run_id}",
            f"orchestration:{os.path.basename(str(context.orchestration_path))}",
        ]
        self._statsd.increment("circuitry.runs.started", tags=self._tags)

    def on_effect_complete(
        self,
        *,
        state: dict[str, Any],
        context: Any,
        effect_path: str,
        effect_result: dict[str, Any],
    ) -> None:
        del state, context
        if self._statsd is None:
            return
        meta = effect_result.get("meta") if isinstance(effect_result, dict) else {}
        meta = meta if isinstance(meta, dict) else {}
        tags = self._tags + [f"effect_path:{effect_path}"]
        self._statsd.increment("circuitry.effects.completed", tags=tags)
        for metric_name, key in (
            ("circuitry.tokens.sent", "tokens_sent"),
            ("circuitry.tokens.received", "tokens_received"),
        ):
            v = meta.get(key)
            if isinstance(v, int):
                self._statsd.histogram(metric_name, v, tags=tags)

    def on_run_success(self, *, state: dict[str, Any], context: Any) -> None:
        del state, context
        if self._statsd is None:
            return
        self._statsd.increment("circuitry.runs.succeeded", tags=self._tags)

    def on_run_failure(
        self, *, state: dict[str, Any], context: Any, error: str
    ) -> None:
        del state, context, error
        if self._statsd is None:
            return
        self._statsd.increment("circuitry.runs.failed", tags=self._tags)

    def check(self) -> CheckResult:
        ok, missing = self._check_dep()
        return CheckResult(ok=ok, missing=missing)


def plugin() -> DatadogPlugin:
    return DatadogPlugin()
