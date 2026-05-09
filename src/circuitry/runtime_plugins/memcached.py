"""Memcached persistence runtime plugin.

Optional dep: ``pymemcache``. Install with
``pip install circuitry-cof[memcached]``.

Stores the JSON-serialised snapshot at ``run:<run_id>`` with optional
expiration. Memcached has no native iteration / scan, so this is best
suited as ephemeral cache rather than authoritative storage.

Config sources (priority order):
  1. Env var ``MEMCACHED_URL`` (e.g. ``localhost:11211``).
  2. ``runtime_plugins.memcached.url`` in config.json.
  3. Default ``localhost:11211``.

Optional:
  - ``runtime_plugins.memcached.ttl_seconds`` (int): expiration.
"""

from __future__ import annotations

import importlib.util
import json
import os
from typing import Any

from ._snapshot_persistence import SnapshotPersistenceBase


def _parse_host_port(spec: str) -> tuple[str, int]:
    if "://" in spec:
        spec = spec.split("://", 1)[1]
    if ":" in spec:
        host, port = spec.split(":", 1)
        return host, int(port)
    return spec, 11211


class MemcachedPlugin(SnapshotPersistenceBase):
    name: str = "memcached"

    def __init__(self) -> None:
        super().__init__()
        self._client: Any = None
        self._ttl: int = 0

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("pymemcache") is None:
            return False, ["library:pymemcache"]
        return True, []

    def _init_run(self, context: Any) -> None:
        from pymemcache.client.base import Client  # type: ignore[import-not-found]

        cfg = (
            (context.runtime_config or {})
            .get("runtime_plugins", {})
            .get("memcached", {})
        )
        url = (
            os.environ.get("MEMCACHED_URL")
            or cfg.get("url")
            or "localhost:11211"
        )
        self._client = Client(_parse_host_port(url))
        ttl_raw = cfg.get("ttl_seconds")
        # pymemcache `expire` of 0 means "no expire" — match that
        # convention when the config doesn't set one.
        self._ttl = int(ttl_raw) if ttl_raw is not None else 0

    def _upsert_snapshot(self, run_id: str, snapshot: dict[str, Any]) -> None:
        payload = json.dumps(snapshot, default=str)
        self._client.set(f"run:{run_id}", payload, expire=self._ttl)

    def _teardown(self) -> None:
        if self._client is not None:
            try:
                self._client.close()
            except Exception:
                pass
            self._client = None


def plugin() -> MemcachedPlugin:
    return MemcachedPlugin()
