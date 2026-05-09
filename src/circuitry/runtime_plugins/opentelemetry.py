"""OpenTelemetry tracing runtime plugin.

Emits one span per orchestration run plus child spans per effect.
Spans carry attributes for ``run_id``, ``orchestration_path``, the
effect path, and any token counts present on the effect's meta.

Optional deps: ``opentelemetry-api`` and ``opentelemetry-sdk`` plus an
exporter — defaults to OTLP HTTP if ``OTEL_EXPORTER_OTLP_ENDPOINT`` is
set, else falls back to a console exporter (for local debugging).
Install with ``pip install circuitry-cof[opentelemetry]``.

Configuration is honored via the standard OTEL_* env vars
(``OTEL_EXPORTER_OTLP_ENDPOINT``, ``OTEL_SERVICE_NAME``, etc).
"""

from __future__ import annotations

import importlib.util
import logging
import os
import threading
from typing import Any

from ..preflight import CheckResult

logger = logging.getLogger(__name__)


class OpentelemetryPlugin:
    name: str = "opentelemetry"

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._tracer: Any = None
        self._provider: Any = None
        self._run_span: Any = None
        # effect_path → active span; populated when an effect starts
        # (we don't get a "start" hook for effects, only "complete",
        # so we open and immediately close the span on completion).
        self._effect_attrs: dict[str, Any] = {}

    def _check_dep(self) -> tuple[bool, list[str]]:
        try:
            api_present = importlib.util.find_spec("opentelemetry") is not None
            sdk_present = importlib.util.find_spec("opentelemetry.sdk") is not None
        except ModuleNotFoundError:
            api_present = sdk_present = False
        missing: list[str] = []
        if not api_present:
            missing.append("library:opentelemetry-api")
        if not sdk_present:
            missing.append("library:opentelemetry-sdk")
        return (not missing, missing)

    def _build_provider(self) -> Any:
        from opentelemetry.sdk.resources import Resource  # type: ignore[import-not-found]
        from opentelemetry.sdk.trace import TracerProvider  # type: ignore[import-not-found]
        from opentelemetry.sdk.trace.export import (  # type: ignore[import-not-found]
            BatchSpanProcessor,
            ConsoleSpanExporter,
        )

        service_name = os.environ.get("OTEL_SERVICE_NAME") or "circuitry"
        provider = TracerProvider(
            resource=Resource.create({"service.name": service_name})
        )

        otlp_endpoint = os.environ.get("OTEL_EXPORTER_OTLP_ENDPOINT")
        if otlp_endpoint:
            try:
                from opentelemetry.exporter.otlp.proto.http.trace_exporter import (  # type: ignore[import-not-found]
                    OTLPSpanExporter,
                )
                exporter = OTLPSpanExporter()
            except ImportError:
                logger.warning(
                    "opentelemetry: OTLP exporter unavailable, falling back to "
                    "console. Install opentelemetry-exporter-otlp-proto-http."
                )
                exporter = ConsoleSpanExporter()
        else:
            exporter = ConsoleSpanExporter()

        provider.add_span_processor(BatchSpanProcessor(exporter))
        return provider

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        del state
        with self._lock:
            from opentelemetry import trace  # type: ignore[import-not-found]

            self._provider = self._build_provider()
            tracer = trace.get_tracer("circuitry", tracer_provider=self._provider)
            self._tracer = tracer
            self._run_span = tracer.start_span(
                "circuitry.run",
                attributes={
                    "circuitry.run_id": context.run_id,
                    "circuitry.orchestration_path": str(context.orchestration_path),
                    "circuitry.dry_run": bool(context.dry_run),
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
            error = meta.get("error")
            if isinstance(error, str) and error:
                attrs["circuitry.error"] = error
            with self._tracer.start_as_current_span(
                f"effect:{effect_path}",
                attributes=attrs,
            ) as span:
                if error:
                    from opentelemetry.trace import Status, StatusCode  # type: ignore[import-not-found]
                    span.set_status(Status(StatusCode.ERROR, error))

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


def plugin() -> OpentelemetryPlugin:
    return OpentelemetryPlugin()
