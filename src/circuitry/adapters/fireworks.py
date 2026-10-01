"""Adapter for Fireworks AI (fireworks.ai) — fast inference for open-source
models via OpenAI-compatible chat completions.

Authentication: ``FIREWORKS_API_KEY`` (https://fireworks.ai/settings/users/api-keys).
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
class FireworksAdapter:
    accepts_images: ClassVar[bool] = True
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
