"""Adapter for Perplexity (perplexity.ai) — search-augmented LLMs (Sonar
family) via OpenAI-compatible chat completions.

Authentication: ``PERPLEXITY_API_KEY`` (https://www.perplexity.ai/settings/api).
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
class PerplexityAdapter:
    accepts_images: ClassVar[bool] = True
    name: str = "perplexity"
    base_url: str = "https://api.perplexity.ai"
    default_model: str = "sonar"

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=self.base_url,
            api_key_env="PERPLEXITY_API_KEY",
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
