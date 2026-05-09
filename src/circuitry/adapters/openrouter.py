"""Adapter for OpenRouter (openrouter.ai) — multi-provider routing layer
exposing one OpenAI-compatible endpoint over many backend models.

Authentication: ``OPENROUTER_API_KEY``. Models follow the
``<provider>/<model>`` slug convention (e.g. ``openai/gpt-4o-mini``,
``anthropic/claude-3.5-sonnet``).
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
class OpenRouterAdapter:
    name: str = "openrouter"
    base_url: str = "https://openrouter.ai/api/v1"
    default_model: str = "openai/gpt-4o-mini"

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=self.base_url,
            api_key_env="OPENROUTER_API_KEY",
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
