"""Tests for the SurrealDB B-prime persistence runtime plugin.

Same driver-model split as clickhouse (SurrealQL via ``query()``, not
DBAPI cursor semantics) — a fake ``surrealdb.Surreal`` client is
injected via ``sys.modules`` to drive the lifecycle hooks without a
live SurrealDB instance.
"""

from __future__ import annotations

import importlib.util
import sys
import types
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from circuitry.core.runtime_plugins import PluginContext, load_plugins
from circuitry.runtime_plugins import surrealdb as surrealdb_mod


class _FakeSurrealClient:
    def __init__(self, url: str) -> None:
        self.url = url
        self.queries: list[tuple[str, dict[str, Any] | None]] = []
        self.used: tuple[str, str] | None = None
        self.signin_attempts: list[dict[str, Any]] = []
        self.signed_in: dict[str, Any] | None = None
        self.authenticated: str | None = None
        self.closed = False
        #: When set, a signin call is only accepted if this returns True —
        #: lets tests simulate a user defined at one specific auth level.
        self.signin_accepts: Callable[[dict[str, Any]], bool] | None = None

    def query(self, sql: str, variables: dict[str, Any] | None = None) -> None:
        self.queries.append((sql, variables))

    def use(self, namespace: str, database: str) -> None:
        self.used = (namespace, database)

    def signin(self, credentials: dict[str, Any]) -> None:
        self.signin_attempts.append(dict(credentials))
        if self.signin_accepts is not None and not self.signin_accepts(credentials):
            raise Exception("There was a problem with authentication")
        self.signed_in = dict(credentials)

    def authenticate(self, token: str) -> None:
        self.authenticated = token

    def close(self) -> None:
        self.closed = True


def _install_fake_surrealdb(
    monkeypatch: pytest.MonkeyPatch,
    *,
    signin_accepts: Callable[[dict[str, Any]], bool] | None = None,
) -> dict[str, _FakeSurrealClient]:
    fake_mod = types.ModuleType("surrealdb")
    holder: dict[str, _FakeSurrealClient] = {}

    def make_client(url: str) -> _FakeSurrealClient:
        client = _FakeSurrealClient(url)
        client.signin_accepts = signin_accepts
        holder["c"] = client
        return client

    fake_mod.Surreal = make_client  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "surrealdb", fake_mod)
    return holder


def _make_context(
    tmp_path: Path, *, run_id: str = "run-1", cfg: dict[str, Any] | None = None
) -> PluginContext:
    return PluginContext(
        run_id=run_id,
        orchestration_path=tmp_path / "orch.yml",
        dry_run=False,
        validate_only=False,
        runtime_config={"runtime_plugins": {"surrealdb": cfg or {}}},
    )


# ---------------------------------------------------------------------------
# Factory + check()
# ---------------------------------------------------------------------------


def test_factory_loads_surrealdb_plugin() -> None:
    results = load_plugins(["circuitry.runtime_plugins.surrealdb"])
    r = results[0]
    assert r.error is None, f"load failed: {r.error}"
    assert r.plugin is not None
    assert r.plugin.name == "surrealdb"


def test_check_reports_missing_dep(monkeypatch: pytest.MonkeyPatch) -> None:
    real = importlib.util.find_spec

    def fake(name: str, *args: Any, **kwargs: Any):
        if name == "surrealdb":
            return None
        return real(name, *args, **kwargs)

    monkeypatch.setattr("importlib.util.find_spec", fake)
    results = load_plugins(["circuitry.runtime_plugins.surrealdb"])
    chk = results[0].plugin.check()
    assert chk.ok is False
    assert "library:surrealdb" in chk.missing


def test_check_ok_when_dep_present(monkeypatch: pytest.MonkeyPatch) -> None:
    real = importlib.util.find_spec

    def fake(name: str, *args: Any, **kwargs: Any):
        if name == "surrealdb":
            return object()
        return real(name, *args, **kwargs)

    monkeypatch.setattr("importlib.util.find_spec", fake)
    plugin = surrealdb_mod.plugin()
    assert plugin.check().ok is True


# ---------------------------------------------------------------------------
# Lifecycle round trip
# ---------------------------------------------------------------------------


