"""NATS pub/sub runtime plugin via nats-py.

Optional dep: ``nats-py``. Install with ``pip install circuitry-cof[nats]``.

NATS uses an asynchronous client API. To keep the pub/sub base class
synchronous, this plugin runs an event loop on a dedicated thread for
the duration of each run and submits coroutines to it.

Config sources (priority order):
  1. Env var ``NATS_URL`` / ``runtime_plugins.nats.url``
     (default ``"nats://localhost:4222"``).
  2. ``runtime_plugins.nats.topic`` (default ``"circuitry-runs"``).
"""

from __future__ import annotations

import asyncio
import importlib.util
import os
import threading
from typing import Any

from ._pubsub import PubSubBase


class NatsPlugin(PubSubBase):
    name: str = "nats"

    def __init__(self) -> None:
        super().__init__()
        self._loop: asyncio.AbstractEventLoop | None = None
        self._loop_thread: threading.Thread | None = None
        self._client: Any = None

    def _check_dep(self) -> tuple[bool, list[str]]:
        # Both ``nats`` (legacy) and ``nats-py`` register as the ``nats``
        # top-level module; the wheel name is ``nats-py`` for
        # disambiguation.
        if importlib.util.find_spec("nats") is None:
            return False, ["library:nats-py"]
        return True, []

    def _start_loop(self) -> None:
        ready = threading.Event()

        def runner() -> None:
            self._loop = asyncio.new_event_loop()
            asyncio.set_event_loop(self._loop)
            ready.set()
            self._loop.run_forever()

        self._loop_thread = threading.Thread(target=runner, daemon=True)
        self._loop_thread.start()
        ready.wait()

    def _open_publisher(self, runtime_config: dict[str, Any]) -> None:
        import nats  # type: ignore[import-not-found]

        cfg = (runtime_config or {}).get("runtime_plugins", {}).get("nats", {})
        url = (
            os.environ.get("NATS_URL")
            or cfg.get("url")
            or "nats://localhost:4222"
        )
        self._start_loop()
        assert self._loop is not None

        async def _connect() -> Any:
            return await nats.connect(url)

        future = asyncio.run_coroutine_threadsafe(_connect(), self._loop)
        self._client = future.result(timeout=10.0)

    def _publish(self, topic: str, payload: bytes) -> None:
        if self._client is None or self._loop is None:
            return

        async def _do_publish() -> None:
            await self._client.publish(topic, payload)
            await self._client.flush(timeout=5.0)

        future = asyncio.run_coroutine_threadsafe(_do_publish(), self._loop)
        future.result(timeout=10.0)

    def _close_publisher(self) -> None:
        if self._client is not None and self._loop is not None:
            async def _drain() -> None:
                try:
                    await self._client.drain()
                except Exception:
                    pass
            try:
                future = asyncio.run_coroutine_threadsafe(_drain(), self._loop)
                future.result(timeout=5.0)
            except Exception:
                pass
            self._client = None
        if self._loop is not None:
            self._loop.call_soon_threadsafe(self._loop.stop)
            if self._loop_thread is not None:
                self._loop_thread.join(timeout=5.0)
            self._loop = None
            self._loop_thread = None


def plugin() -> NatsPlugin:
    return NatsPlugin()
