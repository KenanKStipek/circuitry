"""Unit tests for the SurrealDB tool plugin.

The SDK is lazy-imported inside ``execute()``, so these tests inject a fake
``surrealdb`` module via ``sys.modules`` — no server and no real dep needed.
Everything that touches a live SurrealDB lives in
``tests/integration/test_surrealdb_integration.py``.
"""

from __future__ import annotations

import importlib.util
import json
import sys
import types
from pathlib import Path
from typing import Any, ClassVar

import pytest

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.redaction import REDACTED
from circuitry.cli.runtime_shim import RunRequest, run
from circuitry.plugins import build_plugin
from circuitry.plugins.surrealdb import DEFAULT_URL, SurrealDBPlugin


class FakeSurreal:
    """Records every call so tests can assert what the SDK was asked to do."""

    instances: ClassVar[list[FakeSurreal]] = []

    #: Overridable per-test hooks.
    connect_error: BaseException | None = None
    signin_error: BaseException | None = None
    op_error: BaseException | None = None
    op_result: Any = None

    def __init__(self, url: str) -> None:
        if type(self).connect_error is not None:
            raise type(self).connect_error
        self.url = url
        self.calls: list[tuple[str, tuple[Any, ...]]] = []
        self.closed = False
        type(self).instances.append(self)

    # -- auth / session ----------------------------------------------------
    def signin(self, creds: dict[str, str]) -> None:
        self.calls.append(("signin", (creds,)))
        if type(self).signin_error is not None:
            raise type(self).signin_error

    def authenticate(self, token: str) -> None:
        self.calls.append(("authenticate", (token,)))
        if type(self).signin_error is not None:
            raise type(self).signin_error

    def use(self, namespace: str, database: str) -> None:
        self.calls.append(("use", (namespace, database)))

    # -- operations --------------------------------------------------------
    def _op(self, name: str, *args: Any) -> Any:
        self.calls.append((name, args))
        if type(self).op_error is not None:
            raise type(self).op_error
        return type(self).op_result

    def query(self, *args: Any) -> Any:
        return self._op("query", *args)

    def select(self, target: str) -> Any:
        return self._op("select", target)

    def create(self, table: str, data: dict[str, Any]) -> Any:
        return self._op("create", table, data)

    def upsert(self, record: str, data: dict[str, Any]) -> Any:
        return self._op("upsert", record, data)

    def delete(self, record: str) -> Any:
        return self._op("delete", record)

    def close(self) -> None:
        self.closed = True


@pytest.fixture
def fake_sdk(monkeypatch: pytest.MonkeyPatch) -> type[FakeSurreal]:
    """Install a fake ``surrealdb`` module and reset per-test class state."""
    FakeSurreal.instances = []
    FakeSurreal.connect_error = None
    FakeSurreal.signin_error = None
    FakeSurreal.op_error = None
    FakeSurreal.op_result = [{"id": "person:1", "name": "ada"}]

    module = types.ModuleType("surrealdb")
    module.Surreal = FakeSurreal  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "surrealdb", module)

    monkeypatch.setenv("SURREAL_USER", "root")
    monkeypatch.setenv("SURREAL_PASS", "root-secret-value")
    monkeypatch.delenv("SURREAL_TOKEN", raising=False)
    return FakeSurreal


def _plugin() -> SurrealDBPlugin:
    return SurrealDBPlugin(url=DEFAULT_URL, namespace="test_ns", database="test_db")


# ---------------------------------------------------------------------------
# Registration / config
# ---------------------------------------------------------------------------


def test_factory_builds_surrealdb_with_defaults() -> None:
    plugin = build_plugin(plugin_name="surrealdb", runtime={})
    assert plugin.name == "surrealdb"
    assert isinstance(plugin, SurrealDBPlugin)
    assert plugin.url == DEFAULT_URL
    assert plugin.namespace == ""


