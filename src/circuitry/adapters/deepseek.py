"""Adapter for DeepSeek (deepseek.com) — DeepSeek-V3 / R1 models via
OpenAI-compatible chat completions.

Authentication: ``DEEPSEEK_API_KEY`` (https://platform.deepseek.com/api_keys).
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
class DeepSeekAdapter:
    name: str = "deepseek"
    base_url: str = "https://api.deepseek.com/v1"
    default_model: str = "deepseek-chat"

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=self.base_url,
            api_key_env="DEEPSEEK_API_KEY",
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
