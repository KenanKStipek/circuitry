"""Tests for the document/KV/object/append-log persistence runtime
plugins (mongodb, dynamodb, firestore, couchdb, elasticsearch,
opensearch, redis, memcached, s3, gcs, azure-blob, r2, jsonl-file).

The shared snapshot lifecycle is exercised through one document store
(mongodb) plus one KV (redis) and one object store (s3); other plugins
in the same family get coverage on factory wiring + check() + a
representative happy-path call. jsonl-file has its own dedicated tests
since its append-event semantic differs from the snapshot pattern.
"""

from __future__ import annotations

import importlib.util
import json as _json
import sys
import types
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest

from circuitry.core.runtime_plugins import PluginContext, load_plugins
from circuitry.runtime_plugins import (
    azure_blob as azure_blob_mod,
    couchdb as couchdb_mod,
    dynamodb as dynamodb_mod,
    elasticsearch as es_mod,
    firestore as firestore_mod,
    gcs as gcs_mod,
    jsonl_file as jsonl_mod,
    memcached as memcached_mod,
    mongodb as mongodb_mod,
    opensearch as opensearch_mod,
    r2 as r2_mod,
    redis as redis_mod,
    s3 as s3_mod,
)
from circuitry.runtime_plugins._snapshot_persistence import (
    SnapshotPersistenceBase,
)


STORAGE_PLUGINS = [
    ("mongodb", "pymongo", "pymongo"),
    ("dynamodb", "boto3", "boto3"),
    ("firestore", "google.cloud.firestore", "google-cloud-firestore"),
    ("couchdb", "requests", "requests"),
    ("elasticsearch", "elasticsearch", "elasticsearch"),
    ("opensearch", "opensearchpy", "opensearch-py"),
    ("redis", "redis", "redis"),
    ("memcached", "pymemcache", "pymemcache"),
    ("s3", "boto3", "boto3"),
    ("gcs", "google.cloud.storage", "google-cloud-storage"),
    ("azure-blob", "azure.storage.blob", "azure-storage-blob"),
    ("r2", "boto3", "boto3"),
]


# Plugin-name → file-stem mapping (most match, but azure-blob → azure_blob).
_FILE_STEM = {
    "azure-blob": "azure_blob",
}


@pytest.mark.parametrize("plugin_name,_dep_module,_dep_pypi", STORAGE_PLUGINS)
def test_factory_loads_each_storage_plugin(
    plugin_name: str, _dep_module: str, _dep_pypi: str
) -> None:
    stem = _FILE_STEM.get(plugin_name, plugin_name)
    results = load_plugins([f"circuitry.runtime_plugins.{stem}"])
    r = results[0]
    assert r.error is None, f"load failed: {r.error}"
    assert r.plugin is not None
    assert r.plugin.name == plugin_name


def test_jsonl_file_loads() -> None:
    results = load_plugins(["circuitry.runtime_plugins.jsonl_file"])
    assert results[0].error is None
    assert results[0].plugin.name == "jsonl-file"


def _force_missing(monkeypatch: pytest.MonkeyPatch, *modules: str) -> None:
    real = importlib.util.find_spec

    def fake(name: str, *args: Any, **kwargs: Any):
        if name in modules:
            return None
        return real(name, *args, **kwargs)

    monkeypatch.setattr("importlib.util.find_spec", fake)


@pytest.mark.parametrize("plugin_name,dep_module,dep_pypi", STORAGE_PLUGINS)
def test_check_reports_missing_dep(
    monkeypatch: pytest.MonkeyPatch,
    plugin_name: str,
    dep_module: str,
    dep_pypi: str,
) -> None:
    _force_missing(monkeypatch, dep_module)
    stem = _FILE_STEM.get(plugin_name, plugin_name)
    results = load_plugins([f"circuitry.runtime_plugins.{stem}"])
    chk = results[0].plugin.check()
    assert chk.ok is False
    assert f"library:{dep_pypi}" in chk.missing


# ---------------------------------------------------------------------------
# Shared lifecycle exercised via mongodb + fake pymongo
# ---------------------------------------------------------------------------


