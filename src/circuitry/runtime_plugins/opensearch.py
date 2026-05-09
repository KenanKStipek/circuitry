"""OpenSearch persistence runtime plugin.

Optional dep: ``opensearch-py``. Install with
``pip install circuitry-cof[opensearch]``.

OpenSearch is API-compatible with the older Elasticsearch versions but
maintained separately by the OSS community. The plugin shape mirrors
the elasticsearch plugin — same snapshot semantics, different driver.

Config sources (priority order):
  1. Env var ``OPENSEARCH_URL`` /
     ``runtime_plugins.opensearch.url``.
  2. ``OPENSEARCH_USER`` + ``OPENSEARCH_PASSWORD`` (basic auth) or
     ``runtime_plugins.opensearch.{user,password}``.
  3. ``runtime_plugins.opensearch.index`` (default ``"circuitry-runs"``).
"""

from __future__ import annotations

import importlib.util
import os
from typing import Any
from urllib.parse import urlparse

from ._snapshot_persistence import SnapshotPersistenceBase


def _resolve_config(runtime_config: dict[str, Any]) -> dict[str, Any]:
    cfg = (
        (runtime_config or {}).get("runtime_plugins", {}).get("opensearch", {})
    )
    return {
        "url": os.environ.get("OPENSEARCH_URL") or cfg.get("url"),
        "user": os.environ.get("OPENSEARCH_USER") or cfg.get("user"),
        "password": (
            os.environ.get("OPENSEARCH_PASSWORD") or cfg.get("password")
        ),
        "index": cfg.get("index") or "circuitry-runs",
    }


class OpensearchPlugin(SnapshotPersistenceBase):
    name: str = "opensearch"

    def __init__(self) -> None:
        super().__init__()
        self._client: Any = None
        self._index: str = ""

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("opensearchpy") is None:
            return False, ["library:opensearch-py"]
        return True, []

    def _init_run(self, context: Any) -> None:
        from opensearchpy import OpenSearch  # type: ignore[import-not-found]

        cfg = _resolve_config(context.runtime_config or {})
        if not cfg["url"]:
            raise RuntimeError(
                "opensearch: URL not configured. Set OPENSEARCH_URL or "
                "runtime.runtime_plugins.opensearch.url."
            )
        parsed = urlparse(cfg["url"])
        host = {
            "host": parsed.hostname or "localhost",
            "port": parsed.port or (443 if parsed.scheme == "https" else 9200),
        }
        kwargs: dict[str, Any] = {
            "hosts": [host],
            "use_ssl": parsed.scheme == "https",
        }
        if cfg["user"] and cfg["password"]:
            kwargs["http_auth"] = (cfg["user"], cfg["password"])
        self._client = OpenSearch(**kwargs)
        self._index = cfg["index"]

    def _upsert_snapshot(self, run_id: str, snapshot: dict[str, Any]) -> None:
        self._client.index(index=self._index, id=run_id, body=snapshot)

    def _teardown(self) -> None:
        if self._client is not None:
            try:
                self._client.close()
            except Exception:
                pass
            self._client = None


def plugin() -> OpensearchPlugin:
    return OpensearchPlugin()
