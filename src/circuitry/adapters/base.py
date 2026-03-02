from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Protocol


@dataclass(frozen=True)
class GenerateResult:
    text: str
    raw: dict[str, Any]
    tokens_sent: int | None = None
    tokens_received: int | None = None


class Adapter(Protocol):
    @property
    def name(self) -> str: ...

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult: ...


@dataclass(frozen=True)
class ImageResult:
    raw: dict[str, Any]
    image_bytes: bytes | None = None
    image_url: str | None = None


class ImageAdapter(Protocol):
    @property
    def name(self) -> str: ...

    def generate_image(
        self,
        *,
        model: str,
        prompt: str,
        params: dict[str, Any] | None,
        timeout_seconds: int = 120,
    ) -> ImageResult: ...
