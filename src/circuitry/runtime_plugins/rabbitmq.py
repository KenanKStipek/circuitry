"""RabbitMQ pub/sub runtime plugin via pika.

Optional dep: ``pika``. Install with ``pip install circuitry-cof[rabbitmq]``.

Publishes JSON-encoded lifecycle events to the configured exchange +
routing key. The default exchange is ``""`` (the AMQP default
direct-exchange) with the routing key set to the topic name, which
delivers to the queue with that name. For fan-out / topic-routed
deployments, set ``exchange`` and ``exchange_type`` per-plugin.

Config sources (priority order):
  1. Env var ``RABBITMQ_URL`` / ``runtime_plugins.rabbitmq.url``
     (default ``"amqp://guest:guest@localhost:5672/"``).
  2. ``runtime_plugins.rabbitmq.topic`` (default ``"circuitry-runs"``).
  3. ``runtime_plugins.rabbitmq.exchange`` (default ``""``).
  4. ``runtime_plugins.rabbitmq.exchange_type`` (default ``"direct"``).
"""

from __future__ import annotations

import importlib.util
import os
from typing import Any

from ._pubsub import PubSubBase


class RabbitmqPlugin(PubSubBase):
    name: str = "rabbitmq"

    def __init__(self) -> None:
        super().__init__()
        self._connection: Any = None
        self._channel: Any = None
        self._exchange: str = ""
        self._exchange_type: str = "direct"

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("pika") is None:
            return False, ["library:pika"]
        return True, []

    def _open_publisher(self, runtime_config: dict[str, Any]) -> None:
        import pika  # type: ignore[import-not-found]

        cfg = (
            (runtime_config or {}).get("runtime_plugins", {}).get("rabbitmq", {})
        )
        url = (
            os.environ.get("RABBITMQ_URL")
            or cfg.get("url")
            or "amqp://guest:guest@localhost:5672/"
        )
        params = pika.URLParameters(url)
        self._connection = pika.BlockingConnection(params)
        self._channel = self._connection.channel()
        self._exchange = str(cfg.get("exchange") or "")
        self._exchange_type = str(cfg.get("exchange_type") or "direct")
        if self._exchange:
            self._channel.exchange_declare(
                exchange=self._exchange,
                exchange_type=self._exchange_type,
                durable=bool(cfg.get("durable", False)),
            )
        else:
            # Default exchange routes by queue name = topic.
            self._channel.queue_declare(
                queue=self._topic,
                durable=bool(cfg.get("durable", False)),
            )

    def _publish(self, topic: str, payload: bytes) -> None:
        # When using the default exchange, the routing key IS the
        # destination queue name; otherwise the routing key is the
        # topic and the exchange routes to bound queues.
        self._channel.basic_publish(
            exchange=self._exchange,
            routing_key=topic,
            body=payload,
        )

    def _close_publisher(self) -> None:
        try:
            if self._channel is not None and not self._channel.is_closed:
                self._channel.close()
        except Exception:
            pass
        try:
            if self._connection is not None and self._connection.is_open:
                self._connection.close()
        except Exception:
            pass
        self._channel = None
        self._connection = None


def plugin() -> RabbitmqPlugin:
    return RabbitmqPlugin()
