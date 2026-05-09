"""Kafka pub/sub runtime plugin.

Optional dep: ``confluent-kafka``. Install with
``pip install circuitry-cof[kafka]``.

Publishes JSON-encoded lifecycle events to the configured topic.

Config sources (priority order):
  1. Env var ``KAFKA_BROKERS`` / ``KAFKA_BOOTSTRAP_SERVERS`` /
     ``runtime_plugins.kafka.brokers`` (required).
  2. ``runtime_plugins.kafka.topic`` (default ``"circuitry-runs"``).
  3. ``runtime_plugins.kafka.publish_per_effect`` (bool, default True).
"""

from __future__ import annotations

import importlib.util
import os
from typing import Any

from ._pubsub import PubSubBase


class KafkaPlugin(PubSubBase):
    name: str = "kafka"

    def __init__(self) -> None:
        super().__init__()
        self._producer: Any = None

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("confluent_kafka") is None:
            return False, ["library:confluent-kafka"]
        return True, []

    def _open_publisher(self, runtime_config: dict[str, Any]) -> None:
        from confluent_kafka import Producer  # type: ignore[import-not-found]

        cfg = (runtime_config or {}).get("runtime_plugins", {}).get("kafka", {})
        brokers = (
            os.environ.get("KAFKA_BROKERS")
            or os.environ.get("KAFKA_BOOTSTRAP_SERVERS")
            or cfg.get("brokers")
        )
        if not brokers:
            raise RuntimeError(
                "kafka: brokers not configured. Set KAFKA_BROKERS or "
                "runtime.runtime_plugins.kafka.brokers."
            )
        producer_config: dict[str, Any] = {"bootstrap.servers": brokers}
        if isinstance(cfg.get("client_id"), str):
            producer_config["client.id"] = cfg["client_id"]
        # Forward any caller-supplied librdkafka config dict verbatim.
        for k, v in (cfg.get("producer_config") or {}).items():
            producer_config[str(k)] = v
        self._producer = Producer(producer_config)

    def _publish(self, topic: str, payload: bytes) -> None:
        self._producer.produce(topic, value=payload)
        # poll(0) services delivery callbacks; not strictly required
        # for fire-and-forget but keeps the queue from filling.
        self._producer.poll(0)

    def _close_publisher(self) -> None:
        if self._producer is not None:
            try:
                # Ensure all queued messages are delivered before closing.
                self._producer.flush(timeout=5.0)
            except Exception:
                pass
            self._producer = None


def plugin() -> KafkaPlugin:
    return KafkaPlugin()
