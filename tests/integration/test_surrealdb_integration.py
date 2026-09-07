"""Live SurrealDB round-trip for the ``surrealdb`` tool plugin.

Opt-in: set ``CIRCUITRY_RUN_INTEGRATION=1`` and point ``SURREAL_URL`` at a
running server (defaults to ``ws://localhost:8000/rpc``). Credentials come
from the same env vars the plugin uses in production — ``SURREAL_USER`` /
``SURREAL_PASS`` (or ``SURREAL_TOKEN``).

Local server:

    docker run --rm -p 8000:8000 surrealdb/surrealdb:latest \\
        start --user root --pass root

In CI this runs against a SurrealDB service container; see
``docs/plugins/surrealdb.md``.
"""

from __future__ import annotations

import os
import uuid
from typing import Any

import pytest

from circuitry.plugins.surrealdb import DEFAULT_URL, SurrealDBPlugin

if os.getenv("CIRCUITRY_RUN_INTEGRATION") != "1":
    pytest.skip(
        "Integration tests disabled. Set CIRCUITRY_RUN_INTEGRATION=1 to enable.",
        allow_module_level=True,
    )

pytestmark = pytest.mark.integration

_URL = os.getenv("SURREAL_URL", DEFAULT_URL)
_NAMESPACE = os.getenv("SURREAL_NS", "circuitry_test")
_DATABASE = os.getenv("SURREAL_DB", "circuitry_test")


@pytest.fixture(scope="module")
def plugin() -> SurrealDBPlugin:
    instance = SurrealDBPlugin(url=_URL, namespace=_NAMESPACE, database=_DATABASE)
    result = instance.check()
    if not result.ok:
        pytest.skip(f"SurrealDB not ready: {', '.join(result.missing)}")
    return instance


@pytest.fixture
def table() -> str:
    """A fresh table per test so runs don't collide on a shared server."""
    return f"person_{uuid.uuid4().hex[:8]}"


@pytest.fixture(scope="module")
def scoped_users(plugin: SurrealDBPlugin) -> dict[str, str]:
    """Define a DATABASE-scoped EDITOR and VIEWER user for auth tests.

    Connects as the root user (default dev credentials) to run the
    ``DEFINE USER`` statements, then hands back generated passwords for the
    two scoped users. ``plugin`` is depended on only to reuse its readiness
    skip if the server isn't up.
    """
    from surrealdb import Surreal  # type: ignore[import-not-found]

    editor_password = f"editor-{uuid.uuid4().hex}"
    viewer_password = f"viewer-{uuid.uuid4().hex}"

    root_user = os.getenv("SURREAL_USER", "root")
    root_password = os.getenv("SURREAL_PASS", "root")

    client = Surreal(_URL)
    try:
        client.signin({"username": root_user, "password": root_password})
        client.use(_NAMESPACE, _DATABASE)
        # Passwords are hex-only (uuid4().hex), so string interpolation into
        # SurrealQL here can't break out of the quoted literal.
        client.query(
            f"DEFINE USER editor_user ON DATABASE PASSWORD '{editor_password}' "
            "ROLES EDITOR"
        )
        client.query(
            f"DEFINE USER viewer_user ON DATABASE PASSWORD '{viewer_password}' "
            "ROLES VIEWER"
        )
    finally:
        client.close()

    return {"editor_password": editor_password, "viewer_password": viewer_password}


def _first_record(value: Any) -> dict[str, Any]:
    if isinstance(value, list):
        assert value, f"expected at least one record, got {value!r}"
        return value[0]
    assert isinstance(value, dict), f"expected a record, got {value!r}"
    return value


