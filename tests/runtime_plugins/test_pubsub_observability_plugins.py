"""Tests for the pub/sub and observability runtime plugin cohort.

Pub/sub: kafka, rabbitmq, nats — share a SnapshotPersistenceBase
sibling (PubSubBase) that handles the lifecycle hook → JSON-encoded
event flow. Lifecycle is exercised through kafka with a fake Producer.

Observability: opentelemetry, sentry, datadog, honeycomb, prometheus,
loki, cloudwatch — each is its own plugin, tested for factory wiring,
check() with-and-without deps, and one representative happy-path call
via injected fake SDKs.
"""

from __future__ import annotations

import importlib.util
import json as _json
import sys
import types
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, ClassVar

import pytest

from circuitry.core.runtime_plugins import PluginContext, load_plugins
from circuitry.runtime_plugins import (
    cloudwatch as cloudwatch_mod,
)
from circuitry.runtime_plugins import (
    datadog as datadog_mod,
)
from circuitry.runtime_plugins import (
    honeycomb as honeycomb_mod,
)
from circuitry.runtime_plugins import (
    kafka as kafka_mod,
)
from circuitry.runtime_plugins import (
    loki as loki_mod,
)
from circuitry.runtime_plugins import (
    nats as nats_mod,
)
from circuitry.runtime_plugins import (
    opentelemetry as otel_mod,
)
from circuitry.runtime_plugins import (
    prometheus as prometheus_mod,
)
from circuitry.runtime_plugins import (
    rabbitmq as rabbitmq_mod,
)
from circuitry.runtime_plugins import (
    sentry as sentry_mod,
)

PUBSUB_PLUGINS = [
    ("kafka", "confluent_kafka", "confluent-kafka"),
    ("rabbitmq", "pika", "pika"),
    ("nats", "nats", "nats-py"),
]

OBSERVABILITY_PLUGINS = [
    ("sentry", "sentry_sdk", "sentry-sdk"),
    ("datadog", "datadog", "datadog"),
    ("prometheus", "prometheus_client", "prometheus-client"),
    ("cloudwatch", "boto3", "boto3"),
]


# ---------------------------------------------------------------------------
# Factory + check() reports missing dep
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "plugin_name,_dep_module,_dep_pypi", PUBSUB_PLUGINS + OBSERVABILITY_PLUGINS,
)
def test_factory_loads_each_plugin(
    plugin_name: str, _dep_module: str, _dep_pypi: str
) -> None:
    results = load_plugins([f"circuitry.runtime_plugins.{plugin_name}"])
    r = results[0]
    assert r.error is None, f"load failed: {r.error}"
    assert r.plugin is not None
    assert r.plugin.name == plugin_name


@pytest.mark.parametrize("plugin_name", ["opentelemetry", "honeycomb", "loki"])
def test_factory_loads_otel_loki(plugin_name: str) -> None:
    """opentelemetry / honeycomb / loki use multi-dep checks; verify
    that the plugin loads and check() runs without exploding."""
    results = load_plugins([f"circuitry.runtime_plugins.{plugin_name}"])
    r = results[0]
    assert r.error is None
    # Just verify check() returns a CheckResult with the right shape;
    # the actual ok/missing depends on which optional deps the test
    # environment happens to have installed.
    chk = r.plugin.check()
    assert isinstance(chk.missing, list)


def _force_missing(monkeypatch: pytest.MonkeyPatch, *modules: str) -> None:
    real = importlib.util.find_spec

    def fake(name: str, *args: Any, **kwargs: Any):
        if name in modules:
            return None
        return real(name, *args, **kwargs)

    monkeypatch.setattr("importlib.util.find_spec", fake)


