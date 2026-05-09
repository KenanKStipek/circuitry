"""DynamoDB persistence runtime plugin.

Optional dep: ``boto3``. Install with ``pip install circuitry-cof[dynamodb]``.

Stores one item per run, keyed by ``run_id`` (partition key). The
table must already exist with a ``run_id`` (string) partition key —
the plugin doesn't create tables (table creation is an
infrastructure concern best handled via Terraform / CloudFormation).

Config sources (priority order):
  1. Env var ``DYNAMODB_TABLE`` / ``runtime_plugins.dynamodb.table``
     (required).
  2. Env var ``DYNAMODB_REGION`` / ``runtime_plugins.dynamodb.region``.
  3. Standard boto3 credential chain for auth.
"""

from __future__ import annotations

import importlib.util
import json
import os
from typing import Any

from ._snapshot_persistence import SnapshotPersistenceBase


def _resolve_config(runtime_config: dict[str, Any]) -> dict[str, str | None]:
    cfg = (runtime_config or {}).get("runtime_plugins", {}).get("dynamodb", {})
    return {
        "table": os.environ.get("DYNAMODB_TABLE") or cfg.get("table"),
        "region": os.environ.get("DYNAMODB_REGION") or cfg.get("region"),
    }


class DynamodbPlugin(SnapshotPersistenceBase):
    name: str = "dynamodb"

    def __init__(self) -> None:
        super().__init__()
        self._table: Any = None

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("boto3") is None:
            return False, ["library:boto3"]
        return True, []

    def _init_run(self, context: Any) -> None:
        import boto3  # type: ignore[import-not-found]

        cfg = _resolve_config(context.runtime_config or {})
        if not cfg["table"]:
            raise RuntimeError(
                "dynamodb: table not configured. Set DYNAMODB_TABLE or "
                "runtime.runtime_plugins.dynamodb.table."
            )
        kwargs: dict[str, Any] = {}
        if cfg["region"]:
            kwargs["region_name"] = cfg["region"]
        resource = boto3.resource("dynamodb", **kwargs)
        self._table = resource.Table(cfg["table"])

    def _upsert_snapshot(self, run_id: str, snapshot: dict[str, Any]) -> None:
        # DynamoDB attribute values must be one of its native types
        # (S/N/B/BOOL/NULL/L/M). Rather than walk the state dict
        # converting nested values, serialise ``state`` to a JSON
        # string. The other fields are already string / null compatible.
        item: dict[str, Any] = {
            "run_id": run_id,
            "orchestration_path": snapshot.get("orchestration_path") or "",
            "status": snapshot.get("status") or "",
            "started_at": snapshot.get("started_at") or "",
            "ended_at": snapshot.get("ended_at") or "",
            "error": snapshot.get("error") or "",
            "state": json.dumps(snapshot.get("state") or {}, default=str),
        }
        self._table.put_item(Item=item)

    def _teardown(self) -> None:
        # boto3 resources are stateless; nothing to close.
        self._table = None


def plugin() -> DynamodbPlugin:
    return DynamodbPlugin()
