"""Prometheus runtime plugin via prometheus-client.

Exposes per-run lifecycle as Prometheus metrics. Supports two
registration modes:

  * **Pushgateway** (default when ``PROMETHEUS_PUSHGATEWAY`` is set):
    metrics are pushed to a Prometheus Pushgateway on each run
    finalize. Suitable for batch / cron-style runs that don't expose
    a persistent ``/metrics`` endpoint.

  * **Registry-only**: metrics are accumulated in the default registry
    so that an external ``prometheus_client.start_http_server`` (or a
    framework-managed scrape endpoint) can serve them.

Metrics emitted:
  - Counter ``circuitry_runs_total{status="started|succeeded|failed"}``.
  - Counter ``circuitry_effects_total``.
  - Histogram ``circuitry_tokens_sent`` / ``circuitry_tokens_received``.

Optional dep: ``prometheus-client``. Install with
``pip install circuitry-cof[prometheus]``.
"""

from __future__ import annotations

import importlib.util
import logging
import os
from typing import Any

from ..preflight import CheckResult

logger = logging.getLogger(__name__)


class PrometheusPlugin:
    name: str = "prometheus"

    def __init__(self) -> None:
        self._registry: Any = None
        self._counters: dict[str, Any] = {}
        self._histograms: dict[str, Any] = {}
        self._pushgateway: str | None = None
        self._job: str = "circuitry"
        self._labels: dict[str, str] = {}

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("prometheus_client") is None:
            return False, ["library:prometheus-client"]
        return True, []

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        del state
        try:
            from prometheus_client import (  # type: ignore[import-not-found]
                CollectorRegistry,
                Counter,
                Histogram,
            )
        except ImportError:
            return

        cfg = (
            (context.runtime_config or {})
            .get("runtime_plugins", {})
            .get("prometheus", {})
        )
        self._pushgateway = (
            os.environ.get("PROMETHEUS_PUSHGATEWAY") or cfg.get("pushgateway")
        )
        self._job = str(
            os.environ.get("PROMETHEUS_JOB") or cfg.get("job") or "circuitry"
        )
        # When using a pushgateway, use a fresh registry per-run so
        # we can push exactly this run's deltas. When not pushing,
        # use a process-wide registry the host can scrape.
        if self._pushgateway:
            self._registry = CollectorRegistry()
        else:
            from prometheus_client import REGISTRY  # type: ignore[import-not-found]
            self._registry = REGISTRY

        self._counters["runs"] = self._get_or_create_counter(
            Counter, "circuitry_runs_total",
            "Circuitry orchestration runs by status.",
            ("status",),
        )
        self._counters["effects"] = self._get_or_create_counter(
            Counter, "circuitry_effects_total",
            "Circuitry effects completed.",
            (),
        )
        self._histograms["tokens_sent"] = self._get_or_create_counter(
            Histogram, "circuitry_tokens_sent",
            "Tokens sent per effect.",
            (),
        )
        self._histograms["tokens_received"] = self._get_or_create_counter(
            Histogram, "circuitry_tokens_received",
            "Tokens received per effect.",
            (),
        )
        self._labels = {"run_id": context.run_id}
        self._counters["runs"].labels(status="started").inc()

    def _get_or_create_counter(
        self, factory: Any, name: str, doc: str, labels: tuple[str, ...]
    ) -> Any:
        # Reuse the metric if it's already registered (process-wide
        # registry) — registering twice raises ValueError.
        try:
            kwargs: dict[str, Any] = {"registry": self._registry}
            if labels:
                kwargs["labelnames"] = list(labels)
            return factory(name, doc, **kwargs)
        except ValueError:
            # Already registered — pull it back out.
            existing = (
                self._registry._names_to_collectors.get(name)
                if hasattr(self._registry, "_names_to_collectors")
                else None
            )
            return existing

    def on_effect_complete(
        self,
        *,
        state: dict[str, Any],
        context: Any,
        effect_path: str,
        effect_result: dict[str, Any],
    ) -> None:
        del state, context, effect_path
        if not self._counters:
            return
        meta = effect_result.get("meta") if isinstance(effect_result, dict) else {}
        meta = meta if isinstance(meta, dict) else {}
        self._counters["effects"].inc()
        for key, hist_key in (
            ("tokens_sent", "tokens_sent"),
            ("tokens_received", "tokens_received"),
        ):
            v = meta.get(key)
            if isinstance(v, int):
                self._histograms[hist_key].observe(v)

    def on_run_success(self, *, state: dict[str, Any], context: Any) -> None:
        del state, context
        self._finalize(status="succeeded")

    def on_run_failure(
        self, *, state: dict[str, Any], context: Any, error: str
    ) -> None:
        del state, context, error
        self._finalize(status="failed")

    def check(self) -> CheckResult:
        ok, missing = self._check_dep()
        return CheckResult(ok=ok, missing=missing)

    def _finalize(self, *, status: str) -> None:
        if not self._counters:
            return
        self._counters["runs"].labels(status=status).inc()
        if self._pushgateway:
            try:
                from prometheus_client import (
                    push_to_gateway,  # type: ignore[import-not-found]
                )
                push_to_gateway(
                    self._pushgateway, job=self._job, registry=self._registry
                )
            except Exception as exc:
                logger.warning("prometheus: pushgateway delivery failed: %s", exc)


def plugin() -> PrometheusPlugin:
    return PrometheusPlugin()
