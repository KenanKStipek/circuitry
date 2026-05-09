"""Azure Blob Storage persistence runtime plugin.

Stores one blob per run at ``runs/<run_id>.json`` in the configured
container.

Optional dep: ``azure-storage-blob``. Install with
``pip install circuitry-cof[azure-blob]``.

Auth (priority order):
  1. ``AZURE_STORAGE_CONNECTION_STRING`` env or
     ``runtime_plugins.azure-blob.connection_string``.
  2. ``AZURE_STORAGE_ACCOUNT_URL`` env or
     ``runtime_plugins.azure-blob.account_url`` (uses the
     ``DefaultAzureCredential`` chain — managed identity / CLI / etc).

Required:
  - ``AZURE_BLOB_CONTAINER`` / ``runtime_plugins.azure-blob.container``.

Optional:
  - ``runtime_plugins.azure-blob.prefix`` (default ``"runs/"``).
"""

from __future__ import annotations

import importlib.util
import json
import os
from typing import Any

from ._snapshot_persistence import SnapshotPersistenceBase


def _resolve_config(runtime_config: dict[str, Any]) -> dict[str, str | None]:
    cfg = (runtime_config or {}).get("runtime_plugins", {}).get("azure-blob", {})
    return {
        "connection_string": (
            os.environ.get("AZURE_STORAGE_CONNECTION_STRING")
            or cfg.get("connection_string")
        ),
        "account_url": (
            os.environ.get("AZURE_STORAGE_ACCOUNT_URL")
            or cfg.get("account_url")
        ),
        "container": (
            os.environ.get("AZURE_BLOB_CONTAINER") or cfg.get("container")
        ),
        "prefix": cfg.get("prefix") or "runs/",
    }


class AzureBlobPlugin(SnapshotPersistenceBase):
    name: str = "azure-blob"

    def __init__(self) -> None:
        super().__init__()
        self._service: Any = None
        self._container_client: Any = None
        self._prefix: str = "runs/"

    def _check_dep(self) -> tuple[bool, list[str]]:
        try:
            present = importlib.util.find_spec("azure.storage.blob") is not None
        except ModuleNotFoundError:
            present = False
        if not present:
            return False, ["library:azure-storage-blob"]
        return True, []

    def _init_run(self, context: Any) -> None:
        from azure.storage.blob import BlobServiceClient  # type: ignore[import-not-found]

        cfg = _resolve_config(context.runtime_config or {})
        if not cfg["container"]:
            raise RuntimeError(
                "azure-blob: container not configured. Set "
                "AZURE_BLOB_CONTAINER or runtime.runtime_plugins.azure-blob.container."
            )
        if cfg["connection_string"]:
            self._service = BlobServiceClient.from_connection_string(
                cfg["connection_string"]
            )
        elif cfg["account_url"]:
            try:
                from azure.identity import (  # type: ignore[import-not-found]
                    DefaultAzureCredential,
                )
                creds = DefaultAzureCredential()
            except ImportError as exc:
                raise RuntimeError(
                    "azure-blob: account_url requires azure-identity. "
                    "Install with: pip install azure-identity"
                ) from exc
            self._service = BlobServiceClient(
                account_url=cfg["account_url"], credential=creds
            )
        else:
            raise RuntimeError(
                "azure-blob: provide AZURE_STORAGE_CONNECTION_STRING or "
                "AZURE_STORAGE_ACCOUNT_URL."
            )
        self._container_client = self._service.get_container_client(
            cfg["container"]
        )
        self._prefix = cfg["prefix"] or "runs/"
        if not self._prefix.endswith("/"):
            self._prefix = self._prefix + "/"

    def _upsert_snapshot(self, run_id: str, snapshot: dict[str, Any]) -> None:
        blob = self._container_client.get_blob_client(
            f"{self._prefix}{run_id}.json"
        )
        body = json.dumps(snapshot, default=str).encode("utf-8")
        blob.upload_blob(body, overwrite=True)

    def _teardown(self) -> None:
        if self._service is not None:
            try:
                self._service.close()
            except Exception:
                pass
            self._service = None
            self._container_client = None


def plugin() -> AzureBlobPlugin:
    return AzureBlobPlugin()