def test_lifecycle_round_trip_writes_run_and_effect(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_surrealdb(monkeypatch)
    plugin = surrealdb_mod.plugin()
    ctx = _make_context(tmp_path)

    plugin.on_run_start(state={"input": {"user": "Ada"}}, context=ctx)
    client = holder["c"]

    # Schema bootstrap + initial CREATE runs.
    queries = [q[0] for q in client.queries]
    assert any("DEFINE TABLE IF NOT EXISTS runs" in q for q in queries)
    assert any("DEFINE TABLE IF NOT EXISTS effects" in q for q in queries)
    assert any(q.startswith("CREATE runs CONTENT") for q in queries)
    run_insert = next(q for q in client.queries if q[0].startswith("CREATE runs CONTENT"))
    data = run_insert[1]["data"]
    assert data["run_id"] == "run-1"
    assert data["status"] == "running"
    assert data["inputs"] == {"user": "Ada"}

    plugin.on_effect_complete(
        state={},
        context=ctx,
        effect_path="prime.greet",
        effect_result={
            "value": "hello",
            "meta": {
                "prompt_type": "text",
                "prompt_sent": "hi",
                "tokens_sent": 10,
                "tokens_received": 5,
                "created_at": "2026-01-01T00:00:00",
                "completed_at": "2026-01-01T00:00:01",
            },
        },
    )
    effect_inserts = [q for q in client.queries if q[0].startswith("CREATE effects CONTENT")]
    assert len(effect_inserts) == 1
    effect_data = effect_inserts[0][1]["data"]
    assert effect_data["run_id"] == "run-1"
    assert effect_data["state_path"] == "prime.greet"
    assert effect_data["effect_name"] == "greet"
    assert effect_data["effect_type"] == "prompt"
    assert effect_data["value"] == "hello"
    assert effect_data["tokens_sent"] == 10
    assert effect_data["status"] == "success"

    plugin.on_run_success(state={}, context=ctx)
    updates = [q for q in client.queries if q[0].startswith("UPDATE runs SET")]
    assert len(updates) == 1
    assert updates[0][1]["status"] == "success"
    assert updates[0][1]["run_id"] == "run-1"
    assert client.closed is True


def test_failure_marks_runs_row(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_surrealdb(monkeypatch)
    plugin = surrealdb_mod.plugin()
    ctx = _make_context(tmp_path)

    plugin.on_run_start(state={}, context=ctx)
    plugin.on_run_failure(state={}, context=ctx, error="boom")

    client = holder["c"]
    updates = [q for q in client.queries if q[0].startswith("UPDATE runs SET")]
    assert len(updates) == 1
    assert updates[0][1]["status"] == "failed"
    assert updates[0][1]["error"] == "boom"


def test_effect_complete_before_run_start_is_noop(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _install_fake_surrealdb(monkeypatch)
    plugin = surrealdb_mod.plugin()
    ctx = _make_context(tmp_path)
    # No on_run_start called — the plugin has no open client yet.
    plugin.on_effect_complete(
        state={}, context=ctx, effect_path="prime.greet", effect_result={"value": "x"},
    )  # must not raise


def test_loop_iteration_path_decoded(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_surrealdb(monkeypatch)
    plugin = surrealdb_mod.plugin()
    ctx = _make_context(tmp_path)
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_complete(
        state={},
        context=ctx,
        effect_path="prime.my_loop.iter_3.handle",
        effect_result={"value": "x", "meta": {}},
    )
    client = holder["c"]
    effect_insert = next(q for q in client.queries if q[0].startswith("CREATE effects CONTENT"))
    data = effect_insert[1]["data"]
    assert data["state_path"] == "prime.my_loop.iter_3.handle"
    assert data["effect_name"] == "handle"
    assert data["parent_path"] == "prime.my_loop.iter_3"
    assert data["iteration_index"] == 3


# ---------------------------------------------------------------------------
# store_raw / prod redaction
# ---------------------------------------------------------------------------


def test_store_raw_dev_default_includes_raw(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("CIRCUITRY_ENV", "dev")
    holder = _install_fake_surrealdb(monkeypatch)
    plugin = surrealdb_mod.plugin()
    ctx = _make_context(tmp_path)
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_complete(
        state={},
        context=ctx,
        effect_path="prime.tool1",
        effect_result={"value": "result", "meta": {"raw": {"detail": "verbose data"}}},
    )
    client = holder["c"]
    effect_insert = next(q for q in client.queries if q[0].startswith("CREATE effects CONTENT"))
    assert effect_insert[1]["data"]["raw"] == {"detail": "verbose data"}


def test_store_raw_prod_default_skips_raw(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("CIRCUITRY_ENV", "prod")
    holder = _install_fake_surrealdb(monkeypatch)
    plugin = surrealdb_mod.plugin()
    ctx = _make_context(tmp_path)
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_complete(
        state={},
        context=ctx,
        effect_path="prime.tool1",
        effect_result={"value": "result", "meta": {"raw": {"large": "blob"}}},
    )
    client = holder["c"]
    effect_insert = next(q for q in client.queries if q[0].startswith("CREATE effects CONTENT"))
    assert effect_insert[1]["data"]["raw"] is None


def test_store_raw_config_overrides_environment_default(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("CIRCUITRY_ENV", "prod")
    holder = _install_fake_surrealdb(monkeypatch)
    plugin = surrealdb_mod.plugin()
    ctx = _make_context(tmp_path, cfg={"store_raw": True})
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_complete(
        state={},
        context=ctx,
        effect_path="prime.tool1",
        effect_result={"value": "result", "meta": {"raw": {"large": "blob"}}},
    )
    client = holder["c"]
    effect_insert = next(q for q in client.queries if q[0].startswith("CREATE effects CONTENT"))
    assert effect_insert[1]["data"]["raw"] == {"large": "blob"}


# ---------------------------------------------------------------------------
# Connection / auth resolution
# ---------------------------------------------------------------------------


def test_signin_with_user_and_password(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_surrealdb(monkeypatch)
    monkeypatch.setenv("SURREAL_USER", "root")
    monkeypatch.setenv("SURREAL_PASS", "secret")
    monkeypatch.delenv("SURREAL_TOKEN", raising=False)
    plugin = surrealdb_mod.plugin()
    ctx = _make_context(tmp_path)
    plugin.on_run_start(state={}, context=ctx)

    client = holder["c"]
    assert client.signed_in == {
        "username": "root",
        "password": "secret",
        "namespace": "circuitry",
        "database": "circuitry",
    }
    assert client.authenticated is None


def test_signin_includes_namespace_and_database_for_scoped_user(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_surrealdb(monkeypatch)
    monkeypatch.setenv("SURREAL_USER", "runner")
    monkeypatch.setenv("SURREAL_PASS", "secret")
    monkeypatch.delenv("SURREAL_TOKEN", raising=False)
    plugin = surrealdb_mod.plugin()
    ctx = _make_context(tmp_path, cfg={"namespace": "prod", "database": "trading"})
    plugin.on_run_start(state={}, context=ctx)

    client = holder["c"]
    assert client.signin_attempts[0] == {
        "username": "runner",
        "password": "secret",
        "namespace": "prod",
        "database": "trading",
    }
    assert client.signed_in == client.signin_attempts[0]


def test_signin_falls_back_to_root_when_scoped_signin_rejected(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A ROOT-only user rejects the (default) scoped attempts but accepts bare."""
    holder = _install_fake_surrealdb(
        monkeypatch, signin_accepts=lambda creds: "namespace" not in creds
    )
    monkeypatch.setenv("SURREAL_USER", "root")
    monkeypatch.setenv("SURREAL_PASS", "root")
    monkeypatch.delenv("SURREAL_TOKEN", raising=False)
    plugin = surrealdb_mod.plugin()
    ctx = _make_context(tmp_path)
    plugin.on_run_start(state={}, context=ctx)

    client = holder["c"]
    assert client.signin_attempts == [
        {
            "username": "root",
            "password": "root",
            "namespace": "circuitry",
            "database": "circuitry",
        },
        {"username": "root", "password": "root", "namespace": "circuitry"},
        {"username": "root", "password": "root"},
    ]
    assert client.signed_in == {"username": "root", "password": "root"}


def test_token_auth_takes_priority_over_user_password(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_surrealdb(monkeypatch)
    monkeypatch.setenv("SURREAL_TOKEN", "jwt-token")
    monkeypatch.setenv("SURREAL_USER", "root")
    monkeypatch.setenv("SURREAL_PASS", "secret")
    plugin = surrealdb_mod.plugin()
    ctx = _make_context(tmp_path)
    plugin.on_run_start(state={}, context=ctx)

    client = holder["c"]
    assert client.authenticated == "jwt-token"
    assert client.signed_in is None


def test_no_auth_call_when_credentials_absent(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_surrealdb(monkeypatch)
    monkeypatch.delenv("SURREAL_TOKEN", raising=False)
    monkeypatch.delenv("SURREAL_USER", raising=False)
    monkeypatch.delenv("SURREAL_PASS", raising=False)
    plugin = surrealdb_mod.plugin()
    ctx = _make_context(tmp_path)
    plugin.on_run_start(state={}, context=ctx)

    client = holder["c"]
    assert client.authenticated is None
    assert client.signed_in is None


def test_url_namespace_database_resolution(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_surrealdb(monkeypatch)
    monkeypatch.delenv("SURREAL_URL", raising=False)
    monkeypatch.delenv("SURREAL_NAMESPACE", raising=False)
    monkeypatch.delenv("SURREAL_DATABASE", raising=False)
    plugin = surrealdb_mod.plugin()
    ctx = _make_context(
        tmp_path,
        cfg={"url": "ws://db.example:8000/rpc", "namespace": "prod_ns", "database": "prod_db"},
    )
    plugin.on_run_start(state={}, context=ctx)

    client = holder["c"]
    assert client.url == "ws://db.example:8000/rpc"
    assert client.used == ("prod_ns", "prod_db")


def test_url_defaults_when_unset(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    holder = _install_fake_surrealdb(monkeypatch)
    monkeypatch.delenv("SURREAL_URL", raising=False)
    monkeypatch.delenv("SURREAL_NAMESPACE", raising=False)
    monkeypatch.delenv("SURREAL_DATABASE", raising=False)
    plugin = surrealdb_mod.plugin()
    ctx = _make_context(tmp_path)
    plugin.on_run_start(state={}, context=ctx)

    client = holder["c"]
    assert client.url == "ws://localhost:8000/rpc"
    assert client.used == ("circuitry", "circuitry")
