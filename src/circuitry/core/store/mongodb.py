"""MongoDB state persistence backend.

Optional dep: ``pymongo``. Install with ``pip install circuitry-cof[mongodb]``.

One document per run, ``_id = run_id``, in the configured database and
collection. ``load_latest_state`` returns the most recent document for an
orchestration (``created_at`` descending).

This is the ``PersistenceBackend`` (load-latest + save-snapshot) sibling of
the write-only ``mongodb`` runtime plugin in
``circuitry.runtime_plugins.mongodb``; the two are independent and can be
enabled at the same time.

Credentials live in the connection URI (``mongodb://user:pass@host``), so
``describe()`` — which is embedded verbatim in ``runtime.persistence`` —
reports a userinfo-stripped URI. The un-redacted URI never leaves this
module.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any
from urllib.parse import urlsplit, urlunsplit

_REDACTED = "***REDACTED***"


def sanitize_uri(uri: str) -> str:
    """Strip ``user:pass@`` userinfo from a Mongo connection URI."""
    try:
        parts = urlsplit(uri)
    except ValueError:
        return _REDACTED
    if not parts.scheme or not parts.netloc or "@" not in parts.netloc:
        return uri
    _, _, host = parts.netloc.rpartition("@")
    return urlunsplit(
        (parts.scheme, f"{_REDACTED}@{host}", parts.path, parts.query, parts.fragment)
    )


@dataclass(frozen=True)
class MongodbStatePersistence:
    uri: str
    database: str = "circuitry"
    collection: str = "circuitry_runs"

    @property
    def backend_name(self) -> str:
        return "mongodb"

    @staticmethod
    def from_config(config: dict[str, Any]) -> MongodbStatePersistence:
        uri = str(config.get("uri") or config.get("dsn") or "").strip()
        if not uri:
            raise ValueError(
                "Persistence backend 'mongodb' requires runtime.persistence.uri"
            )

        database = str(config.get("database") or "circuitry").strip()
        if not database:
            raise ValueError(
                "runtime.persistence.database must be a non-empty string"
            )

        collection = str(config.get("collection") or "circuitry_runs").strip()
        if not collection:
            raise ValueError(
                "runtime.persistence.collection must be a non-empty string"
            )

        return MongodbStatePersistence(
            uri=uri, database=database, collection=collection
        )

    def describe(self) -> dict[str, Any]:
        return {
            "backend": self.backend_name,
            "database": self.database,
            "collection": self.collection,
            "uri": sanitize_uri(self.uri),
        }

    def load_latest_state(self, *, orchestration_path: str) -> dict[str, Any] | None:
        try:
            client = self._connect()
            try:
                collection = client[self.database][self.collection]
                doc = collection.find_one(
                    {"orchestration_path": orchestration_path},
                    sort=[("created_at", -1)],
                )
            finally:
                self._close(client)
        except Exception as e:
            raise RuntimeError(
                "MongoDB state load failed for orchestration "
                f"{orchestration_path}: {e}"
            ) from e

        if not doc:
            return None

        state = doc.get("state")
        if isinstance(state, dict):
            return state
        raise RuntimeError(
            f"Persisted state has unsupported type: {type(state).__name__}"
        )

    def save_run_snapshot(
        self,
        *,
        orchestration_path: str,
        run_id: str,
        ok: bool,
        error: str | None,
        state: dict[str, Any],
    ) -> None:
        doc = {
            "_id": run_id,
            "run_id": run_id,
            "orchestration_path": orchestration_path,
            "created_at": datetime.now(timezone.utc).isoformat(),
            "ok": ok,
            "error": error,
            "state": state,
        }
        try:
            client = self._connect()
            try:
                collection = client[self.database][self.collection]
                collection.replace_one({"_id": run_id}, doc, upsert=True)
            finally:
                self._close(client)
        except Exception as e:
            raise RuntimeError(
                f"MongoDB state save failed for run_id={run_id}: {e}"
            ) from e

    def _connect(self) -> Any:
        try:
            from pymongo import MongoClient  # type: ignore[import-not-found]
        except ImportError as e:
            raise RuntimeError(
                "pymongo is required for mongodb persistence. Install with: "
                "pip install pymongo"
            ) from e

        return MongoClient(self.uri)

    @staticmethod
    def _close(client: Any) -> None:
        close = getattr(client, "close", None)
        if close is None:
            return
        try:
            close()
        except Exception:
            pass
