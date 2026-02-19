from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Protocol


@dataclass(frozen=True)
class Pulse:
    """Execution-time info (timestamps, run ids, etc.)."""

    run_id: str


@dataclass(frozen=True)
class EffectiveSettings:
    model: str
    adapter: str
    runtime: dict[str, Any]
    plugins: list[str]
    sources: dict[str, str]


@dataclass(frozen=True)
class GenerateResult:
    text: str
    raw: dict[str, Any]
    tokens_sent: int | None = None
    tokens_received: int | None = None


class Adapter(Protocol):
    name: str

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult: ...
