"""Tests for the SQL B-prime persistence runtime plugin cohort
(postgres, cockroachdb, mysql, duckdb, mssql, clickhouse).

The plugins use lazy imports so each test injects fake driver modules
via ``sys.modules`` to drive the lifecycle hooks without requiring the
real DB driver. The shared base class is exercised through one of the
DBAPI-compatible plugins (postgres) — every other DBAPI plugin
(cockroachdb / mysql / duckdb / mssql) is verified for factory wiring,
dialect, and check() output, since their lifecycle code path is
identical.

ClickHouse uses a different driver model and gets its own coverage.
"""

from __future__ import annotations

import importlib.util
import sys
import types
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pytest

from circuitry.core.runtime_plugins import PluginContext, load_plugins
from circuitry.runtime_plugins import (
    clickhouse as clickhouse_mod,
)
from circuitry.runtime_plugins import (
    cockroachdb as cockroachdb_mod,
)
from circuitry.runtime_plugins import (
    duckdb as duckdb_mod,
)
from circuitry.runtime_plugins import (
    mssql as mssql_mod,
)
from circuitry.runtime_plugins import (
    mysql as mysql_mod,
)
from circuitry.runtime_plugins import (
    postgres as postgres_mod,
)
from circuitry.runtime_plugins._sql_schema import (
    CLICKHOUSE,
    COCKROACHDB,
    DUCKDB,
    MSSQL,
    MYSQL,
    POSTGRES,
    SQLITE,
    effect_results_ddl,
    runs_ddl,
)

# ---------------------------------------------------------------------------
# Dialect smoke tests
# ---------------------------------------------------------------------------


def test_postgres_dialect_renders_jsonb_and_bigserial() -> None:
    ddl = effect_results_ddl(POSTGRES)
    assert "BIGSERIAL PRIMARY KEY" in ddl
    assert "JSONB" in ddl
    assert "TIMESTAMPTZ" in ddl


def test_cockroachdb_dialect_aliases_postgres() -> None:
    assert COCKROACHDB is POSTGRES


def test_mysql_dialect_renders_auto_increment_and_json() -> None:
    ddl = effect_results_ddl(MYSQL)
    assert "AUTO_INCREMENT" in ddl
    assert "JSON" in ddl


def test_duckdb_dialect_includes_pre_ddl_sequence() -> None:
    assert any("CREATE SEQUENCE" in s for s in DUCKDB.pre_ddl)
    ddl = effect_results_ddl(DUCKDB)
    assert "DEFAULT nextval" in ddl


def test_mssql_dialect_uses_identity_and_nvarchar() -> None:
    ddl = effect_results_ddl(MSSQL)
    assert "IDENTITY(1,1)" in ddl
    assert "NVARCHAR(MAX)" in ddl


def test_clickhouse_dialect_metadata() -> None:
    # Even though ClickHouse uses its own DDL strings (not the
    # generic builder), the dialect should still be defined for
    # documentation / tooling consistency.
    assert CLICKHOUSE.json_type == "String"
    assert "DateTime64" in CLICKHOUSE.timestamp_type


def test_sqlite_dialect_unchanged() -> None:
    ddl = runs_ddl(SQLITE)
    assert "TEXT PRIMARY KEY" in ddl


# ---------------------------------------------------------------------------
# Plugin loading + check()
# ---------------------------------------------------------------------------


SQL_PLUGINS = [
    ("postgres", "psycopg2", "psycopg2-binary"),
    ("cockroachdb", "psycopg2", "psycopg2-binary"),
    ("mysql", "mysql.connector", "mysql-connector-python"),
    ("duckdb", "duckdb", "duckdb"),
    ("mssql", "pyodbc", "pyodbc"),
    ("clickhouse", "clickhouse_connect", "clickhouse-connect"),
]


