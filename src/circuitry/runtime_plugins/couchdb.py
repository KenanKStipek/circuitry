"""CouchDB persistence runtime plugin.

Talks to CouchDB's HTTP API directly via ``requests`` — no separate
SDK required. Stores one document per run at ``<db>/<run_id>``.
CouchDB's MVCC needs the previous ``_rev`` field on updates; the
plugin tracks it across the run's lifecycle.

Optional dep: ``requests``. Install with ``pip install circuitry-cof[couchdb]``.

Config sources (priority order):
  1. Env var ``COUCHDB_URL`` (full URL incl. credentials and database
     path, e.g. ``http://admin:pw@localhost:5984/circuitry``).
  2. Env var ``COUCHDB_HOST``+``COUCHDB_USER``+``COUCHDB_PASSWORD``+
     ``COUCHDB_DATABASE``.
  3. ``runtime_plugins.couchdb.{url,host,...}`` in config.json.
"""

from __future__ import annotations

import importlib.util
import os
from typing import Any
from urllib.parse import quote

from ._snapshot_persistence import SnapshotPersistenceBase


def _resolve_db_url(runtime_config: dict[str, Any]) -> str:
    cfg = (runtime_config or {}).get("runtime_plugins", {}).get("couchdb", {})
    url = os.environ.get("COUCHDB_URL") or cfg.get("url")
    if url:
        return url.rstrip("/")
    host = os.environ.get("COUCHDB_HOST") or cfg.get("host") or ""
    user = os.environ.get("COUCHDB_USER") or cfg.get("user") or ""
    password = os.environ.get("COUCHDB_PASSWORD") or cfg.get("password") or ""
    database = os.environ.get("COUCHDB_DATABASE") or cfg.get("database") or ""
    if not host or not database:
        return ""
    if not host.startswith(("http://", "https://")):
        host = f"http://{host}"
    auth = f"{quote(user)}:{quote(password)}@" if user else ""
    return f"{host.split('://', 1)[0]}://{auth}{host.split('://', 1)[1].rstrip('/')}/{database}"


class CouchdbPlugin(SnapshotPersistenceBase):
    name: str = "couchdb"

    def __init__(self) -> None:
        super().__init__()
        self._db_url: str = ""
        self._timeout_seconds: int = 30
        self._rev: str | None = None
        self._session: Any = None

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("requests") is None:
            return False, ["library:requests"]
        return True, []

    def _init_run(self, context: Any) -> None:
        import requests  # type: ignore[import-not-found]

        self._db_url = _resolve_db_url(context.runtime_config or {})
        if not self._db_url:
            raise RuntimeError(
                "couchdb: database URL not configured. Set COUCHDB_URL or "
                "COUCHDB_HOST + COUCHDB_DATABASE."
            )
        self._session = requests.Session()
        self._rev = None

    def _upsert_snapshot(self, run_id: str, snapshot: dict[str, Any]) -> None:
        doc = dict(snapshot)
        if self._rev is not None:
            doc["_rev"] = self._rev
        encoded_id = quote(run_id, safe="")
        url = f"{self._db_url}/{encoded_id}"
        resp = self._session.put(url, json=doc, timeout=self._timeout_seconds)
        if resp.status_code >= 400:
            raise RuntimeError(
                f"couchdb upsert failed (HTTP {resp.status_code}): {resp.text[:500]}"
            )
        body = resp.json() if resp.text else {}
        self._rev = body.get("rev") or self._rev

    def _teardown(self) -> None:
        if self._session is not None:
            try:
                self._session.close()
            except Exception:
                pass
            self._session = None
            self._rev = None


def plugin() -> CouchdbPlugin:
    return CouchdbPlugin()