def test_factory_reads_url_namespace_database_from_runtime_config() -> None:
    plugin = build_plugin(
        plugin_name="surrealdb",
        runtime={
            "plugins": {
                "surrealdb": {
                    "url": "ws://db.internal:8000/rpc",
                    "namespace": "prod",
                    "database": "app",
                }
            }
        },
    )
    assert isinstance(plugin, SurrealDBPlugin)
    assert plugin.url == "ws://db.internal:8000/rpc"
    assert plugin.namespace == "prod"
    assert plugin.database == "app"
    # Display target for verbose/TUI output.
    assert plugin.base_url == "ws://db.internal:8000/rpc"


def test_surrealdb_listed_in_extensions_and_allowlist_gating(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from typer.testing import CliRunner

    from circuitry.cli.app import app

    config_path = tmp_path / "config.json"
    config_path.write_text(
        json.dumps({"enabled_tools": ["clock"]}), encoding="utf-8"
    )
    runner = CliRunner()

    result = runner.invoke(app, ["list", "--extensions", "--json"])
    assert result.exit_code == 0
    payload = json.loads(result.stdout)
    tools = {entry["name"]: entry["status"] for entry in payload["tool_plugins"]}
    assert "surrealdb" in tools

    gated = runner.invoke(
        app, ["list", "--extensions", "--json", "--config", str(config_path)]
    )
    assert gated.exit_code == 0
    gated_tools = {
        entry["name"]: entry["status"] for entry in json.loads(gated.stdout)["tool_plugins"]
    }
    assert gated_tools["surrealdb"] == "disabled (not in allowlist)"
    assert gated_tools["clock"] == "enabled"


# ---------------------------------------------------------------------------
# check()
# ---------------------------------------------------------------------------


def test_check_reports_missing_library_and_host(monkeypatch: pytest.MonkeyPatch) -> None:
    real_find_spec = importlib.util.find_spec

    def fake_find_spec(name: str, *args: Any, **kwargs: Any) -> Any:
        if name == "surrealdb":
            return None
        return real_find_spec(name, *args, **kwargs)

    monkeypatch.setattr(importlib.util, "find_spec", fake_find_spec)
    # The real SDK may be installed locally; check() short-circuits on an
    # already-imported module, so drop it for the duration of this test.
    monkeypatch.delitem(sys.modules, "surrealdb", raising=False)
    monkeypatch.delenv("SURREAL_USER", raising=False)
    monkeypatch.delenv("SURREAL_PASS", raising=False)
    monkeypatch.delenv("SURREAL_TOKEN", raising=False)

    # Port 1 is reserved and never listening — a deterministic unreachable host.
    result = SurrealDBPlugin(url="ws://127.0.0.1:1/rpc").check()

    assert result.ok is False
    assert "library:surrealdb" in result.missing
    assert "host:ws://127.0.0.1:1/rpc" in result.missing
    assert "env:SURREAL_USER" in result.missing
    assert "env:SURREAL_PASS" in result.missing


def test_check_token_satisfies_credential_requirement(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("SURREAL_USER", raising=False)
    monkeypatch.delenv("SURREAL_PASS", raising=False)
    monkeypatch.setenv("SURREAL_TOKEN", "jwt.token.value")

    result = SurrealDBPlugin(url="ws://127.0.0.1:1/rpc").check()

    assert "env:SURREAL_USER" not in result.missing
    assert "env:SURREAL_PASS" not in result.missing


# ---------------------------------------------------------------------------
# Operations
# ---------------------------------------------------------------------------


def test_query_passes_surrealql_and_params(fake_sdk: type[FakeSurreal]) -> None:
    fake_sdk.op_result = [{"result": [{"name": "ada"}], "status": "OK"}]

    result = _plugin().execute(
        params={
            "mode": "query",
            "query": "SELECT * FROM person WHERE name = $name",
            "params": {"name": "ada"},
        }
    )

    db = fake_sdk.instances[-1]
    assert ("query", ("SELECT * FROM person WHERE name = $name", {"name": "ada"})) in db.calls
    assert ("use", ("test_ns", "test_db")) in db.calls
    assert result.raw["mode"] == "query"
    assert result.raw["namespace"] == "test_ns"
    assert db.closed is True


def test_query_without_params_omits_variables(fake_sdk: type[FakeSurreal]) -> None:
    _plugin().execute(params={"mode": "query", "query": "INFO FOR DB"})
    assert ("query", ("INFO FOR DB",)) in fake_sdk.instances[-1].calls


def test_select_create_upsert_delete_dispatch(fake_sdk: type[FakeSurreal]) -> None:
    plugin = _plugin()

    plugin.execute(params={"mode": "select", "target": "person"})
    assert ("select", ("person",)) in fake_sdk.instances[-1].calls

    plugin.execute(params={"mode": "create", "table": "person", "data": {"name": "ada"}})
    assert ("create", ("person", {"name": "ada"})) in fake_sdk.instances[-1].calls

    plugin.execute(
        params={"mode": "upsert", "record": "person:1", "data": {"name": "grace"}}
    )
    assert ("upsert", ("person:1", {"name": "grace"})) in fake_sdk.instances[-1].calls

    plugin.execute(params={"mode": "delete", "record": "person:1"})
    assert ("delete", ("person:1",)) in fake_sdk.instances[-1].calls


def test_select_accepts_record_alias(fake_sdk: type[FakeSurreal]) -> None:
    _plugin().execute(params={"mode": "select", "record": "person:1"})
    assert ("select", ("person:1",)) in fake_sdk.instances[-1].calls


def test_params_override_namespace_and_database(fake_sdk: type[FakeSurreal]) -> None:
    _plugin().execute(
        params={
            "mode": "select",
            "target": "person",
            "namespace": "other_ns",
            "database": "other_db",
        }
    )
    assert ("use", ("other_ns", "other_db")) in fake_sdk.instances[-1].calls


def test_signin_uses_env_user_and_pass(fake_sdk: type[FakeSurreal]) -> None:
    _plugin().execute(params={"mode": "select", "target": "person"})
    assert (
        "signin",
        ({"username": "root", "password": "root-secret-value"},),
    ) in fake_sdk.instances[-1].calls


def test_token_auth_prefers_authenticate(
    fake_sdk: type[FakeSurreal], monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("SURREAL_TOKEN", "jwt.token.value")
    _plugin().execute(params={"mode": "select", "target": "person"})
    calls = fake_sdk.instances[-1].calls
    assert ("authenticate", ("jwt.token.value",)) in calls
    assert not any(name == "signin" for name, _ in calls)


def test_record_ids_are_coerced_to_json_safe_values(
    fake_sdk: type[FakeSurreal],
) -> None:
    class RecordID:
        def __str__(self) -> str:
            return "person:1"

    fake_sdk.op_result = [{"id": RecordID(), "name": "ada"}]

    result = _plugin().execute(params={"mode": "select", "target": "person"})

    assert result.value == [{"id": "person:1", "name": "ada"}]
    json.dumps(result.value)  # must round-trip into run state


# ---------------------------------------------------------------------------
# Argument validation
# ---------------------------------------------------------------------------


def test_unknown_mode_rejected(fake_sdk: type[FakeSurreal]) -> None:
    with pytest.raises(ValueError, match="unknown mode"):
        _plugin().execute(params={"mode": "drop", "table": "person"})


@pytest.mark.parametrize(
    ("params", "match"),
    [
        ({"mode": "query"}, r"query requires"),
        ({"mode": "query", "query": "INFO FOR DB", "params": "nope"}, r"must be a dict"),
        ({"mode": "select"}, r"select requires"),
        ({"mode": "create", "data": {"a": 1}}, r"create requires"),
        ({"mode": "create", "table": "person"}, r"requires params\['data'\]"),
        ({"mode": "upsert", "data": {"a": 1}}, r"upsert requires"),
        ({"mode": "upsert", "record": "person:1"}, r"requires params\['data'\]"),
        ({"mode": "delete"}, r"delete requires"),
    ],
)
def test_invalid_args_rejected_before_connecting(
    fake_sdk: type[FakeSurreal], params: dict[str, Any], match: str
) -> None:
    with pytest.raises(ValueError, match=match):
        _plugin().execute(params=params)
    assert fake_sdk.instances == []


def test_missing_namespace_or_database_rejected(fake_sdk: type[FakeSurreal]) -> None:
    with pytest.raises(ValueError, match="namespace and a database"):
        SurrealDBPlugin(url=DEFAULT_URL).execute(
            params={"mode": "select", "target": "person"}
        )


def test_missing_credentials_are_actionable(
    fake_sdk: type[FakeSurreal], monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("SURREAL_USER", raising=False)
    monkeypatch.delenv("SURREAL_PASS", raising=False)
    monkeypatch.delenv("SURREAL_TOKEN", raising=False)

    with pytest.raises(RuntimeError, match="SURREAL_USER and SURREAL_PASS"):
        _plugin().execute(params={"mode": "select", "target": "person"})


def test_missing_sdk_is_actionable(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setitem(sys.modules, "surrealdb", None)
    monkeypatch.setenv("SURREAL_USER", "root")
    monkeypatch.setenv("SURREAL_PASS", "root")

    with pytest.raises(RuntimeError, match="pip install surrealdb"):
        _plugin().execute(params={"mode": "select", "target": "person"})


def test_readiness_message_avoids_rich_markup_brackets() -> None:
    """`[surrealdb]` in a message is eaten by Rich's console markup."""
    message = SurrealDBPlugin(url="ws://127.0.0.1:1/rpc").check().message or ""
    assert "[" not in message


def test_doctor_message_installs_the_right_sdk_when_rendered(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """`cof doctor`'s missing-deps message must actually install the SDK.

    `runtime.plugins.surrealdb.url` (bracketed) is deliberately not used in
    the message — see `test_readiness_message_avoids_rich_markup_brackets` —
    but the plain `pip install surrealdb` fallback must still name a real,
    installable package so a human copy-pasting it from `cof doctor`'s Rich
    table ends up with the same SDK `circuitry-cof[surrealdb]` would give
    them.
    """
    monkeypatch.setitem(sys.modules, "surrealdb", None)
    monkeypatch.delenv("SURREAL_USER", raising=False)
    monkeypatch.delenv("SURREAL_PASS", raising=False)
    monkeypatch.delenv("SURREAL_TOKEN", raising=False)

    result = SurrealDBPlugin(url="ws://127.0.0.1:1/rpc").check()
    assert result.ok is False

    from rich.console import Console
    from rich.table import Table

    table = Table()
    table.add_column("Missing / message")
    table.add_row(f"{result.missing} — {result.message}")
    console = Console(record=True, width=120)
    console.print(table)
    rendered = console.export_text()

    assert "pip install surrealdb" in rendered
    assert "circuitry-cof" not in rendered  # would need escaping to render safely


# ---------------------------------------------------------------------------
# Error mapping
# ---------------------------------------------------------------------------


def test_connection_refused_maps_to_actionable_error(
    fake_sdk: type[FakeSurreal],
) -> None:
    fake_sdk.connect_error = ConnectionRefusedError("[Errno 111] Connection refused")

    with pytest.raises(RuntimeError, match="cannot connect to ws://localhost:8000/rpc"):
        _plugin().execute(params={"mode": "select", "target": "person"})


def test_auth_failure_maps_to_credentials_hint(fake_sdk: type[FakeSurreal]) -> None:
    fake_sdk.signin_error = Exception("There was a problem with authentication")

    with pytest.raises(RuntimeError, match="authentication rejected"):
        _plugin().execute(params={"mode": "select", "target": "person"})


def test_surrealql_error_raised_from_operation(fake_sdk: type[FakeSurreal]) -> None:
    fake_sdk.op_error = Exception("Parse error: Failed to parse query at line 1")

    with pytest.raises(RuntimeError, match="surrealdb query failed: Parse error"):
        _plugin().execute(params={"mode": "query", "query": "SELEC * FROM person"})


def test_statement_level_error_status_is_raised(fake_sdk: type[FakeSurreal]) -> None:
    fake_sdk.op_result = [{"status": "ERR", "result": "Table 'nope' does not exist"}]

    with pytest.raises(RuntimeError, match="SurrealQL error: Table 'nope' does not exist"):
        _plugin().execute(params={"mode": "query", "query": "SELECT * FROM nope"})


def test_connection_dropped_mid_operation_maps_to_connection_error(
    fake_sdk: type[FakeSurreal],
) -> None:
    fake_sdk.op_error = ConnectionResetError("connection reset by peer")

    with pytest.raises(RuntimeError, match="cannot connect to"):
        _plugin().execute(params={"mode": "select", "target": "person"})


def test_client_is_closed_even_when_operation_fails(
    fake_sdk: type[FakeSurreal],
) -> None:
    fake_sdk.op_error = Exception("boom")

    with pytest.raises(RuntimeError):
        _plugin().execute(params={"mode": "select", "target": "person"})

    assert fake_sdk.instances[-1].closed is True


# ---------------------------------------------------------------------------
# Credential redaction + end-to-end run recording
# ---------------------------------------------------------------------------


_ORCH = """
effects:
  - type: tool
    name: write_person
    provider: surrealdb
    params:
      mode: create
      table: person
      data:
        name: ada
""".strip() + "\n"


def _run_orchestration(tmp_path: Path, cfg: CircuitryConfig) -> Any:
    orch_path = tmp_path / "surreal.yml"
    orch_path.write_text(_ORCH, encoding="utf-8")
    return run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=cfg,
            # check() probes the configured host; no server here, and the
            # point of these tests is what the *run* records.
            skip_preflight=True,
        )
    )


def _config(url: str = DEFAULT_URL) -> CircuitryConfig:
    return CircuitryConfig(
        runtime={
            "plugins": {
                "surrealdb": {
                    "url": url,
                    "namespace": "test_ns",
                    "database": "test_db",
                }
            }
        }
    )


def test_credentials_never_reach_effective_settings_or_state(
    fake_sdk: type[FakeSurreal], tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    secret = "root-secret-value"
    monkeypatch.setenv("SURREAL_PASS", secret)
    monkeypatch.setenv("SURREAL_TOKEN", "")

    result = _run_orchestration(tmp_path, _config())

    assert result.ok is True
    serialized = json.dumps(result.state, default=str)
    assert secret not in serialized
    assert "SURREAL_PASS" not in serialized

    surreal_settings = result.state["runtime"]["effective_settings"]["runtime"]["plugins"][
        "surrealdb"
    ]
    assert surreal_settings == {
        "url": DEFAULT_URL,
        "namespace": "test_ns",
        "database": "test_db",
    }


def test_url_userinfo_is_redacted_in_effective_settings(
    fake_sdk: type[FakeSurreal], tmp_path: Path
) -> None:
    result = _run_orchestration(
        tmp_path, _config("ws://root:hunter2@localhost:8000/rpc")
    )

    assert result.ok is True
    embedded = result.state["runtime"]["effective_settings"]["runtime"]["plugins"][
        "surrealdb"
    ]
    assert "hunter2" not in embedded["url"]
    assert REDACTED in embedded["url"]


def test_failed_operation_records_run_not_ok(
    fake_sdk: type[FakeSurreal], tmp_path: Path
) -> None:
    fake_sdk.connect_error = ConnectionRefusedError("[Errno 111] Connection refused")

    result = _run_orchestration(tmp_path, _config())

    assert result.ok is False
    error = result.state["prime"]["write_person"]["meta"]["error"]
    assert "cannot connect to" in error