@pytest.mark.parametrize("plugin_name,_dep_module,_dep_pypi", SQL_PLUGINS)
def test_factory_loads_each_sql_plugin(
    plugin_name: str, _dep_module: str, _dep_pypi: str
) -> None:
    results = load_plugins([f"circuitry.runtime_plugins.{plugin_name}"])
    r = results[0]
    assert r.error is None, f"load failed: {r.error}"
    assert r.plugin is not None
    assert r.plugin.name == plugin_name


def _force_missing(monkeypatch: pytest.MonkeyPatch, *modules: str) -> None:
    real = importlib.util.find_spec

    def fake(name: str, *args: Any, **kwargs: Any):
        if name in modules:
            return None
        return real(name, *args, **kwargs)

    monkeypatch.setattr("importlib.util.find_spec", fake)


@pytest.mark.parametrize("plugin_name,dep_module,dep_pypi", SQL_PLUGINS)
def test_check_reports_missing_dep(
    monkeypatch: pytest.MonkeyPatch,
    plugin_name: str,
    dep_module: str,
    dep_pypi: str,
) -> None:
    _force_missing(monkeypatch, dep_module)
    results = load_plugins([f"circuitry.runtime_plugins.{plugin_name}"])
    r = results[0].plugin
    assert r is not None
    chk = r.check()
    assert chk.ok is False
    assert f"library:{dep_pypi}" in chk.missing


# ---------------------------------------------------------------------------
# Shared base-class lifecycle (tested via PostgresPlugin + fake psycopg2)
# ---------------------------------------------------------------------------


@dataclass
class _FakeCursor:
    parent: _FakeConn

    def execute(self, sql: str, params: tuple | None = None) -> None:
        self.parent.statements.append((sql, tuple(params) if params else ()))

    def close(self) -> None:
        return None


class _FakeConn:
    """Minimal DBAPI-compatible fake connection."""

    def __init__(self) -> None:
        self.statements: list[tuple[str, tuple]] = []
        self.commits: int = 0
        self.closed: bool = False

    def cursor(self) -> _FakeCursor:
        return _FakeCursor(parent=self)

    def commit(self) -> None:
        self.commits += 1

    def close(self) -> None:
        self.closed = True


def _install_fake_psycopg2(
    monkeypatch: pytest.MonkeyPatch, conn: _FakeConn
) -> None:
    fake = types.ModuleType("psycopg2")
    fake.connect = lambda *args, **kwargs: conn
    monkeypatch.setitem(sys.modules, "psycopg2", fake)


def _make_context(tmp_path: Path) -> PluginContext:
    return PluginContext(
        run_id="run-1",
        orchestration_path=tmp_path / "orch.yml",
        dry_run=False,
        validate_only=False,
        runtime_config={
            "runtime_plugins": {
                "postgres": {"dsn": "postgres://localhost/test"},
            },
        },
    )


