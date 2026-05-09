"""Elasticsearch persistence runtime plugin.

Optional dep: ``elasticsearch``. Install with
``pip install circuitry-cof[elasticsearch]``.

Indexes one document per run at ``<index>/_doc/<run_id>``. Uses
``client.index(...)`` so subsequent writes for the same run_id
overwrite (Elasticsearch routes by ``_id`` and replaces the document).

Config sources (priority order):
  1. Env var ``ES_URL`` / ``ELASTICSEARCH_URL`` /
     ``runtime_plugins.elasticsearch.url``.
  2. ``ELASTICSEARCH_API_KEY`` env or ``runtime_plugins.elasticsearch.api_key``.
  3. ``runtime_plugins.elasticsearch.index`` (default ``"circuitry-runs"``).
"""

from __future__ import annotations

import importlib.util
import os
from typing import Any

from ._snapshot_persistence import SnapshotPersistenceBase


def _resolve_config(runtime_config: dict[str, Any]) -> dict[str, str | None]:
    cfg = (
        (runtime_config or {}).get("runtime_plugins", {}).get("elasticsearch", {})
    )
    return {
        "url": (
            os.environ.get("ES_URL")
            or os.environ.get("ELASTICSEARCH_URL")
            or cfg.get("url")
        ),
        "api_key": os.environ.get("ELASTICSEARCH_API_KEY") or cfg.get("api_key"),
        "index": cfg.get("index") or "circuitry-runs",
    }


class ElasticsearchPlugin(SnapshotPersistenceBase):
    name: str = "elasticsearch"

    def __init__(self) -> None:
        super().__init__()
        self._client: Any = None
        self._index: str = ""

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("elasticsearch") is None:
            return False, ["library:elasticsearch"]
        return True, []

    def _init_run(self, context: Any) -> None:
        from elasticsearch import Elasticsearch  # type: ignore[import-not-found]

        cfg = _resolve_config(context.runtime_config or {})
        if not cfg["url"]:
            raise RuntimeError(
                "elasticsearch: URL not configured. Set ES_URL or "
                "runtime.runtime_plugins.elasticsearch.url."
            )
        kwargs: dict[str, Any] = {"hosts": [cfg["url"]]}
        if cfg["api_key"]:
            kwargs["api_key"] = cfg["api_key"]
        self._client = Elasticsearch(**kwargs)
        self._index = cfg["index"] or "circuitry-runs"

    def _upsert_snapshot(self, run_id: str, snapshot: dict[str, Any]) -> None:
        # client.index() with an explicit id replaces the document on
        # subsequent writes — exactly the snapshot semantic we want.
        self._client.index(index=self._index, id=run_id, document=snapshot)

    def _teardown(self) -> None:
        if self._client is not None:
            try:
                self._client.close()
            except Exception:
                pass
            self._client = None


def plugin() -> ElasticsearchPlugin:
    return ElasticsearchPlugin()
