"""Google Firestore persistence runtime plugin.

Optional dep: ``google-cloud-firestore``. Install with
``pip install circuitry-cof[firestore]``.

Stores one document per run at ``<collection>/<run_id>``. Auth via
``GOOGLE_APPLICATION_CREDENTIALS`` (service account) or the standard
Google Cloud auth chain.

Config sources (priority order):
  1. Env var ``FIRESTORE_COLLECTION`` /
     ``runtime_plugins.firestore.collection`` (default ``"runs"``).
  2. Env var ``FIRESTORE_PROJECT`` /
     ``runtime_plugins.firestore.project`` (defaults to credentials).
  3. Env var ``FIRESTORE_DATABASE`` /
     ``runtime_plugins.firestore.database`` (default ``"(default)"``).
"""

from __future__ import annotations

import importlib.util
import os
from typing import Any

from ._snapshot_persistence import SnapshotPersistenceBase


def _resolve_config(runtime_config: dict[str, Any]) -> dict[str, str]:
    cfg = (runtime_config or {}).get("runtime_plugins", {}).get("firestore", {})
    return {
        "collection": (
            os.environ.get("FIRESTORE_COLLECTION")
            or cfg.get("collection")
            or "runs"
        ),
        "project": os.environ.get("FIRESTORE_PROJECT") or cfg.get("project") or "",
        "database": (
            os.environ.get("FIRESTORE_DATABASE")
            or cfg.get("database")
            or "(default)"
        ),
    }


class FirestorePlugin(SnapshotPersistenceBase):
    name: str = "firestore"

    def __init__(self) -> None:
        super().__init__()
        self._client: Any = None
        self._collection: Any = None

    def _check_dep(self) -> tuple[bool, list[str]]:
        try:
            present = importlib.util.find_spec("google.cloud.firestore") is not None
        except ModuleNotFoundError:
            present = False
        if not present:
            return False, ["library:google-cloud-firestore"]
        return True, []

    def _init_run(self, context: Any) -> None:
        from google.cloud import firestore  # type: ignore[import-not-found]

        cfg = _resolve_config(context.runtime_config or {})
        kwargs: dict[str, Any] = {}
        if cfg["project"]:
            kwargs["project"] = cfg["project"]
        if cfg["database"] and cfg["database"] != "(default)":
            kwargs["database"] = cfg["database"]
        self._client = firestore.Client(**kwargs)
        self._collection = self._client.collection(cfg["collection"])

    def _upsert_snapshot(self, run_id: str, snapshot: dict[str, Any]) -> None:
        # set() with no merge=True replaces the entire document — matches
        # the snapshot semantic.
        self._collection.document(run_id).set(snapshot)

    def _teardown(self) -> None:
        if self._client is not None:
            try:
                self._client.close()
            except Exception:
                pass
            self._client = None
            self._collection = None


def plugin() -> FirestorePlugin:
    return FirestorePlugin()
