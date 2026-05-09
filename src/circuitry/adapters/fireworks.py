"""Adapter for Fireworks AI (fireworks.ai) — fast inference for open-source
models via OpenAI-compatible chat completions.

Authentication: ``FIREWORKS_API_KEY`` (https://fireworks.ai/settings/users/api-keys).
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
class FireworksAdapter:
    name: str = "fireworks"
    base_url: str = "https://api.fireworks.ai/inference/v1"
    default_model: str = "accounts/fireworks/models/llama-v3p3-70b-instruct"

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=self.base_url,
            api_key_env="FIREWORKS_API_KEY",
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