@pytest.mark.parametrize("plugin_name,dep_module,dep_pypi", PUBSUB_PLUGINS)
def test_pubsub_check_reports_missing_dep(
    monkeypatch: pytest.MonkeyPatch,
    plugin_name: str,
    dep_module: str,
    dep_pypi: str,
) -> None:
    _force_missing(monkeypatch, dep_module)
    results = load_plugins([f"circuitry.runtime_plugins.{plugin_name}"])
    chk = results[0].plugin.check()
    assert chk.ok is False
    assert f"library:{dep_pypi}" in chk.missing


@pytest.mark.parametrize("plugin_name,dep_module,dep_pypi", OBSERVABILITY_PLUGINS)
def test_observability_check_reports_missing_dep(
    monkeypatch: pytest.MonkeyPatch,
    plugin_name: str,
    dep_module: str,
    dep_pypi: str,
) -> None:
    _force_missing(monkeypatch, dep_module)
    # Some observability plugins also report missing env vars; we only
    # verify the dep marker is present, not that it's the only missing
    # entry.
    if plugin_name in ("sentry", "datadog"):
        # Drop the env vars they look for so check() includes them too.
        for env in ("SENTRY_DSN", "DD_API_KEY"):
            monkeypatch.delenv(env, raising=False)
    results = load_plugins([f"circuitry.runtime_plugins.{plugin_name}"])
    chk = results[0].plugin.check()
    assert chk.ok is False
    assert f"library:{dep_pypi}" in chk.missing


# ---------------------------------------------------------------------------
# Pub/sub lifecycle (kafka with fake Producer)
# ---------------------------------------------------------------------------


@dataclass
class _FakeKafkaProducer:
    produced: list[tuple[str, bytes]] = field(default_factory=list)
    polled: int = 0
    flushed: int = 0

    def produce(self, topic: str, value: bytes) -> None:
        self.produced.append((topic, value))

    def poll(self, _timeout: float) -> None:
        self.polled += 1

    def flush(self, timeout: float = 0) -> None:
        self.flushed += 1


def _install_fake_kafka(monkeypatch: pytest.MonkeyPatch) -> dict[str, Any]:
    fake = types.ModuleType("confluent_kafka")
    holder: dict[str, _FakeKafkaProducer] = {}

    def make_producer(config: dict) -> _FakeKafkaProducer:
        holder["p"] = _FakeKafkaProducer()
        holder["config"] = config  # type: ignore[assignment]
        return holder["p"]

    fake.Producer = make_producer
    monkeypatch.setitem(sys.modules, "confluent_kafka", fake)
    return holder


def _make_context(tmp_path: Path, plugin_name: str) -> PluginContext:
    return PluginContext(
        run_id="run-1",
        orchestration_path=tmp_path / "orch.yml",
        dry_run=False,
        validate_only=False,
        runtime_config={
            "runtime_plugins": {
                plugin_name: {"topic": "circuitry-events"},
            },
        },
    )


