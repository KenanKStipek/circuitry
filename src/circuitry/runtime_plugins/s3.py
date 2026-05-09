"""S3 persistence runtime plugin.

Stores one blob per run at ``runs/<run_id>.json`` in the configured
bucket. Distinct from the ``s3`` *tool plugin* (the LLM-callable
storage surface) — both share the name but live in different
namespaces (PLUGIN_REGISTRY for tools, runtime_plugins/ for runtime).

Optional dep: ``boto3``. Install with ``pip install circuitry-cof[s3-runtime]``.

Config sources (priority order):
  1. Env var ``S3_BUCKET`` / ``runtime_plugins.s3.bucket`` (required).
  2. Env var ``S3_PREFIX`` / ``runtime_plugins.s3.prefix``
     (default ``"runs/"``).
  3. Env var ``AWS_REGION`` / ``runtime_plugins.s3.region``.
  4. Standard AWS credentials chain for auth.
"""

from __future__ import annotations

import importlib.util
import json
import os
from typing import Any

from ._snapshot_persistence import SnapshotPersistenceBase


def _resolve_config(runtime_config: dict[str, Any]) -> dict[str, str | None]:
    cfg = (runtime_config or {}).get("runtime_plugins", {}).get("s3", {})
    return {
        "bucket": os.environ.get("S3_BUCKET") or cfg.get("bucket"),
        "prefix": os.environ.get("S3_PREFIX") or cfg.get("prefix") or "runs/",
        "region": (
            os.environ.get("AWS_REGION")
            or os.environ.get("S3_REGION")
            or cfg.get("region")
        ),
    }


class S3Plugin(SnapshotPersistenceBase):
    name: str = "s3"

    def __init__(self) -> None:
        super().__init__()
        self._client: Any = None
        self._bucket: str | None = None
        self._prefix: str = "runs/"

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("boto3") is None:
            return False, ["library:boto3"]
        return True, []

    def _init_run(self, context: Any) -> None:
        import boto3  # type: ignore[import-not-found]

        cfg = _resolve_config(context.runtime_config or {})
        if not cfg["bucket"]:
            raise RuntimeError(
                "s3: bucket not configured. Set S3_BUCKET or "
                "runtime.runtime_plugins.s3.bucket."
            )
        kwargs: dict[str, Any] = {}
        if cfg["region"]:
            kwargs["region_name"] = cfg["region"]
        self._client = boto3.client("s3", **kwargs)
        self._bucket = cfg["bucket"]
        self._prefix = cfg["prefix"] or "runs/"
        if not self._prefix.endswith("/"):
            self._prefix = self._prefix + "/"

    def _upsert_snapshot(self, run_id: str, snapshot: dict[str, Any]) -> None:
        key = f"{self._prefix}{run_id}.json"
        body = json.dumps(snapshot, default=str).encode("utf-8")
        self._client.put_object(
            Bucket=self._bucket, Key=key, Body=body,
            ContentType="application/json",
        )

    def _teardown(self) -> None:
        # boto3 clients are stateless wrappers over botocore.
        self._client = None


def plugin() -> S3Plugin:
    return S3Plugin()
