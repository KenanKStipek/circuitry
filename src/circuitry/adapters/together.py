"""Adapter for Together AI (together.ai) — open-source models served via
OpenAI-compatible chat completions.

Authentication: ``TOGETHER_API_KEY`` (https://api.together.ai/settings/api-keys).
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
class TogetherAdapter:
    name: str = "together"
    base_url: str = "https://api.together.xyz/v1"
    default_model: str = "meta-llama/Llama-3.3-70B-Instruct-Turbo"

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=self.base_url,
            api_key_env="TOGETHER_API_KEY",
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