@dataclass
class _FakeCollection:
    upserts: list[tuple[dict, dict]] = field(default_factory=list)

    def replace_one(self, filter: dict, doc: dict, upsert: bool = False) -> Any:
        self.upserts.append((filter, dict(doc)))
        return None


class _FakeMongoClient:
    def __init__(self, *args: Any, **kwargs: Any) -> None:
        self._collections: dict[tuple[str, str], _FakeCollection] = {}
        self.closed = False

    def __getitem__(self, db_name: str) -> "_FakeMongoDB":
        return _FakeMongoDB(self, db_name)

    def close(self) -> None:
        self.closed = True


class _FakeMongoDB:
    def __init__(self, client: _FakeMongoClient, db_name: str) -> None:
        self._client = client
        self._db_name = db_name

    def __getitem__(self, collection_name: str) -> _FakeCollection:
        key = (self._db_name, collection_name)
        if key not in self._client._collections:
            self._client._collections[key] = _FakeCollection()
        return self._client._collections[key]


def _install_fake_pymongo(monkeypatch: pytest.MonkeyPatch) -> _FakeMongoClient:
    fake_mod = types.ModuleType("pymongo")
    client_holder: dict[str, _FakeMongoClient] = {}

    def make_client(*args: Any, **kwargs: Any) -> _FakeMongoClient:
        client_holder["c"] = _FakeMongoClient(*args, **kwargs)
        return client_holder["c"]

    fake_mod.MongoClient = make_client
    monkeypatch.setitem(sys.modules, "pymongo", fake_mod)
    return client_holder  # type: ignore[return-value]


def _make_context(tmp_path: Path, plugin_name: str = "mongodb") -> PluginContext:
    return PluginContext(
        run_id="run-1",
        orchestration_path=tmp_path / "orch.yml",
        dry_run=False,
        validate_only=False,
        runtime_config={
            "runtime_plugins": {
                plugin_name: {},
            },
        },
    )