def test_create_select_upsert_delete_round_trip(
    plugin: SurrealDBPlugin, table: str
) -> None:
    created = plugin.execute(
        params={"mode": "create", "table": table, "data": {"name": "ada", "score": 1}}
    )
    record_id = str(_first_record(created.value)["id"])
    assert created.raw["mode"] == "create"

    selected = plugin.execute(params={"mode": "select", "target": table})
    assert _first_record(selected.value)["name"] == "ada"

    upserted = plugin.execute(
        params={"mode": "upsert", "record": record_id, "data": {"name": "grace", "score": 2}}
    )
    assert _first_record(upserted.value)["name"] == "grace"

    after_upsert = plugin.execute(params={"mode": "select", "target": record_id})
    assert _first_record(after_upsert.value)["score"] == 2

    plugin.execute(params={"mode": "delete", "record": record_id})

    remaining = plugin.execute(params={"mode": "select", "target": table})
    assert remaining.value in ([], None)


def test_query_with_bound_params(plugin: SurrealDBPlugin, table: str) -> None:
    plugin.execute(
        params={"mode": "create", "table": table, "data": {"name": "ada", "score": 10}}
    )
    plugin.execute(
        params={"mode": "create", "table": table, "data": {"name": "grace", "score": 20}}
    )

    result = plugin.execute(
        params={
            "mode": "query",
            "query": f"SELECT name FROM {table} WHERE score > $floor",
            "params": {"floor": 15},
        }
    )

    flattened = str(result.value)
    assert "grace" in flattened
    assert "ada" not in flattened


def test_surrealql_syntax_error_is_actionable(plugin: SurrealDBPlugin) -> None:
    with pytest.raises(RuntimeError) as excinfo:
        plugin.execute(params={"mode": "query", "query": "SELEC * FROM person"})
    assert "surrealdb" in str(excinfo.value)


def test_bad_credentials_are_rejected(
    table: str, monkeypatch: pytest.MonkeyPatch, plugin: SurrealDBPlugin
) -> None:
    monkeypatch.delenv("SURREAL_TOKEN", raising=False)
    monkeypatch.setenv("SURREAL_USER", "not-a-user")
    monkeypatch.setenv("SURREAL_PASS", "not-a-password")

    with pytest.raises(RuntimeError, match="authentication rejected"):
        plugin.execute(params={"mode": "select", "target": table})


def test_connection_refused_is_actionable() -> None:
    unreachable = SurrealDBPlugin(
        url="ws://127.0.0.1:1/rpc", namespace=_NAMESPACE, database=_DATABASE
    )
    with pytest.raises(RuntimeError, match="cannot connect to"):
        unreachable.execute(params={"mode": "select", "target": "person"})


def test_database_scoped_editor_and_viewer_authenticate_with_correct_grants(
    scoped_users: dict[str, str],
    table: str,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A DATABASE-scoped EDITOR/VIEWER pair authenticates via the fixed
    ``_authenticate()`` (namespace+database now reach the signin payload),
    and each user's grants are enforced by the server: EDITOR can create,
    VIEWER's create is denied but its select still works."""
    monkeypatch.delenv("SURREAL_TOKEN", raising=False)
    monkeypatch.setenv("SURREAL_USER", "editor_user")
    monkeypatch.setenv("SURREAL_PASS", scoped_users["editor_password"])

    editor = SurrealDBPlugin(url=_URL, namespace=_NAMESPACE, database=_DATABASE)
    created = editor.execute(
        params={"mode": "create", "table": table, "data": {"name": "ada"}}
    )
    assert created.raw["mode"] == "create"
    assert _first_record(created.value)["name"] == "ada"

    monkeypatch.setenv("SURREAL_USER", "viewer_user")
    monkeypatch.setenv("SURREAL_PASS", scoped_users["viewer_password"])

    viewer = SurrealDBPlugin(url=_URL, namespace=_NAMESPACE, database=_DATABASE)
    with pytest.raises(RuntimeError):
        viewer.execute(
            params={"mode": "create", "table": table, "data": {"name": "grace"}}
        )

    selected = viewer.execute(params={"mode": "select", "target": table})
    assert _first_record(selected.value)["name"] == "ada"
