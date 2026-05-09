"""Cloudflare R2 persistence runtime plugin.

R2 is S3-API-compatible — same boto3 driver, just a different endpoint
and Cloudflare account-scoped credentials.

Optional dep: ``boto3``. Install with ``pip install circuitry-cof[r2]``.

Required:
  - ``R2_ACCOUNT_ID`` env or ``runtime_plugins.r2.account_id``.
  - ``R2_ACCESS_KEY_ID`` env or ``runtime_plugins.r2.access_key_id``.
  - ``R2_SECRET_ACCESS_KEY`` env or ``runtime_plugins.r2.secret_access_key``.
  - ``R2_BUCKET`` env or ``runtime_plugins.r2.bucket``.

Optional:
  - ``runtime_plugins.r2.prefix`` (default ``"runs/"``).
  - ``runtime_plugins.r2.endpoint_url`` (defaults to the standard
    ``https://<account_id>.r2.cloudflarestorage.com``).
"""

from __future__ import annotations

import importlib.util
import json
import os
from typing import Any

from ._snapshot_persistence import SnapshotPersistenceBase


def _resolve_config(runtime_config: dict[str, Any]) -> dict[str, str | None]:
    cfg = (runtime_config or {}).get("runtime_plugins", {}).get("r2", {})
    account_id = os.environ.get("R2_ACCOUNT_ID") or cfg.get("account_id")
    return {
        "account_id": account_id,
        "access_key_id": (
            os.environ.get("R2_ACCESS_KEY_ID") or cfg.get("access_key_id")
        ),
        "secret_access_key": (
            os.environ.get("R2_SECRET_ACCESS_KEY")
            or cfg.get("secret_access_key")
        ),
        "bucket": os.environ.get("R2_BUCKET") or cfg.get("bucket"),
        "prefix": cfg.get("prefix") or "runs/",
        "endpoint_url": (
            cfg.get("endpoint_url")
            or (
                f"https://{account_id}.r2.cloudflarestorage.com"
                if account_id
                else None
            )
        ),
    }


class R2Plugin(SnapshotPersistenceBase):
    name: str = "r2"

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
        for required in ("account_id", "access_key_id", "secret_access_key", "bucket"):
            if not cfg.get(required):
                raise RuntimeError(
                    f"r2: {required} not configured. Set R2_{required.upper()} "
                    f"or runtime.runtime_plugins.r2.{required}."
                )
        self._client = boto3.client(
            "s3",
            endpoint_url=cfg["endpoint_url"],
            aws_access_key_id=cfg["access_key_id"],
            aws_secret_access_key=cfg["secret_access_key"],
            region_name="auto",
        )
        self._bucket = cfg["bucket"]
        self._prefix = cfg["prefix"] or "runs/"
        if not self._prefix.endswith("/"):
            self._prefix = self._prefix + "/"

    def _upsert_snapshot(self, run_id: str, snapshot: dict[str, Any]) -> None:
        body = json.dumps(snapshot, default=str).encode("utf-8")
        self._client.put_object(
            Bucket=self._bucket,
            Key=f"{self._prefix}{run_id}.json",
            Body=body,
            ContentType="application/json",
        )

    def _teardown(self) -> None:
        self._client = None


def plugin() -> R2Plugin:
    return R2Plugin()
