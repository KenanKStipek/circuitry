"""Honeycomb runtime plugin — Honeycomb's OTLP ingest used as the
exporter for an OpenTelemetry tracer.

Honeycomb accepts OTLP traces directly; this plugin sets the OTLP
endpoint and the ``x-honeycomb-team`` header from environment, then
delegates to OpenTelemetry's tracer for span creation. The implementation
is intentionally close to ``opentelemetry`` plugin's so both end up
emitting the same span schema.

Optional deps: ``opentelemetry-api``, ``opentelemetry-sdk``,
``opentelemetry-exporter-otlp-proto-http``. Install with
``pip install circuitry-cof[honeycomb]``.

Required:
  - ``HONEYCOMB_API_KEY`` (or ``runtime_plugins.honeycomb.api_key``).

Optional:
  - ``HONEYCOMB_DATASET`` / ``runtime_plugins.honeycomb.dataset``
    (default ``"circuitry"``) — sets the ``service.name`` resource
    attribute.
  - ``HONEYCOMB_API_HOST`` / ``runtime_plugins.honeycomb.api_host``
    (default ``"https://api.honeycomb.io"``).
"""

from __future__ import annotations

import importlib.util
import logging
import os
import threading
from typing import Any

from ..preflight import CheckResult

logger = logging.getLogger(__name__)


class HoneycombPlugin:
    name: str = "honeycomb"

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._provider: Any = None
        self._tracer: Any = None
        self._run_span: Any = None

    def _check_dep(self) -> tuple[bool, list[str]]:
        missing: list[str] = []
        try:
            api = importlib.util.find_spec("opentelemetry") is not None
            sdk = importlib.util.find_spec("opentelemetry.sdk") is not None
            otlp = (
                importlib.util.find_spec(
                    "opentelemetry.exporter.otlp.proto.http.trace_exporter"
                ) is not None
            )
        except ModuleNotFoundError:
            api = sdk = otlp = False
        if not (api and sdk and otlp):
            missing.append("library:opentelemetry-exporter-otlp-proto-http")
        if not os.environ.get("HONEYCOMB_API_KEY"):
            missing.append("env:HONEYCOMB_API_KEY")
        return (not missing, missing)

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        del state
        with self._lock:
            try:
                from opentelemetry import trace  # type: ignore[import-not-found]
                from opentelemetry.exporter.otlp.proto.http.trace_exporter import (  # type: ignore[import-not-found]
                    OTLPSpanExporter,
                )
                from opentelemetry.sdk.resources import Resource  # type: ignore[import-not-found]
                from opentelemetry.sdk.trace import TracerProvider  # type: ignore[import-not-found]
                from opentelemetry.sdk.trace.export import (  # type: ignore[import-not-found]
                    BatchSpanProcessor,
                )
            except ImportError:
                logger.warning(
                    "honeycomb: opentelemetry deps missing; plugin is a no-op."
                )
                return

            cfg = (
                (context.runtime_config or {})
                .get("runtime_plugins", {})
                .get("honeycomb", {})
            )
            api_key = (
                os.environ.get("HONEYCOMB_API_KEY") or cfg.get("api_key")
            )
            if not api_key:
                logger.info("honeycomb: no API key; plugin is a no-op.")
                return
            dataset = (
                os.environ.get("HONEYCOMB_DATASET")
                or cfg.get("dataset")
                or "circuitry"
            )
            api_host = (
                os.environ.get("HONEYCOMB_API_HOST")
                or cfg.get("api_host")
                or "https://api.honeycomb.io"
            )

            exporter = OTLPSpanExporter(
                endpoint=f"{api_host}/v1/traces",
                headers={"x-honeycomb-team": api_key},
            )
            self._provider = TracerProvider(
                resource=Resource.create({"service.name": dataset})
            )
            self._provider.add_span_processor(BatchSpanProcessor(exporter))
            self._tracer = trace.get_tracer(
                "circuitry", tracer_provider=self._provider
            )
            self._run_span = self._tracer.start_span(
                "circuitry.run",
                attributes={
                    "circuitry.run_id": context.run_id,
                    "circuitry.orchestration_path": str(context.orchestration_path),
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
            if self._tracer is None:
                return
            meta = effect_result.get("meta") if isinstance(effect_result, dict) else {}
            meta = meta if isinstance(meta, dict) else {}
            attrs: dict[str, Any] = {
                "circuitry.run_id": context.run_id,
                "circuitry.effect_path": effect_path,
            }
            for key in ("tokens_sent", "tokens_received"):
                v = meta.get(key)
                if isinstance(v, int):
                    attrs[f"circuitry.{key}"] = v
            with self._tracer.start_as_current_span(
                f"effect:{effect_path}", attributes=attrs
            ):
                pass

    def on_run_success(self, *, state: dict[str, Any], context: Any) -> None:
        del state, context
        self._finalize(success=True, error=None)

    def on_run_failure(
        self, *, state: dict[str, Any], context: Any, error: str
    ) -> None:
        del state, context
        self._finalize(success=False, error=error)

    def check(self) -> CheckResult:
        ok, missing = self._check_dep()
        return CheckResult(ok=ok, missing=missing)

    def _finalize(self, *, success: bool, error: str | None) -> None:
        with self._lock:
            if self._run_span is not None:
                if not success:
                    from opentelemetry.trace import Status, StatusCode  # type: ignore[import-not-found]
                    self._run_span.set_status(
                        Status(StatusCode.ERROR, error or "run failed")
                    )
                self._run_span.end()
                self._run_span = None
            if self._provider is not None:
                try:
                    self._provider.shutdown()
                except Exception:
                    pass
                self._provider = None
                self._tracer = None


def plugin() -> HoneycombPlugin:
    return HoneycombPlugin()