def test_mongodb_lifecycle_writes_snapshot_per_phase(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_pymongo(monkeypatch)
    plugin = mongodb_mod.plugin()
    ctx = _make_context(tmp_path, "mongodb")

    plugin.on_run_start(state={"name": "K"}, context=ctx)
    plugin.on_run_success(state={"name": "K", "out": "done"}, context=ctx)

    client = holder["c"]
    coll = list(client._collections.values())[0]
    assert len(coll.upserts) == 2  # start + success
    start_doc = coll.upserts[0][1]
    assert start_doc["status"] == "running"
    assert start_doc["_id"] == "run-1"
    succ_doc = coll.upserts[1][1]
    assert succ_doc["status"] == "success"
    assert succ_doc["state"] == {"name": "K", "out": "done"}
    assert client.closed is True


def test_mongodb_failure_records_error(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_pymongo(monkeypatch)
    plugin = mongodb_mod.plugin()
    ctx = _make_context(tmp_path, "mongodb")

    plugin.on_run_start(state={}, context=ctx)
    plugin.on_run_failure(state={}, context=ctx, error="boom")

    coll = list(holder["c"]._collections.values())[0]
    fail_doc = coll.upserts[-1][1]
    assert fail_doc["status"] == "failed"
    assert fail_doc["error"] == "boom"


def test_mongodb_skips_per_effect_update_by_default(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_pymongo(monkeypatch)
    plugin = mongodb_mod.plugin()
    ctx = _make_context(tmp_path, "mongodb")
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_complete(
        state={"x": 1}, context=ctx,
        effect_path="prime.greet", effect_result={"value": "hi"},
    )
    coll = list(holder["c"]._collections.values())[0]
    # Only the start event wrote so far — effect skipped.
    assert len(coll.upserts) == 1


def test_mongodb_update_per_effect_when_configured(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_pymongo(monkeypatch)
    plugin = mongodb_mod.plugin()
    ctx = PluginContext(
        run_id="run-1",
        orchestration_path=tmp_path / "x.yml",
        dry_run=False,
        validate_only=False,
        runtime_config={
            "runtime_plugins": {
                "mongodb": {"update_per_effect": True},
            },
        },
    )
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_complete(
        state={"x": 1}, context=ctx,
        effect_path="prime.greet", effect_result={"value": "hi"},
    )
    coll = list(holder["c"]._collections.values())[0]
    assert len(coll.upserts) == 2  # start + per-effect update


# ---------------------------------------------------------------------------
# DynamoDB — JSON serialization of state field
# ---------------------------------------------------------------------------


def test_dynamodb_serializes_state_to_json(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake = types.ModuleType("boto3")
    captured: list[dict] = []

    class FakeTable:
        def put_item(self, *, Item: dict) -> None:
            captured.append(dict(Item))

    class FakeResource:
        def Table(self, name: str) -> FakeTable:
            return FakeTable()

    fake.resource = lambda *a, **k: FakeResource()
    monkeypatch.setitem(sys.modules, "boto3", fake)
    monkeypatch.setenv("DYNAMODB_TABLE", "my-table")

    plugin = dynamodb_mod.plugin()
    ctx = _make_context(tmp_path, "dynamodb")
    plugin.on_run_start(state={"nested": {"a": 1}}, context=ctx)

    item = captured[0]
    assert item["run_id"] == "run-1"
    # state is JSON-encoded — dynamodb has no map type for arbitrary
    # nested user values without conversion.
    assert _json.loads(item["state"]) == {"nested": {"a": 1}}


def test_dynamodb_requires_table_name(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake = types.ModuleType("boto3")
    fake.resource = lambda *a, **k: None
    monkeypatch.setitem(sys.modules, "boto3", fake)
    monkeypatch.delenv("DYNAMODB_TABLE", raising=False)

    plugin = dynamodb_mod.plugin()
    ctx = PluginContext(
        run_id="r", orchestration_path=tmp_path / "x.yml",
        dry_run=False, validate_only=False, runtime_config={},
    )
    with pytest.raises(RuntimeError, match="dynamodb: table"):
        plugin.on_run_start(state={}, context=ctx)


# ---------------------------------------------------------------------------
# Firestore — google.cloud.firestore mock
# ---------------------------------------------------------------------------


def test_firestore_writes_via_document_set(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake_google = types.ModuleType("google")
    fake_cloud = types.ModuleType("google.cloud")
    fake_firestore = types.ModuleType("google.cloud.firestore")

    captured: dict[str, Any] = {}

    class FakeDoc:
        def __init__(self, doc_id: str) -> None:
            self._id = doc_id

        def set(self, snapshot: dict) -> None:
            captured["set_called"] = True
            captured["id"] = self._id
            captured["snapshot"] = dict(snapshot)

    class FakeCollection:
        def document(self, doc_id: str) -> FakeDoc:
            return FakeDoc(doc_id)

    class FakeFirestoreClient:
        def __init__(self, **kwargs: Any) -> None:
            captured["client_kwargs"] = kwargs

        def collection(self, name: str) -> FakeCollection:
            captured["collection"] = name
            return FakeCollection()

        def close(self) -> None:
            captured["closed"] = True

    fake_firestore.Client = FakeFirestoreClient
    fake_cloud.firestore = fake_firestore
    fake_google.cloud = fake_cloud
    monkeypatch.setitem(sys.modules, "google", fake_google)
    monkeypatch.setitem(sys.modules, "google.cloud", fake_cloud)
    monkeypatch.setitem(sys.modules, "google.cloud.firestore", fake_firestore)

    plugin = firestore_mod.plugin()
    ctx = _make_context(tmp_path, "firestore")
    plugin.on_run_start(state={"x": 1}, context=ctx)
    plugin.on_run_success(state={"x": 1}, context=ctx)

    assert captured["set_called"] is True
    assert captured["id"] == "run-1"
    assert captured["snapshot"]["status"] == "success"
    assert captured["closed"] is True


# ---------------------------------------------------------------------------
# CouchDB — HTTP via mocked requests session
# ---------------------------------------------------------------------------


def test_couchdb_uses_put_with_revision_chain(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake_mod = types.ModuleType("requests")
    captured_calls: list[tuple[str, dict]] = []

    class FakeResponse:
        def __init__(self, *, status_code: int, body: dict) -> None:
            self.status_code = status_code
            self._body = body

        @property
        def text(self) -> str:
            return _json.dumps(self._body) if self._body else ""

        def json(self) -> dict:
            return self._body

    rev_counter = {"i": 0}

    class FakeSession:
        def put(self, url: str, json: dict, timeout: int) -> FakeResponse:
            captured_calls.append((url, dict(json)))
            rev_counter["i"] += 1
            return FakeResponse(
                status_code=201,
                body={"rev": f"{rev_counter['i']}-fake"},
            )

        def close(self) -> None:
            pass

    fake_mod.Session = FakeSession
    monkeypatch.setitem(sys.modules, "requests", fake_mod)
    monkeypatch.setenv("COUCHDB_URL", "http://admin:pw@localhost:5984/circuitry")

    plugin = couchdb_mod.plugin()
    ctx = _make_context(tmp_path, "couchdb")
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_run_success(state={}, context=ctx)

    assert len(captured_calls) == 2
    url, doc = captured_calls[0]
    assert url.endswith("/run-1")
    assert "_rev" not in doc
    # Second write should include the rev returned by the first.
    _url2, doc2 = captured_calls[1]
    assert doc2["_rev"] == "1-fake"


# ---------------------------------------------------------------------------
# Elasticsearch / OpenSearch — index() upsert
# ---------------------------------------------------------------------------


def test_elasticsearch_calls_index_with_run_id(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake_mod = types.ModuleType("elasticsearch")
    captured: dict[str, Any] = {}

    class FakeES:
        def __init__(self, **kwargs: Any) -> None:
            captured["init"] = kwargs

        def index(self, *, index: str, id: str, document: dict) -> None:
            captured["index"] = index
            captured["id"] = id
            captured["doc"] = dict(document)

        def close(self) -> None:
            captured["closed"] = True

    fake_mod.Elasticsearch = FakeES
    monkeypatch.setitem(sys.modules, "elasticsearch", fake_mod)
    monkeypatch.setenv("ES_URL", "http://localhost:9200")

    plugin = es_mod.plugin()
    ctx = _make_context(tmp_path, "elasticsearch")
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_run_success(state={}, context=ctx)

    assert captured["id"] == "run-1"
    assert captured["doc"]["status"] == "success"
    assert captured["closed"] is True


def test_opensearch_parses_url_for_host_kwargs(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake_mod = types.ModuleType("opensearchpy")
    captured: dict[str, Any] = {}

    class FakeOS:
        def __init__(self, **kwargs: Any) -> None:
            captured["init"] = kwargs

        def index(self, **kwargs: Any) -> None:
            captured["index_args"] = kwargs

        def close(self) -> None:
            captured["closed"] = True

    fake_mod.OpenSearch = FakeOS
    monkeypatch.setitem(sys.modules, "opensearchpy", fake_mod)
    monkeypatch.setenv("OPENSEARCH_URL", "https://os.example:9200")
    monkeypatch.setenv("OPENSEARCH_USER", "alice")
    monkeypatch.setenv("OPENSEARCH_PASSWORD", "secret")

    plugin = opensearch_mod.plugin()
    ctx = _make_context(tmp_path, "opensearch")
    plugin.on_run_start(state={}, context=ctx)

    init = captured["init"]
    assert init["hosts"] == [{"host": "os.example", "port": 9200}]
    assert init["use_ssl"] is True
    assert init["http_auth"] == ("alice", "secret")


# ---------------------------------------------------------------------------
# Redis — JSON serialization + TTL
# ---------------------------------------------------------------------------


def test_redis_writes_with_ttl(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake_mod = types.ModuleType("redis")
    captured: list[tuple[str, str, dict]] = []

    class FakeRedisClient:
        def set(self, key: str, value: str, ex: int | None = None) -> None:
            captured.append((key, value, {"ex": ex}))

        def close(self) -> None:
            pass

    fake_mod.from_url = lambda url: FakeRedisClient()
    monkeypatch.setitem(sys.modules, "redis", fake_mod)

    plugin = redis_mod.plugin()
    ctx = PluginContext(
        run_id="run-1", orchestration_path=tmp_path / "x.yml",
        dry_run=False, validate_only=False,
        runtime_config={
            "runtime_plugins": {
                "redis": {"ttl_seconds": 3600, "key_prefix": "circuitry."},
            },
        },
    )
    plugin.on_run_start(state={"x": 1}, context=ctx)
    plugin.on_run_success(state={"x": 1}, context=ctx)

    assert len(captured) == 2
    key, payload, kw = captured[0]
    assert key == "circuitry.run:run-1"
    assert kw["ex"] == 3600
    parsed = _json.loads(payload)
    assert parsed["status"] == "running"


def test_redis_no_ttl_when_unset(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake_mod = types.ModuleType("redis")
    captured_kwargs: list[dict[str, Any]] = []

    class FakeRedisClient:
        def set(self, *args: Any, **kwargs: Any) -> None:
            captured_kwargs.append(kwargs)

        def close(self) -> None:
            pass

    fake_mod.from_url = lambda url: FakeRedisClient()
    monkeypatch.setitem(sys.modules, "redis", fake_mod)

    plugin = redis_mod.plugin()
    ctx = _make_context(tmp_path, "redis")
    plugin.on_run_start(state={}, context=ctx)

    # Without ttl_seconds, plugin calls set() WITHOUT the ex kwarg.
    assert "ex" not in captured_kwargs[0]


# ---------------------------------------------------------------------------
# Memcached
# ---------------------------------------------------------------------------


def test_memcached_writes_with_expire(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake_pkg = types.ModuleType("pymemcache")
    fake_client = types.ModuleType("pymemcache.client")
    fake_base = types.ModuleType("pymemcache.client.base")
    captured: list[tuple[str, str, int]] = []

    class FakeClient:
        def __init__(self, host_port: tuple) -> None:
            self.host_port = host_port

        def set(self, key: str, value: str, expire: int = 0) -> None:
            captured.append((key, value, expire))

        def close(self) -> None:
            pass

    fake_base.Client = FakeClient
    fake_client.base = fake_base
    fake_pkg.client = fake_client
    monkeypatch.setitem(sys.modules, "pymemcache", fake_pkg)
    monkeypatch.setitem(sys.modules, "pymemcache.client", fake_client)
    monkeypatch.setitem(sys.modules, "pymemcache.client.base", fake_base)

    plugin = memcached_mod.plugin()
    ctx = PluginContext(
        run_id="run-1", orchestration_path=tmp_path / "x.yml",
        dry_run=False, validate_only=False,
        runtime_config={
            "runtime_plugins": {"memcached": {"ttl_seconds": 600}},
        },
    )
    plugin.on_run_start(state={}, context=ctx)
    assert captured[0][0] == "run:run-1"
    assert captured[0][2] == 600


# ---------------------------------------------------------------------------
# S3 / GCS / Azure / R2 — single representative check
# ---------------------------------------------------------------------------


def test_s3_writes_blob_at_run_id_path(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake_mod = types.ModuleType("boto3")
    captured: dict[str, Any] = {}

    class FakeS3Client:
        def put_object(self, **kwargs: Any) -> None:
            captured.update(kwargs)

    fake_mod.client = lambda service, **kwargs: FakeS3Client()
    monkeypatch.setitem(sys.modules, "boto3", fake_mod)
    monkeypatch.setenv("S3_BUCKET", "my-bucket")

    plugin = s3_mod.plugin()
    ctx = _make_context(tmp_path, "s3")
    plugin.on_run_start(state={"x": 1}, context=ctx)

    assert captured["Bucket"] == "my-bucket"
    assert captured["Key"] == "runs/run-1.json"
    assert captured["ContentType"] == "application/json"
    body = _json.loads(captured["Body"].decode("utf-8"))
    assert body["status"] == "running"


def test_s3_requires_bucket(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake_mod = types.ModuleType("boto3")
    fake_mod.client = lambda service, **kwargs: None
    monkeypatch.setitem(sys.modules, "boto3", fake_mod)
    monkeypatch.delenv("S3_BUCKET", raising=False)

    plugin = s3_mod.plugin()
    ctx = PluginContext(
        run_id="r", orchestration_path=tmp_path / "x.yml",
        dry_run=False, validate_only=False, runtime_config={},
    )
    with pytest.raises(RuntimeError, match="s3: bucket"):
        plugin.on_run_start(state={}, context=ctx)


def test_gcs_uploads_to_bucket(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake_google = types.ModuleType("google")
    fake_cloud = types.ModuleType("google.cloud")
    fake_storage = types.ModuleType("google.cloud.storage")

    captured: dict[str, Any] = {}

    class FakeBlob:
        def __init__(self, name: str) -> None:
            self._name = name

        def upload_from_string(self, data: str, content_type: str) -> None:
            captured["data"] = data
            captured["name"] = self._name
            captured["content_type"] = content_type

    class FakeBucket:
        def blob(self, name: str) -> FakeBlob:
            return FakeBlob(name)

    class FakeStorageClient:
        def __init__(self, **kwargs: Any) -> None:
            captured["client_kwargs"] = kwargs

        def bucket(self, name: str) -> FakeBucket:
            captured["bucket_name"] = name
            return FakeBucket()

    fake_storage.Client = FakeStorageClient
    fake_cloud.storage = fake_storage
    fake_google.cloud = fake_cloud
    monkeypatch.setitem(sys.modules, "google", fake_google)
    monkeypatch.setitem(sys.modules, "google.cloud", fake_cloud)
    monkeypatch.setitem(sys.modules, "google.cloud.storage", fake_storage)
    monkeypatch.setenv("GCS_BUCKET", "my-gcs-bucket")

    plugin = gcs_mod.plugin()
    ctx = _make_context(tmp_path, "gcs")
    plugin.on_run_start(state={}, context=ctx)
    assert captured["bucket_name"] == "my-gcs-bucket"
    assert captured["name"] == "runs/run-1.json"


def test_azure_blob_uses_connection_string(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake_azure = types.ModuleType("azure")
    fake_azure_storage = types.ModuleType("azure.storage")
    fake_azure_blob = types.ModuleType("azure.storage.blob")

    captured: dict[str, Any] = {}

    class FakeBlobClient:
        def __init__(self, name: str) -> None:
            self._name = name

        def upload_blob(self, body: bytes, overwrite: bool) -> None:
            captured["name"] = self._name
            captured["overwrite"] = overwrite
            captured["body"] = body

    class FakeContainerClient:
        def get_blob_client(self, name: str) -> FakeBlobClient:
            return FakeBlobClient(name)

    class FakeBlobServiceClient:
        @classmethod
        def from_connection_string(cls, cstr: str) -> "FakeBlobServiceClient":
            captured["connection_string"] = cstr
            return cls()

        def get_container_client(self, name: str) -> FakeContainerClient:
            captured["container"] = name
            return FakeContainerClient()

        def close(self) -> None:
            captured["closed"] = True

    fake_azure_blob.BlobServiceClient = FakeBlobServiceClient
    fake_azure_storage.blob = fake_azure_blob
    fake_azure.storage = fake_azure_storage
    monkeypatch.setitem(sys.modules, "azure", fake_azure)
    monkeypatch.setitem(sys.modules, "azure.storage", fake_azure_storage)
    monkeypatch.setitem(sys.modules, "azure.storage.blob", fake_azure_blob)
    monkeypatch.setenv("AZURE_STORAGE_CONNECTION_STRING", "DefaultEndpointsProtocol=https;...")
    monkeypatch.setenv("AZURE_BLOB_CONTAINER", "my-container")

    plugin = azure_blob_mod.plugin()
    ctx = _make_context(tmp_path, "azure-blob")
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_run_success(state={}, context=ctx)
    assert captured["container"] == "my-container"
    assert captured["name"] == "runs/run-1.json"
    assert captured["overwrite"] is True
    assert captured["closed"] is True


def test_r2_resolves_endpoint_from_account_id(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake_mod = types.ModuleType("boto3")
    captured_kwargs: dict[str, Any] = {}

    class FakeS3Client:
        def put_object(self, **kwargs: Any) -> None:
            pass

    def fake_client(service: str, **kwargs: Any) -> FakeS3Client:
        captured_kwargs.update(kwargs)
        return FakeS3Client()

    fake_mod.client = fake_client
    monkeypatch.setitem(sys.modules, "boto3", fake_mod)
    monkeypatch.setenv("R2_ACCOUNT_ID", "abc123")
    monkeypatch.setenv("R2_ACCESS_KEY_ID", "akid")
    monkeypatch.setenv("R2_SECRET_ACCESS_KEY", "secret")
    monkeypatch.setenv("R2_BUCKET", "my-r2-bucket")

    plugin = r2_mod.plugin()
    ctx = _make_context(tmp_path, "r2")
    plugin.on_run_start(state={}, context=ctx)

    assert (
        captured_kwargs["endpoint_url"]
        == "https://abc123.r2.cloudflarestorage.com"
    )
    assert captured_kwargs["aws_access_key_id"] == "akid"
    assert captured_kwargs["region_name"] == "auto"


# ---------------------------------------------------------------------------
# JSONL append-log
# ---------------------------------------------------------------------------


def _read_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [_json.loads(line) for line in path.read_text().splitlines() if line]


def test_jsonl_writes_run_lifecycle_events(tmp_path: Path) -> None:
    log_path = tmp_path / "events.jsonl"
    plugin = jsonl_mod.plugin()
    ctx = PluginContext(
        run_id="run-1", orchestration_path=tmp_path / "x.yml",
        dry_run=False, validate_only=False,
        runtime_config={"runtime_plugins": {"jsonl-file": {"path": str(log_path)}}},
    )
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

    events = _read_jsonl(log_path)
    assert len(events) == 3
    assert events[0]["event"] == "run_start"
    assert events[0]["run_id"] == "run-1"
    assert events[1]["event"] == "effect_complete"
    assert events[1]["effect_path"] == "prime.greet"
    assert events[1]["tokens_sent"] == 10
    assert events[2]["event"] == "run_success"


def test_jsonl_skips_effect_events_when_disabled(tmp_path: Path) -> None:
    log_path = tmp_path / "events.jsonl"
    plugin = jsonl_mod.plugin()
    ctx = PluginContext(
        run_id="run-1", orchestration_path=tmp_path / "x.yml",
        dry_run=False, validate_only=False,
        runtime_config={
            "runtime_plugins": {
                "jsonl-file": {"path": str(log_path), "include_effects": False},
            },
        },
    )
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_complete(
        state={}, context=ctx, effect_path="prime.x", effect_result={},
    )
    plugin.on_run_success(state={}, context=ctx)
    events = _read_jsonl(log_path)
    assert [e["event"] for e in events] == ["run_start", "run_success"]


def test_jsonl_failure_records_error(tmp_path: Path) -> None:
    log_path = tmp_path / "events.jsonl"
    plugin = jsonl_mod.plugin()
    ctx = PluginContext(
        run_id="run-1", orchestration_path=tmp_path / "x.yml",
        dry_run=False, validate_only=False,
        runtime_config={"runtime_plugins": {"jsonl-file": {"path": str(log_path)}}},
    )
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_run_failure(state={}, context=ctx, error="boom")
    events = _read_jsonl(log_path)
    assert events[-1]["event"] == "run_failure"
    assert events[-1]["error"] == "boom"


def test_jsonl_check_always_ok() -> None:
    assert jsonl_mod.plugin().check().ok is True


# ---------------------------------------------------------------------------
# Snapshot base class invariants (exercised via mongodb)
# ---------------------------------------------------------------------------


def test_snapshot_includes_canonical_fields(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_pymongo(monkeypatch)
    plugin = mongodb_mod.plugin()
    ctx = _make_context(tmp_path, "mongodb")
    plugin.on_run_start(state={"k": "v"}, context=ctx)

    coll = list(holder["c"]._collections.values())[0]
    doc = coll.upserts[0][1]
    for required in (
        "run_id", "orchestration_path", "status", "started_at",
        "ended_at", "error", "state", "_id",
    ):
        assert required in doc, f"missing field: {required}"
    assert doc["run_id"] == doc["_id"]


def test_base_class_protocol_requirement() -> None:
    """Subclasses MUST override the abstract methods — fail fast if a
    new plugin author forgets."""
    base = SnapshotPersistenceBase()
    with pytest.raises(NotImplementedError):
        base._check_dep()
    with pytest.raises(NotImplementedError):
        base._upsert_snapshot("run-1", {})
