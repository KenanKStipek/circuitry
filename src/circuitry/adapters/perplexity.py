"""Adapter for Perplexity (perplexity.ai) — search-augmented LLMs (Sonar
family) via OpenAI-compatible chat completions.

Authentication: ``PERPLEXITY_API_KEY`` (https://www.perplexity.ai/settings/api).
"""

from __future__ import annotations

from dataclasses import dataclass

from ..preflight import CheckResult
from ._openai_compat import (
    OpenAICompatibleConfig,
    chat_completion,
    check_dependencies,
)
from .base import GenerateResult


@dataclass(frozen=True)
class PerplexityAdapter:
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
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return chat_completion(
            cfg=self._cfg(),
            model=model,
            prompt=prompt,
            timeout_seconds=timeout_seconds,
        )

    def check(self) -> CheckResult:
        return check_dependencies(self._cfg())
