"""Redis persistence runtime plugin.

Optional dep: ``redis``. Install with ``pip install circuitry-cof[redis]``.

Stores the JSON-serialised snapshot at ``run:<run_id>`` with optional
TTL.

Config sources (priority order):
  1. Env var ``REDIS_URL`` (e.g. ``redis://:password@host:6379/0``).
  2. ``runtime_plugins.redis.url`` in config.json.
  3. Default ``redis://localhost:6379``.

Optional:
  - ``runtime_plugins.redis.ttl_seconds`` (int): TTL for the run key.
    Default unset (no expiration).
  - ``runtime_plugins.redis.key_prefix`` (str): prepended to the
    ``run:`` key (default empty).
"""

from __future__ import annotations

import importlib.util
import json
import os
from typing import Any

from ._snapshot_persistence import SnapshotPersistenceBase


class RedisPlugin(SnapshotPersistenceBase):
    name: str = "redis"

    def __init__(self) -> None:
        super().__init__()
        self._client: Any = None
        self._ttl: int | None = None
        self._key_prefix: str = ""

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("redis") is None:
            return False, ["library:redis"]
        return True, []

    def _init_run(self, context: Any) -> None:
        import redis  # type: ignore[import-not-found]

        cfg = (
            (context.runtime_config or {}).get("runtime_plugins", {}).get("redis", {})
        )
        url = (
            os.environ.get("REDIS_URL")
            or cfg.get("url")
            or "redis://localhost:6379"
        )
        self._client = redis.from_url(url)
        self._key_prefix = str(cfg.get("key_prefix") or "")
        ttl_raw = cfg.get("ttl_seconds")
        self._ttl = int(ttl_raw) if ttl_raw is not None else None

    def _key_for(self, run_id: str) -> str:
        return f"{self._key_prefix}run:{run_id}"

    def _upsert_snapshot(self, run_id: str, snapshot: dict[str, Any]) -> None:
        payload = json.dumps(snapshot, default=str)
        if self._ttl is not None and self._ttl > 0:
            self._client.set(self._key_for(run_id), payload, ex=self._ttl)
        else:
            self._client.set(self._key_for(run_id), payload)

    def _teardown(self) -> None:
        if self._client is not None:
            try:
                self._client.close()
            except Exception:
                pass
            self._client = None


def plugin() -> RedisPlugin:
    return RedisPlugin()