def test_kafka_lifecycle_publishes_each_event(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_kafka(monkeypatch)
    monkeypatch.setenv("KAFKA_BROKERS", "localhost:9092")

    plugin = kafka_mod.plugin()
    ctx = _make_context(tmp_path, "kafka")

    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_complete(
        state={}, context=ctx,
        effect_path="prime.greet",
        effect_result={"value": "hi", "meta": {}},
    )
    plugin.on_run_success(state={}, context=ctx)

    producer = holder["p"]
    assert len(producer.produced) == 3
    assert all(t == "circuitry-events" for t, _ in producer.produced)
    events = [_json.loads(body) for _, body in producer.produced]
    assert [e["event"] for e in events] == [
        "run_start", "effect_complete", "run_success",
    ]
    assert events[0]["run_id"] == "run-1"
    assert events[1]["effect_path"] == "prime.greet"
    assert producer.flushed >= 1


def test_kafka_requires_brokers(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _install_fake_kafka(monkeypatch)
    monkeypatch.delenv("KAFKA_BROKERS", raising=False)
    monkeypatch.delenv("KAFKA_BOOTSTRAP_SERVERS", raising=False)

    plugin = kafka_mod.plugin()
    ctx = PluginContext(
        run_id="r", orchestration_path=tmp_path / "x.yml",
        dry_run=False, validate_only=False, runtime_config={},
    )
    with pytest.raises(RuntimeError, match="kafka: brokers"):
        plugin.on_run_start(state={}, context=ctx)


def test_kafka_publish_per_effect_can_be_disabled(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_kafka(monkeypatch)
    monkeypatch.setenv("KAFKA_BROKERS", "localhost:9092")

    plugin = kafka_mod.plugin()
    ctx = PluginContext(
        run_id="run-1", orchestration_path=tmp_path / "x.yml",
        dry_run=False, validate_only=False,
        runtime_config={
            "runtime_plugins": {"kafka": {"publish_per_effect": False}},
        },
    )
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_complete(
        state={}, context=ctx, effect_path="prime.x", effect_result={},
    )
    plugin.on_run_success(state={}, context=ctx)
    events = [_json.loads(body) for _, body in holder["p"].produced]
    assert [e["event"] for e in events] == ["run_start", "run_success"]


def test_kafka_failure_event_carries_error(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_kafka(monkeypatch)
    monkeypatch.setenv("KAFKA_BROKERS", "localhost:9092")
    plugin = kafka_mod.plugin()
    ctx = _make_context(tmp_path, "kafka")
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_run_failure(state={}, context=ctx, error="boom")
    events = [_json.loads(body) for _, body in holder["p"].produced]
    assert events[-1]["event"] == "run_failure"
    assert events[-1]["error"] == "boom"


# ---------------------------------------------------------------------------
# RabbitMQ — fake pika channel
# ---------------------------------------------------------------------------


def test_rabbitmq_publishes_via_basic_publish(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake = types.ModuleType("pika")
    captured: dict[str, Any] = {"published": [], "queue_declared": False}

    class FakeChannel:
        is_closed = False

        def queue_declare(self, **kwargs: Any) -> None:
            captured["queue_declared"] = True
            captured["queue_name"] = kwargs.get("queue")

        def exchange_declare(self, **kwargs: Any) -> None:
            captured["exchange_declared"] = kwargs

        def basic_publish(self, **kwargs: Any) -> None:
            captured["published"].append(kwargs)

        def close(self) -> None:
            self.is_closed = True

    class FakeConn:
        is_open = True
        is_closed = False

        def __init__(self, params: Any) -> None:
            captured["params"] = params

        def channel(self) -> FakeChannel:
            return FakeChannel()

        def close(self) -> None:
            self.is_open = False

    fake.URLParameters = lambda url: {"url": url}
    fake.BlockingConnection = FakeConn
    monkeypatch.setitem(sys.modules, "pika", fake)
    monkeypatch.setenv("RABBITMQ_URL", "amqp://guest:guest@localhost:5672/")

    plugin = rabbitmq_mod.plugin()
    ctx = _make_context(tmp_path, "rabbitmq")
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_run_success(state={}, context=ctx)
    assert captured["queue_declared"] is True
    assert len(captured["published"]) == 2
    bodies = [_json.loads(p["body"]) for p in captured["published"]]
    assert [b["event"] for b in bodies] == ["run_start", "run_success"]


# ---------------------------------------------------------------------------
# NATS — exercises the asyncio loop helper
# ---------------------------------------------------------------------------


def test_nats_lifecycle_uses_async_publish(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake = types.ModuleType("nats")
    published: list[tuple[str, bytes]] = []

    class FakeClient:
        async def publish(self, subject: str, payload: bytes) -> None:
            published.append((subject, payload))

        async def flush(self, timeout: float = 5.0) -> None:
            return None

        async def drain(self) -> None:
            return None

    async def fake_connect(url: str) -> FakeClient:
        return FakeClient()

    fake.connect = fake_connect
    monkeypatch.setitem(sys.modules, "nats", fake)
    monkeypatch.setenv("NATS_URL", "nats://localhost:4222")

    plugin = nats_mod.plugin()
    ctx = _make_context(tmp_path, "nats")
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_run_success(state={}, context=ctx)
    assert len(published) >= 2
    events = [_json.loads(body) for _, body in published]
    assert "run_start" in [e["event"] for e in events]
    assert "run_success" in [e["event"] for e in events]


# ---------------------------------------------------------------------------
# OpenTelemetry — fake tracer provider
# ---------------------------------------------------------------------------


class _FakeSpan:
    def __init__(self, name: str, attributes: dict | None = None) -> None:
        self.name = name
        self.attributes = dict(attributes or {})
        self.ended = False
        self.status: Any = None

    def set_status(self, status: Any) -> None:
        self.status = status

    def end(self) -> None:
        self.ended = True

    def __enter__(self) -> _FakeSpan:
        return self

    def __exit__(self, *args: Any) -> None:
        self.end()


class _FakeTracer:
    def __init__(self) -> None:
        self.spans: list[_FakeSpan] = []

    def start_span(self, name: str, attributes: dict | None = None) -> _FakeSpan:
        span = _FakeSpan(name, attributes)
        self.spans.append(span)
        return span

    def start_as_current_span(self, name: str, attributes: dict | None = None) -> _FakeSpan:
        return self.start_span(name, attributes)


def _install_fake_otel(monkeypatch: pytest.MonkeyPatch) -> _FakeTracer:
    tracer = _FakeTracer()

    fake_api = types.ModuleType("opentelemetry")
    fake_trace = types.ModuleType("opentelemetry.trace")

    class FakeStatus:
        OK = "OK"
        ERROR = "ERROR"

        def __init__(self, code: Any, description: str = "") -> None:
            self.code = code
            self.description = description

    class FakeStatusCode:
        OK = "OK"
        ERROR = "ERROR"

    fake_trace.Status = FakeStatus
    fake_trace.StatusCode = FakeStatusCode

    def fake_get_tracer(name: str, *, tracer_provider: Any = None) -> _FakeTracer:
        return tracer

    fake_trace.get_tracer = fake_get_tracer
    fake_api.trace = fake_trace

    fake_sdk = types.ModuleType("opentelemetry.sdk")
    fake_resources = types.ModuleType("opentelemetry.sdk.resources")
    fake_resources.Resource = types.SimpleNamespace(
        create=lambda d: {"resource": d}
    )
    fake_sdk_trace = types.ModuleType("opentelemetry.sdk.trace")
    fake_sdk_export = types.ModuleType("opentelemetry.sdk.trace.export")

    class FakeProvider:
        def __init__(self, resource: Any) -> None:
            self._resource = resource
            self._processors: list[Any] = []
            self.shutdown_called = False

        def add_span_processor(self, p: Any) -> None:
            self._processors.append(p)

        def shutdown(self) -> None:
            self.shutdown_called = True

    fake_sdk_trace.TracerProvider = FakeProvider
    fake_sdk_export.BatchSpanProcessor = lambda exporter: ("batch", exporter)
    fake_sdk_export.ConsoleSpanExporter = lambda: "console"

    monkeypatch.setitem(sys.modules, "opentelemetry", fake_api)
    monkeypatch.setitem(sys.modules, "opentelemetry.trace", fake_trace)
    monkeypatch.setitem(sys.modules, "opentelemetry.sdk", fake_sdk)
    monkeypatch.setitem(
        sys.modules, "opentelemetry.sdk.resources", fake_resources
    )
    monkeypatch.setitem(sys.modules, "opentelemetry.sdk.trace", fake_sdk_trace)
    monkeypatch.setitem(
        sys.modules, "opentelemetry.sdk.trace.export", fake_sdk_export
    )
    return tracer


def test_opentelemetry_creates_run_span_and_per_effect_spans(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    tracer = _install_fake_otel(monkeypatch)
    plugin = otel_mod.plugin()
    ctx = _make_context(tmp_path, "opentelemetry")
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_complete(
        state={}, context=ctx,
        effect_path="prime.greet",
        effect_result={
            "value": "hi",
            "meta": {"tokens_sent": 10, "tokens_received": 5},
        },
    )
    plugin.on_run_success(state={}, context=ctx)
    span_names = [s.name for s in tracer.spans]
    assert "circuitry.run" in span_names
    assert "effect:prime.greet" in span_names
    run_span = next(s for s in tracer.spans if s.name == "circuitry.run")
    assert run_span.ended is True
    eff_span = next(s for s in tracer.spans if s.name == "effect:prime.greet")
    assert eff_span.attributes["circuitry.tokens_sent"] == 10


# ---------------------------------------------------------------------------
# Sentry
# ---------------------------------------------------------------------------


def test_sentry_captures_failure_message(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake = types.ModuleType("sentry_sdk")
    captured: dict[str, Any] = {"breadcrumbs": [], "messages": [], "tags": {}}

    fake.init = lambda **kwargs: captured.setdefault("init_kwargs", kwargs)
    fake.set_tag = lambda k, v: captured["tags"].update({k: v})
    fake.add_breadcrumb = lambda **kwargs: captured["breadcrumbs"].append(kwargs)
    fake.capture_message = lambda msg, level=None: captured["messages"].append(
        (msg, level)
    )
    monkeypatch.setitem(sys.modules, "sentry_sdk", fake)
    monkeypatch.setenv("SENTRY_DSN", "https://abc@sentry.example/1")

    plugin = sentry_mod.plugin()
    ctx = _make_context(tmp_path, "sentry")
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_run_failure(state={}, context=ctx, error="boom")
    assert captured["tags"]["circuitry.run_id"] == "run-1"
    assert any(m[0].startswith("circuitry run failed") for m in captured["messages"])


# ---------------------------------------------------------------------------
# Datadog
# ---------------------------------------------------------------------------


def test_datadog_emits_run_and_effect_metrics(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake = types.ModuleType("datadog")
    increments: list[tuple[str, list]] = []
    histograms: list[tuple[str, float, list]] = []

    class FakeStatsd:
        def increment(self, metric: str, tags: list | None = None) -> None:
            increments.append((metric, list(tags or [])))

        def histogram(self, metric: str, value: float, tags: list | None = None) -> None:
            histograms.append((metric, value, list(tags or [])))

    fake.initialize = lambda **k: None
    fake.statsd = FakeStatsd()
    monkeypatch.setitem(sys.modules, "datadog", fake)
    monkeypatch.setenv("DD_API_KEY", "abc")

    plugin = datadog_mod.plugin()
    ctx = _make_context(tmp_path, "datadog")
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_complete(
        state={}, context=ctx,
        effect_path="prime.greet",
        effect_result={"value": "hi", "meta": {"tokens_sent": 10}},
    )
    plugin.on_run_success(state={}, context=ctx)

    metric_names = [m for m, _ in increments]
    assert "circuitry.runs.started" in metric_names
    assert "circuitry.effects.completed" in metric_names
    assert "circuitry.runs.succeeded" in metric_names
    assert any(h[0] == "circuitry.tokens.sent" and h[1] == 10 for h in histograms)


# ---------------------------------------------------------------------------
# Honeycomb
# ---------------------------------------------------------------------------


def test_honeycomb_includes_team_header(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    captured_init: dict[str, Any] = {}

    fake_api = types.ModuleType("opentelemetry")
    fake_trace = types.ModuleType("opentelemetry.trace")
    tracer = _FakeTracer()

    class FakeStatus:
        def __init__(self, code: Any, description: str = "") -> None:
            self.code = code

    class FakeStatusCode:
        ERROR = "ERROR"

    fake_trace.get_tracer = lambda name, tracer_provider=None: tracer
    fake_trace.Status = FakeStatus
    fake_trace.StatusCode = FakeStatusCode
    fake_api.trace = fake_trace

    fake_sdk = types.ModuleType("opentelemetry.sdk")
    fake_resources = types.ModuleType("opentelemetry.sdk.resources")
    fake_resources.Resource = types.SimpleNamespace(create=lambda d: d)
    fake_sdk_trace = types.ModuleType("opentelemetry.sdk.trace")
    fake_sdk_export = types.ModuleType("opentelemetry.sdk.trace.export")

    class FakeProvider:
        def __init__(self, resource: Any) -> None:
            captured_init["resource"] = resource

        def add_span_processor(self, p: Any) -> None:
            captured_init["processor"] = p

        def shutdown(self) -> None:
            captured_init["shutdown"] = True

    fake_sdk_trace.TracerProvider = FakeProvider
    fake_sdk_export.BatchSpanProcessor = lambda exporter: exporter
    fake_otlp_pkg = types.ModuleType("opentelemetry.exporter")
    fake_otlp_pkg2 = types.ModuleType("opentelemetry.exporter.otlp")
    fake_otlp_pkg3 = types.ModuleType("opentelemetry.exporter.otlp.proto")
    fake_otlp_pkg4 = types.ModuleType("opentelemetry.exporter.otlp.proto.http")
    fake_otlp_te = types.ModuleType(
        "opentelemetry.exporter.otlp.proto.http.trace_exporter"
    )

    def fake_otlp_exporter(*, endpoint: str, headers: dict | None = None) -> dict:
        captured_init["endpoint"] = endpoint
        captured_init["headers"] = dict(headers or {})
        return {"endpoint": endpoint, "headers": headers}

    fake_otlp_te.OTLPSpanExporter = fake_otlp_exporter

    for name, mod in (
        ("opentelemetry", fake_api),
        ("opentelemetry.trace", fake_trace),
        ("opentelemetry.sdk", fake_sdk),
        ("opentelemetry.sdk.resources", fake_resources),
        ("opentelemetry.sdk.trace", fake_sdk_trace),
        ("opentelemetry.sdk.trace.export", fake_sdk_export),
        ("opentelemetry.exporter", fake_otlp_pkg),
        ("opentelemetry.exporter.otlp", fake_otlp_pkg2),
        ("opentelemetry.exporter.otlp.proto", fake_otlp_pkg3),
        ("opentelemetry.exporter.otlp.proto.http", fake_otlp_pkg4),
        ("opentelemetry.exporter.otlp.proto.http.trace_exporter", fake_otlp_te),
    ):
        monkeypatch.setitem(sys.modules, name, mod)

    monkeypatch.setenv("HONEYCOMB_API_KEY", "team-secret")

    plugin = honeycomb_mod.plugin()
    ctx = _make_context(tmp_path, "honeycomb")
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_run_success(state={}, context=ctx)

    assert captured_init["headers"]["x-honeycomb-team"] == "team-secret"
    assert captured_init["endpoint"].endswith("/v1/traces")
    assert captured_init["shutdown"] is True


# ---------------------------------------------------------------------------
# Prometheus
# ---------------------------------------------------------------------------


def test_prometheus_increments_run_counter(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake = types.ModuleType("prometheus_client")
    increments: list[tuple[str, dict]] = []
    histograms: list[tuple[str, float]] = []

    class FakeRegistry:
        _names_to_collectors: ClassVar[dict] = {}

    class FakeCounter:
        def __init__(self, name: str, doc: str, labelnames: list | None = None, registry: Any = None) -> None:
            self.name = name
            self.labelnames = labelnames or []

        def labels(self, **kwargs: Any) -> FakeCounter:
            self._labels = kwargs
            return self

        def inc(self) -> None:
            increments.append((self.name, getattr(self, "_labels", {})))

    class FakeHistogram:
        def __init__(self, name: str, doc: str, registry: Any = None) -> None:
            self.name = name

        def observe(self, value: float) -> None:
            histograms.append((self.name, value))

    fake.CollectorRegistry = FakeRegistry
    fake.Counter = FakeCounter
    fake.Histogram = FakeHistogram
    fake.REGISTRY = FakeRegistry()
    monkeypatch.setitem(sys.modules, "prometheus_client", fake)

    plugin = prometheus_mod.plugin()
    ctx = _make_context(tmp_path, "prometheus")
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_complete(
        state={}, context=ctx,
        effect_path="prime.x",
        effect_result={"meta": {"tokens_sent": 7, "tokens_received": 4}},
    )
    plugin.on_run_success(state={}, context=ctx)

    assert ("circuitry_runs_total", {"status": "started"}) in increments
    assert ("circuitry_runs_total", {"status": "succeeded"}) in increments
    # Effects counter has no labels.
    assert any(m == "circuitry_effects_total" for m, _ in increments)
    assert ("circuitry_tokens_sent", 7) in histograms
    assert ("circuitry_tokens_received", 4) in histograms


# ---------------------------------------------------------------------------
# Loki
# ---------------------------------------------------------------------------


def test_loki_pushes_lifecycle_events(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake = types.ModuleType("requests")
    posts: list[dict] = []

    def fake_post(url: str, **kwargs: Any) -> Any:
        posts.append({"url": url, **kwargs})
        return types.SimpleNamespace(status_code=204)

    fake.post = fake_post
    monkeypatch.setitem(sys.modules, "requests", fake)
    monkeypatch.setenv("LOKI_URL", "http://loki.example:3100")

    plugin = loki_mod.plugin()
    ctx = _make_context(tmp_path, "loki")
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_run_failure(state={}, context=ctx, error="boom")

    assert len(posts) == 2
    assert posts[0]["url"].endswith("/loki/api/v1/push")
    streams = posts[0]["json"]["streams"]
    assert streams[0]["stream"]["service"] == "circuitry"
    assert streams[0]["stream"]["event"] == "run_start"
    fail_streams = posts[1]["json"]["streams"]
    assert fail_streams[0]["stream"]["event"] == "run_failure"
    body = _json.loads(fail_streams[0]["values"][0][1])
    assert body["error"] == "boom"


def test_loki_no_op_without_url(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake = types.ModuleType("requests")
    fake.post = lambda *a, **k: None  # type: ignore[assignment]
    monkeypatch.setitem(sys.modules, "requests", fake)
    monkeypatch.delenv("LOKI_URL", raising=False)

    plugin = loki_mod.plugin()
    ctx = PluginContext(
        run_id="r", orchestration_path=tmp_path / "x.yml",
        dry_run=False, validate_only=False, runtime_config={},
    )
    # No exception, no HTTP call.
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_run_success(state={}, context=ctx)


# ---------------------------------------------------------------------------
# CloudWatch
# ---------------------------------------------------------------------------


def test_cloudwatch_writes_log_events_with_run_stream(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake = types.ModuleType("boto3")
    captured: dict[str, Any] = {"streams": [], "events": []}

    class FakeLogsClient:
        def create_log_stream(self, **kwargs: Any) -> None:
            captured["streams"].append(kwargs)

        def put_log_events(self, **kwargs: Any) -> dict:
            captured["events"].append(kwargs)
            return {"nextSequenceToken": "tok-2"}

    fake.client = lambda service, **kwargs: FakeLogsClient()
    monkeypatch.setitem(sys.modules, "boto3", fake)
    monkeypatch.setenv("CLOUDWATCH_LOG_GROUP", "/circuitry/runs")

    plugin = cloudwatch_mod.plugin()
    ctx = _make_context(tmp_path, "cloudwatch")
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_run_success(state={}, context=ctx)

    assert captured["streams"][0]["logGroupName"] == "/circuitry/runs"
    assert captured["streams"][0]["logStreamName"] == "circuitry-run-1"
    assert len(captured["events"]) == 2
    msgs = [_json.loads(e["logEvents"][0]["message"]) for e in captured["events"]]
    assert [m["event"] for m in msgs] == ["run_start", "run_success"]
