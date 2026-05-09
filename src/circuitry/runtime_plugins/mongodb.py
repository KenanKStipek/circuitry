"""MongoDB persistence runtime plugin.

Optional dep: ``pymongo``. Install with ``pip install circuitry-cof[mongodb]``.

Stores one document per run, keyed by ``_id = run_id``, in the
configured collection. Uses ``replace_one(..., upsert=True)`` so the
final-status snapshot replaces the in-progress one.

Config sources (priority order):
  1. Env var ``MONGODB_URI``.
  2. ``runtime.runtime_plugins.mongodb.uri`` in config.json.
  3. Default ``mongodb://localhost:27017``.

  4. Env var ``MONGODB_DATABASE`` / runtime config ``database``
     (default ``"circuitry"``).
  5. Env var ``MONGODB_COLLECTION`` / runtime config ``collection``
     (default ``"runs"``).
"""

from __future__ import annotations

import importlib.util
import os
from typing import Any

from ._snapshot_persistence import SnapshotPersistenceBase


def _resolve_config(runtime_config: dict[str, Any]) -> dict[str, str]:
    cfg = (runtime_config or {}).get("runtime_plugins", {}).get("mongodb", {})
    return {
        "uri": (
            os.environ.get("MONGODB_URI")
            or cfg.get("uri")
            or "mongodb://localhost:27017"
        ),
        "database": (
            os.environ.get("MONGODB_DATABASE")
            or cfg.get("database")
            or "circuitry"
        ),
        "collection": (
            os.environ.get("MONGODB_COLLECTION")
            or cfg.get("collection")
            or "runs"
        ),
    }


class MongodbPlugin(SnapshotPersistenceBase):
    name: str = "mongodb"

    def __init__(self) -> None:
        super().__init__()
        self._client: Any = None
        self._collection: Any = None

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("pymongo") is None:
            return False, ["library:pymongo"]
        return True, []

    def _init_run(self, context: Any) -> None:
        from pymongo import MongoClient  # type: ignore[import-not-found]

        cfg = _resolve_config(context.runtime_config or {})
        self._client = MongoClient(cfg["uri"])
        self._collection = self._client[cfg["database"]][cfg["collection"]]

    def _upsert_snapshot(self, run_id: str, snapshot: dict[str, Any]) -> None:
        # Use _id = run_id so MongoDB's natural unique index handles
        # deduplication. Copy the snapshot to avoid mutating the caller's
        # dict with the _id field.
        doc = dict(snapshot)
        doc["_id"] = run_id
        self._collection.replace_one({"_id": run_id}, doc, upsert=True)

    def _teardown(self) -> None:
        if self._client is not None:
            try:
                self._client.close()
            except Exception:
                pass
            self._client = None
            self._collection = None


def plugin() -> MongodbPlugin:
    return MongodbPlugin()
