"""Adapter for Cohere (cohere.com) — Command models via the
OpenAI-compatibility endpoint.

Authentication: ``COHERE_API_KEY`` (https://dashboard.cohere.com/api-keys).
Cohere's native ``/v2/chat`` API has its own request/response shape;
this adapter routes through their compatibility shim at
``/compatibility/v1/chat/completions`` so the standard helper applies.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import ClassVar

from ..preflight import CheckResult
from ._openai_compat import (
    OpenAICompatibleConfig,
    chat_completion,
    check_dependencies,
)
from .base import GenerateOptions, GenerateResult


@dataclass(frozen=True)
class CohereAdapter:
    accepts_images: ClassVar[bool] = True
    name: str = "cohere"
    base_url: str = "https://api.cohere.com"
    default_model: str = "command-r-plus"

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=self.base_url,
            api_key_env="COHERE_API_KEY",
            default_model=self.default_model,
            chat_completions_path="/compatibility/v1/chat/completions",
        )

    def generate(
        self,
        *,
        model: str,
        prompt: str,
        timeout_seconds: int = 120,
        options: GenerateOptions | None = None,
    ) -> GenerateResult:
        return chat_completion(
            cfg=self._cfg(),
            model=model,
            prompt=prompt,
            timeout_seconds=timeout_seconds,
            options=options,
        )

    def check(self) -> CheckResult:
        return check_dependencies(self._cfg())
