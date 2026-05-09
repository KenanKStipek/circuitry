"""Google Cloud Storage persistence runtime plugin.

Stores one blob per run at ``runs/<run_id>.json`` in the configured
bucket. Auth via ``GOOGLE_APPLICATION_CREDENTIALS`` (service account)
or the standard Google Cloud auth chain.

Optional dep: ``google-cloud-storage``. Install with
``pip install circuitry-cof[gcs]``.

Config sources (priority order):
  1. Env var ``GCS_BUCKET`` / ``runtime_plugins.gcs.bucket`` (required).
  2. Env var ``GCS_PREFIX`` / ``runtime_plugins.gcs.prefix``
     (default ``"runs/"``).
  3. ``GCS_PROJECT`` / ``runtime_plugins.gcs.project``.
"""

from __future__ import annotations

import importlib.util
import json
import os
from typing import Any

from ._snapshot_persistence import SnapshotPersistenceBase


def _resolve_config(runtime_config: dict[str, Any]) -> dict[str, str | None]:
    cfg = (runtime_config or {}).get("runtime_plugins", {}).get("gcs", {})
    return {
        "bucket": os.environ.get("GCS_BUCKET") or cfg.get("bucket"),
        "prefix": os.environ.get("GCS_PREFIX") or cfg.get("prefix") or "runs/",
        "project": os.environ.get("GCS_PROJECT") or cfg.get("project") or "",
    }


class GcsPlugin(SnapshotPersistenceBase):
    name: str = "gcs"

    def __init__(self) -> None:
        super().__init__()
        self._client: Any = None
        self._bucket: Any = None
        self._prefix: str = "runs/"

    def _check_dep(self) -> tuple[bool, list[str]]:
        try:
            present = importlib.util.find_spec("google.cloud.storage") is not None
        except ModuleNotFoundError:
            present = False
        if not present:
            return False, ["library:google-cloud-storage"]
        return True, []

    def _init_run(self, context: Any) -> None:
        from google.cloud import storage  # type: ignore[import-not-found]

        cfg = _resolve_config(context.runtime_config or {})
        if not cfg["bucket"]:
            raise RuntimeError(
                "gcs: bucket not configured. Set GCS_BUCKET or "
                "runtime.runtime_plugins.gcs.bucket."
            )
        kwargs: dict[str, Any] = {}
        if cfg["project"]:
            kwargs["project"] = cfg["project"]
        self._client = storage.Client(**kwargs)
        self._bucket = self._client.bucket(cfg["bucket"])
        self._prefix = cfg["prefix"] or "runs/"
        if not self._prefix.endswith("/"):
            self._prefix = self._prefix + "/"

    def _upsert_snapshot(self, run_id: str, snapshot: dict[str, Any]) -> None:
        blob = self._bucket.blob(f"{self._prefix}{run_id}.json")
        blob.upload_from_string(
            json.dumps(snapshot, default=str),
            content_type="application/json",
        )

    def _teardown(self) -> None:
        # google-cloud-storage Client doesn't require explicit close.
        self._client = None
        self._bucket = None


def plugin() -> GcsPlugin:
    return GcsPlugin()
