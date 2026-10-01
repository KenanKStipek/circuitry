"""Adapter for Mistral AI (mistral.ai) — Mistral / Codestral / Magistral
models via OpenAI-compatible chat completions.

Authentication: ``MISTRAL_API_KEY`` (https://console.mistral.ai/api-keys).
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
class MistralAdapter:
    accepts_images: ClassVar[bool] = True
    name: str = "mistral"
    base_url: str = "https://api.mistral.ai/v1"
    default_model: str = "mistral-large-latest"

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=self.base_url,
            api_key_env="MISTRAL_API_KEY",
            default_model=self.default_model,
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
