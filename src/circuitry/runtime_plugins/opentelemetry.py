"""OpenTelemetry tracing runtime plugin.

Emits one span per orchestration run plus one child span per effect,
nested along the same dotted state path every other runtime plugin and
``--live-state`` already key off — a loop's or ``use``'s own span is the
parent of everything its body runs, exactly as the orchestration nests.
Spans carry attributes for ``run_id``, ``orchestration_path``, the effect
path, adapter/model, and any token counts present on the effect's meta.

Span start/end come from the effect's own ``meta.created_at`` /
``meta.completed_at`` — recorded when the effect actually started and
landed — not from whenever this plugin happens to be invoked, so a span's
duration is the effect's real wall time, not zero.

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
from datetime import datetime
from typing import Any

from ..preflight import CheckResult

logger = logging.getLogger(__name__)


def _iso_to_ns(value: Any) -> int | None:
    """An ISO-8601 ``meta.created_at``/``completed_at`` timestamp as
    nanoseconds since the epoch — the unit ``Span.start()``/``.end()`` take.
    ``None`` for anything that isn't a parseable string, so the caller falls
    back to "now" the same way an un-timed span always has."""
    if not isinstance(value, str) or not value:
        return None
    try:
        dt = datetime.fromisoformat(value)
    except ValueError:
        return None
    return int(dt.timestamp() * 1_000_000_000)


class OpentelemetryPlugin:
    name: str = "opentelemetry"

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._tracer: Any = None
        self._provider: Any = None
        self._run_span: Any = None
        self._run_ctx: Any = None
        #: effect_path -> one (thread_id, span, context-with-that-span-
        #: current) entry per currently-open span at that path, for every
        #: effect whose start has fired but whose complete hasn't yet — a
        #: child effect's start looks up its nearest open ancestor here to
        #: parent itself under it (see ``_parent_context``). A list, not a
        #: single entry: an unnamed loop doesn't namespace its body's state
        #: path per pass (``loop.py``'s ``_execute_body`` only does that for
        #: a *named* loop), so concurrent tree-flow branches can report the
        #: exact same ``effect_path`` at once; without this, the second
        #: branch's ``on_effect_start`` would overwrite the first's entry
        #: and its `on_effect_complete`` would then end the wrong branch's
        #: span (#331 finding 7). ``threading.get_ident()`` disambiguates
        #: which entry belongs to which completing call — a span's own
        #: start and complete always fire on the same thread, whatever else
        #: is running concurrently.
        self._spans: dict[str, list[tuple[int, Any, Any]]] = {}

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
        from opentelemetry.sdk.resources import (
            Resource,  # type: ignore[import-not-found]
        )
        from opentelemetry.sdk.trace import (
            TracerProvider,  # type: ignore[import-not-found]
        )
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

    def _parent_context(self, effect_path: str) -> Any:
        """The context of *effect_path*'s nearest still-open ancestor span,
        or the run span's context when none is open (a top-level effect).

        Walks dotted-path prefixes outside-in rather than just dropping the
        last segment: an each/while loop's own span lives at ``prime.shots``,
        but its body's effects are nested one level deeper under
        ``prime.shots.iter_<n>`` — a path segment that never gets its own
        start/complete event (only named effects do) and so is never a key
        in ``_spans``. Skipping straight to it would miss the loop entirely
        and parent every pass's effects under the run instead.
        """
        parts = effect_path.split(".")
        for i in range(len(parts) - 1, 0, -1):
            entries = self._spans.get(".".join(parts[:i]))
            if entries:
                # An ancestor container may itself be running one branch per
                # thread (a tree loop/dynamic) — its own span was opened on
                # whichever thread drives that container, not necessarily
                # this child's thread, so this can't be a thread-id lookup
                # like the exact-path push/pop below. Any still-open entry
                # is a correct parent; the most recently opened one is the
                # closest guess when more than one is open (an unnamed
                # container reused across concurrent branches, #331 finding
                # 7 — inherently ambiguous without a name to tell them apart).
                return entries[-1][2]
        return self._run_ctx

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
            self._run_ctx = trace.set_span_in_context(self._run_span)
            self._spans = {}

    def on_effect_start(
        self,
        *,
        state: dict[str, Any],
        context: Any,
        effect_path: str,
        effect_node: dict[str, Any],
    ) -> None:
        del state
        with self._lock:
            if self._tracer is None:
                return
            from opentelemetry import trace  # type: ignore[import-not-found]

            meta = effect_node.get("meta") if isinstance(effect_node, dict) else {}
            meta = meta if isinstance(meta, dict) else {}
            attrs: dict[str, Any] = {
                "circuitry.run_id": context.run_id,
                "circuitry.effect_path": effect_path,
            }
            adapter = meta.get("adapter")
            if isinstance(adapter, str) and adapter:
                attrs["circuitry.adapter"] = adapter
            model = meta.get("model")
            if isinstance(model, str) and model:
                attrs["circuitry.model"] = model
            flow = meta.get("flow") or meta.get("mode")
            if isinstance(flow, str) and flow:
                attrs["circuitry.flow"] = flow
            provider = meta.get("provider")
            if isinstance(provider, str) and provider:
                attrs["circuitry.provider"] = provider
            span = self._tracer.start_span(
                f"effect:{effect_path}",
                context=self._parent_context(effect_path),
                attributes=attrs,
                start_time=_iso_to_ns(meta.get("created_at")),
            )
            self._spans.setdefault(effect_path, []).append(
                (threading.get_ident(), span, trace.set_span_in_context(span))
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
            end_ns = _iso_to_ns(meta.get("completed_at"))

            span = self._pop_own_span(effect_path)
            if span is None:
                # No matching on_effect_start — a plugin attached mid-run,
                # or a caller that only wires on_effect_complete (some test
                # harnesses). Degrade to a span with whatever timing meta
                # has rather than dropping the event.
                attrs = {
                    "circuitry.run_id": context.run_id,
                    "circuitry.effect_path": effect_path,
                }
                provider = meta.get("provider")
                if isinstance(provider, str) and provider:
                    attrs["circuitry.provider"] = provider
                span = self._tracer.start_span(
                    f"effect:{effect_path}",
                    context=self._parent_context(effect_path),
                    attributes=attrs,
                    start_time=_iso_to_ns(meta.get("created_at")) or end_ns,
                )

            for key in ("tokens_sent", "tokens_received"):
                v = meta.get(key)
                if isinstance(v, int) and not isinstance(v, bool):
                    span.set_attribute(f"circuitry.{key}", v)
            error = meta.get("error")
            if isinstance(error, str) and error:
                from opentelemetry.trace import (  # type: ignore[import-not-found]
                    Status,
                    StatusCode,
                )
                span.set_status(Status(StatusCode.ERROR, error))
                span.set_attribute("circuitry.error", error)
            span.end(end_time=end_ns)

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

    def _pop_own_span(self, effect_path: str) -> Any | None:
        """Remove and return *this call's own* span at *effect_path* — the
        entry whose start fired on this same thread, not any other
        concurrent branch's entry sharing the same (unnamed-container)
        path (#331 finding 7)."""
        entries = self._spans.get(effect_path)
        if not entries:
            return None
        ident = threading.get_ident()
        for i, (thread_id, span, _ctx) in enumerate(entries):
            if thread_id == ident:
                entries.pop(i)
                if not entries:
                    del self._spans[effect_path]
                return span
        return None

    def _finalize(self, *, success: bool, error: str | None) -> None:
        with self._lock:
            # Any span whose complete never fired (a run that crashed mid-
            # effect) still gets closed, so the exporter doesn't hold it open
            # forever — best-effort "now" for its end time.
            for entries in self._spans.values():
                for _thread_id, _span, _ctx in entries:
                    _span.end()
            self._spans = {}
            if self._run_span is not None:
                if not success:
                    from opentelemetry.trace import (  # type: ignore[import-not-found]
                        Status,
                        StatusCode,
                    )
                    self._run_span.set_status(
                        Status(StatusCode.ERROR, error or "run failed")
                    )
                self._run_span.end()
                self._run_span = None
                self._run_ctx = None
            if self._provider is not None:
                try:
                    self._provider.shutdown()
                except Exception:
                    pass
                self._provider = None
                self._tracer = None


def plugin() -> OpentelemetryPlugin:
    return OpentelemetryPlugin()