def test_postgres_lifecycle_round_trip(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    conn = _FakeConn()
    _install_fake_psycopg2(monkeypatch, conn)

    plugin = postgres_mod.plugin()
    ctx = _make_context(tmp_path)

    plugin.on_run_start(state={"name": "Kenan"}, context=ctx)
    # Schema DDL + initial INSERT INTO runs.
    statements = [s[0] for s in conn.statements]
    assert any("CREATE TABLE IF NOT EXISTS runs" in s for s in statements)
    assert any("CREATE TABLE IF NOT EXISTS effect_results" in s for s in statements)
    assert any("INSERT INTO runs" in s for s in statements)
    # Postgres uses %s placeholders.
    insert_sql = next(s for s in statements if "INSERT INTO runs" in s)
    assert "%s" in insert_sql
    assert "?" not in insert_sql

    # An effect completes — should write an effect_results row.
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
    effect_inserts = [
        s for s in conn.statements if "INSERT INTO effect_results" in s[0]
    ]
    assert len(effect_inserts) == 1
    sql, params = effect_inserts[0]
    # state_path, effect_name positional check.
    assert "prime.greet" in params
    assert "greet" in params
    # effect_type derived from meta.prompt_type / prompt_sent.
    assert "prompt" in params

    plugin.on_run_success(state={}, context=ctx)
    # UPDATE runs SET status=success.
    update_sqls = [
        s for s in conn.statements if s[0].startswith("UPDATE runs")
    ]
    assert len(update_sqls) == 1
    assert "success" in update_sqls[0][1]
    assert conn.closed is True


def test_postgres_failure_marks_runs_row(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    conn = _FakeConn()
    _install_fake_psycopg2(monkeypatch, conn)

    plugin = postgres_mod.plugin()
    ctx = _make_context(tmp_path)
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_run_failure(state={}, context=ctx, error="boom")

    update_sqls = [
        s for s in conn.statements if s[0].startswith("UPDATE runs")
    ]
    assert len(update_sqls) == 1
    assert "failed" in update_sqls[0][1]
    assert "boom" in update_sqls[0][1]


def test_postgres_store_raw_dev_default_includes_raw(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("CIRCUITRY_ENV", "dev")
    conn = _FakeConn()
    _install_fake_psycopg2(monkeypatch, conn)

    plugin = postgres_mod.plugin()
    ctx = _make_context(tmp_path)
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_complete(
        state={},
        context=ctx,
        effect_path="prime.tool1",
        effect_result={
            "value": "result",
            "meta": {"raw": {"detail": "verbose data"}, "stdout": "x"},
        },
    )
    effect_insert = next(
        s for s in conn.statements if "INSERT INTO effect_results" in s[0]
    )
    # raw column should NOT be NULL — the raw dict was JSON-encoded.
    raw_param = effect_insert[1][7]  # 8th column = raw
    assert raw_param is not None
    assert "verbose data" in raw_param


def test_postgres_store_raw_prod_default_skips_raw(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("CIRCUITRY_ENV", "prod")
    conn = _FakeConn()
    _install_fake_psycopg2(monkeypatch, conn)

    plugin = postgres_mod.plugin()
    ctx = _make_context(tmp_path)
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_complete(
        state={},
        context=ctx,
        effect_path="prime.tool1",
        effect_result={
            "value": "result",
            "meta": {"raw": {"large": "blob"}, "stdout": "x"},
        },
    )
    effect_insert = next(
        s for s in conn.statements if "INSERT INTO effect_results" in s[0]
    )
    raw_param = effect_insert[1][7]
    assert raw_param is None


def test_postgres_loop_iteration_path_decoded(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    conn = _FakeConn()
    _install_fake_psycopg2(monkeypatch, conn)

    plugin = postgres_mod.plugin()
    ctx = _make_context(tmp_path)
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_effect_complete(
        state={},
        context=ctx,
        effect_path="prime.my_loop.iter_3.handle",
        effect_result={"value": "x", "meta": {}},
    )
    insert = next(
        s for s in conn.statements if "INSERT INTO effect_results" in s[0]
    )
    params = insert[1]
    # Order: run_id, state_path, effect_name, effect_type, parent_path, iter_index, ...
    assert params[1] == "prime.my_loop.iter_3.handle"
    assert params[2] == "handle"  # effect_name
    assert params[4] == "prime.my_loop.iter_3"  # parent_path
    assert params[5] == 3  # iteration_index


# ---------------------------------------------------------------------------
# Cockroachdb shares behaviour with postgres — verify the connection
# string requirement and dialect alias.
# ---------------------------------------------------------------------------


def test_cockroachdb_requires_connection_string(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake = types.ModuleType("psycopg2")
    fake.connect = lambda *a, **k: _FakeConn()
    monkeypatch.setitem(sys.modules, "psycopg2", fake)
    monkeypatch.delenv("CRDB_URL", raising=False)
    monkeypatch.delenv("DATABASE_URL", raising=False)

    plugin = cockroachdb_mod.plugin()
    ctx = PluginContext(
        run_id="r1",
        orchestration_path=tmp_path / "x.yml",
        dry_run=False,
        validate_only=False,
        runtime_config={},
    )
    with pytest.raises(RuntimeError, match="cockroachdb: connection string"):
        plugin.on_run_start(state={}, context=ctx)


# ---------------------------------------------------------------------------
# MySQL DSN parsing
# ---------------------------------------------------------------------------


def test_mysql_dsn_parsed_to_kwargs(monkeypatch: pytest.MonkeyPatch) -> None:
    """Verify _resolve_kwargs converts a mysql:// DSN to driver kwargs
    independent of whether mysql-connector is installed."""
    monkeypatch.setenv(
        "MYSQL_URL", "mysql://alice:secret@db.example:3306/inventory"
    )
    kwargs = mysql_mod._resolve_kwargs({})
    assert kwargs["host"] == "db.example"
    assert kwargs["port"] == 3306
    assert kwargs["user"] == "alice"
    assert kwargs["password"] == "secret"
    assert kwargs["database"] == "inventory"


def test_mysql_env_var_kwargs(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("MYSQL_URL", raising=False)
    monkeypatch.setenv("MYSQL_HOST", "h")
    monkeypatch.setenv("MYSQL_USER", "u")
    monkeypatch.setenv("MYSQL_PASSWORD", "p")
    monkeypatch.setenv("MYSQL_DATABASE", "d")
    monkeypatch.setenv("MYSQL_PORT", "3307")
    kwargs = mysql_mod._resolve_kwargs({})
    assert kwargs == {
        "host": "h", "user": "u", "password": "p",
        "database": "d", "port": 3307,
    }


# ---------------------------------------------------------------------------
# DuckDB schema bootstrap includes the pre_ddl SEQUENCE
# ---------------------------------------------------------------------------


def test_duckdb_schema_creates_sequence(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    conn = _FakeConn()
    fake = types.ModuleType("duckdb")
    fake.connect = lambda *args, **kwargs: conn
    monkeypatch.setitem(sys.modules, "duckdb", fake)

    plugin = duckdb_mod.plugin()
    ctx = PluginContext(
        run_id="r1",
        orchestration_path=tmp_path / "x.yml",
        dry_run=False,
        validate_only=False,
        runtime_config={
            "runtime_plugins": {"duckdb": {"db_path": str(tmp_path / "x.duckdb")}}
        },
    )
    plugin.on_run_start(state={}, context=ctx)
    statements = [s[0] for s in conn.statements]
    seq_idx = next(
        i for i, s in enumerate(statements) if "CREATE SEQUENCE" in s
    )
    runs_idx = next(
        i for i, s in enumerate(statements) if "CREATE TABLE IF NOT EXISTS runs" in s
    )
    # Sequence must come before the runs table DDL — DuckDB checks
    # nextval() at table-creation time.
    assert seq_idx < runs_idx


# ---------------------------------------------------------------------------
# MSSQL DDL + connection string handling
# ---------------------------------------------------------------------------


def test_mssql_requires_connection_string(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    fake = types.ModuleType("pyodbc")
    fake.connect = lambda *a, **k: _FakeConn()
    monkeypatch.setitem(sys.modules, "pyodbc", fake)
    monkeypatch.delenv("MSSQL_URL", raising=False)

    plugin = mssql_mod.plugin()
    ctx = PluginContext(
        run_id="r1",
        orchestration_path=tmp_path / "x.yml",
        dry_run=False,
        validate_only=False,
        runtime_config={},
    )
    with pytest.raises(RuntimeError, match="connection string"):
        plugin.on_run_start(state={}, context=ctx)


# ---------------------------------------------------------------------------
# ClickHouse — different driver model
# ---------------------------------------------------------------------------


class _FakeClickhouseClient:
    def __init__(self) -> None:
        self.commands: list[tuple[str, dict | None]] = []
        self.inserts: list[tuple[str, list, list]] = []
        self.closed = False

    def command(self, sql: str, parameters: dict | None = None) -> None:
        self.commands.append((sql, parameters))

    def insert(
        self, table: str, rows: list, column_names: list,
    ) -> None:
        self.inserts.append((table, rows, column_names))

    def close(self) -> None:
        self.closed = True


def _install_fake_clickhouse(
    monkeypatch: pytest.MonkeyPatch, client: _FakeClickhouseClient
) -> None:
    fake = types.ModuleType("clickhouse_connect")
    fake.get_client = lambda *args, **kwargs: client
    monkeypatch.setitem(sys.modules, "clickhouse_connect", fake)


def _make_clickhouse_context(tmp_path: Path) -> PluginContext:
    return PluginContext(
        run_id="ch-run-1",
        orchestration_path=tmp_path / "orch.yml",
        dry_run=False,
        validate_only=False,
        runtime_config={
            "runtime_plugins": {
                "clickhouse": {"url": "http://default@localhost:8123/test"},
            },
        },
    )


def test_clickhouse_lifecycle_uses_insert_and_alter(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    client = _FakeClickhouseClient()
    _install_fake_clickhouse(monkeypatch, client)

    plugin = clickhouse_mod.plugin()
    ctx = _make_clickhouse_context(tmp_path)

    plugin.on_run_start(state={"name": "k"}, context=ctx)
    # DDL via command(); initial run row via insert().
    assert any("CREATE TABLE IF NOT EXISTS runs" in c[0] for c in client.commands)
    assert any("ENGINE = ReplacingMergeTree" in c[0] for c in client.commands)
    runs_inserts = [i for i in client.inserts if i[0] == "runs"]
    assert len(runs_inserts) == 1
    _, rows, _ = runs_inserts[0]
    assert rows[0][0] == "ch-run-1"
    assert rows[0][2] == "running"

    # Each effect_result gets a plugin-managed monotonic id starting at 1.
    plugin.on_effect_complete(
        state={},
        context=ctx,
        effect_path="prime.greet",
        effect_result={"value": "x", "meta": {"prompt_type": "text"}},
    )
    plugin.on_effect_complete(
        state={},
        context=ctx,
        effect_path="prime.farewell",
        effect_result={"value": "y", "meta": {"prompt_type": "text"}},
    )
    eff = [i for i in client.inserts if i[0] == "effect_results"]
    assert len(eff) == 2
    assert eff[0][1][0][0] == 1  # id of first effect
    assert eff[1][1][0][0] == 2  # id of second effect

    plugin.on_run_success(state={}, context=ctx)
    update_cmds = [c for c in client.commands if "ALTER TABLE runs UPDATE" in c[0]]
    assert len(update_cmds) == 1
    params = update_cmds[0][1]
    assert params is not None
    assert params["status"] == "success"
    assert params["run_id"] == "ch-run-1"
    assert client.closed is True


def test_clickhouse_url_parsed_to_kwargs(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(
        "CLICKHOUSE_URL", "https://alice:secret@ch.example:8443/analytics"
    )
    kwargs = clickhouse_mod._resolve_client_kwargs({})
    assert kwargs["host"] == "ch.example"
    assert kwargs["port"] == 8443
    assert kwargs["username"] == "alice"
    assert kwargs["password"] == "secret"
    assert kwargs["database"] == "analytics"
    assert kwargs["secure"] is True


def test_clickhouse_failure_uses_alter_with_error(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    client = _FakeClickhouseClient()
    _install_fake_clickhouse(monkeypatch, client)

    plugin = clickhouse_mod.plugin()
    ctx = _make_clickhouse_context(tmp_path)
    plugin.on_run_start(state={}, context=ctx)
    plugin.on_run_failure(state={}, context=ctx, error="kaboom")

    alter = next(c for c in client.commands if "ALTER TABLE runs" in c[0])
    assert alter[1]["status"] == "failed"
    assert alter[1]["error"] == "kaboom"
