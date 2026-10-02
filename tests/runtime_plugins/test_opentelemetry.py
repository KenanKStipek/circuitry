"""OpenTelemetry runtime plugin: real span timing and parenting (#271).

Uses the real ``opentelemetry-sdk`` with its ``InMemorySpanExporter`` rather
than a hand-faked tracer — the behaviour under test (a span's start/end
coming from the effect's own timestamps, and children nesting under their
parent's real span context) is exactly what a fake tracer's simplified
``start_span(name, attributes)`` signature can't exercise.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("opentelemetry.sdk")

from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import SimpleSpanProcessor
from opentelemetry.sdk.trace.export.in_memory_span_exporter import (
    InMemorySpanExporter,
)

from circuitry.core.runtime_plugins import PluginContext
from circuitry.runtime_plugins import opentelemetry as otel_mod

CREATED_AT = "2024-01-01T00:00:00+00:00"
COMPLETED_AT = "2024-01-01T00:00:05+00:00"  # 5s later


def _make_context(tmp_path: Path) -> PluginContext:
    return PluginContext(
        run_id="run-1",
        orchestration_path=tmp_path / "orch.yml",
        dry_run=False,
        validate_only=False,
        runtime_config={},
    )


@pytest.fixture
def plugin_with_exporter(
    monkeypatch: pytest.MonkeyPatch,
) -> tuple[otel_mod.OpentelemetryPlugin, InMemorySpanExporter]:
    """A real plugin wired to a real, synchronous, in-memory exporter —
    ``SimpleSpanProcessor`` exports on every ``span.end()``, not on a
    background batching timer, so spans are visible to the test the moment
    the plugin hook that ends them returns."""
    exporter = InMemorySpanExporter()

    def _build_provider(self: otel_mod.OpentelemetryPlugin) -> TracerProvider:
        del self
        provider = TracerProvider()
        provider.add_span_processor(SimpleSpanProcessor(exporter))
        return provider

    monkeypatch.setattr(otel_mod.OpentelemetryPlugin, "_build_provider", _build_provider)
    return otel_mod.plugin(), exporter


def test_span_duration_comes_from_effect_timestamps_not_call_time(
    plugin_with_exporter: tuple[otel_mod.OpentelemetryPlugin, InMemorySpanExporter],
    tmp_path: Path,
) -> None:
    """A span's duration is ``completed_at - created_at`` from meta, not
    however long the plugin hook call itself took (effectively zero)."""
    plugin, exporter = plugin_with_exporter
    ctx = _make_context(tmp_path)

    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_start(
        state={},
        context=ctx,
        effect_path="prime.greet",
        effect_node={"meta": {"created_at": CREATED_AT, "adapter": "noop", "model": "x"}},
    )
    plugin.on_effect_complete(
        state={},
        context=ctx,
        effect_path="prime.greet",
        effect_result={
            "meta": {
                "created_at": CREATED_AT,
                "completed_at": COMPLETED_AT,
                "tokens_sent": 10,
                "tokens_received": 5,
            }
        },
    )
    plugin.on_run_success(state={}, context=ctx)

    spans = {s.name: s for s in exporter.get_finished_spans()}
    effect_span = spans["effect:prime.greet"]
    duration_s = (effect_span.end_time - effect_span.start_time) / 1_000_000_000
    assert duration_s == pytest.approx(5.0, abs=0.01)
    assert effect_span.attributes["circuitry.adapter"] == "noop"
    assert effect_span.attributes["circuitry.model"] == "x"
    assert effect_span.attributes["circuitry.tokens_sent"] == 10
    assert effect_span.attributes["circuitry.tokens_received"] == 5


def test_loop_and_nested_effects_parent_under_the_run_and_each_other(
    plugin_with_exporter: tuple[otel_mod.OpentelemetryPlugin, InMemorySpanExporter],
    tmp_path: Path,
) -> None:
    """A loop's span is the run's child; a pass's body effect is the loop's
    child (nested under ``prime.shots.iter_0.step``, even though
    ``iter_0`` itself never gets its own start/complete event)."""
    plugin, exporter = plugin_with_exporter
    ctx = _make_context(tmp_path)

    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_start(
        state={},
        context=ctx,
        effect_path="prime.shots",
        effect_node={"meta": {"created_at": CREATED_AT, "mode": "each"}},
    )
    plugin.on_effect_start(
        state={},
        context=ctx,
        effect_path="prime.shots.iter_0.step",
        effect_node={"meta": {"created_at": CREATED_AT}},
    )
    plugin.on_effect_complete(
        state={},
        context=ctx,
        effect_path="prime.shots.iter_0.step",
        effect_result={"meta": {"created_at": CREATED_AT, "completed_at": COMPLETED_AT}},
    )
    plugin.on_effect_complete(
        state={},
        context=ctx,
        effect_path="prime.shots",
        effect_result={"meta": {"created_at": CREATED_AT, "completed_at": COMPLETED_AT}},
    )
    plugin.on_run_success(state={}, context=ctx)

    spans = {s.name: s for s in exporter.get_finished_spans()}
    run_span = spans["circuitry.run"]
    loop_span = spans["effect:prime.shots"]
    step_span = spans["effect:prime.shots.iter_0.step"]

    assert loop_span.parent is not None
    assert loop_span.parent.span_id == run_span.context.span_id
    assert step_span.parent is not None
    assert step_span.parent.span_id == loop_span.context.span_id


def test_error_sets_span_status_and_attribute(
    plugin_with_exporter: tuple[otel_mod.OpentelemetryPlugin, InMemorySpanExporter],
    tmp_path: Path,
) -> None:
    from opentelemetry.trace import StatusCode

    plugin, exporter = plugin_with_exporter
    ctx = _make_context(tmp_path)

    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_start(
        state={},
        context=ctx,
        effect_path="prime.step",
        effect_node={"meta": {"created_at": CREATED_AT}},
    )
    plugin.on_effect_complete(
        state={},
        context=ctx,
        effect_path="prime.step",
        effect_result={
            "meta": {
                "created_at": CREATED_AT,
                "completed_at": COMPLETED_AT,
                "error": "boom",
            }
        },
    )
    plugin.on_run_failure(state={}, context=ctx, error="boom")

    spans = {s.name: s for s in exporter.get_finished_spans()}
    step_span = spans["effect:prime.step"]
    assert step_span.status.status_code == StatusCode.ERROR
    assert step_span.attributes["circuitry.error"] == "boom"
    assert spans["circuitry.run"].status.status_code == StatusCode.ERROR


def test_effect_complete_without_a_prior_start_still_gets_a_span(
    plugin_with_exporter: tuple[otel_mod.OpentelemetryPlugin, InMemorySpanExporter],
    tmp_path: Path,
) -> None:
    """A caller that only wires ``on_effect_complete`` (or a plugin attached
    mid-run) still gets a span, not a dropped event."""
    plugin, exporter = plugin_with_exporter
    ctx = _make_context(tmp_path)

    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_complete(
        state={},
        context=ctx,
        effect_path="prime.greet",
        effect_result={"meta": {"created_at": CREATED_AT, "completed_at": COMPLETED_AT}},
    )
    plugin.on_run_success(state={}, context=ctx)

    spans = {s.name: s for s in exporter.get_finished_spans()}
    assert "effect:prime.greet" in spans


def test_check_reports_missing_dep(monkeypatch: pytest.MonkeyPatch) -> None:
    import importlib.util

    real_find_spec = importlib.util.find_spec

    def fake_find_spec(name: str, *args: Any, **kwargs: Any):
        if name == "opentelemetry.sdk":
            return None
        return real_find_spec(name, *args, **kwargs)

    monkeypatch.setattr(importlib.util, "find_spec", fake_find_spec)
    chk = otel_mod.plugin().check()
    assert chk.ok is False
    assert "library:opentelemetry-sdk" in chk.missing
