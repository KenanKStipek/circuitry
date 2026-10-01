"""Adapter for Groq (groq.com) — OpenAI-compatible chat completions on
specialized LPU inference hardware.

Authentication: ``GROQ_API_KEY`` (https://console.groq.com/keys).
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
class GroqAdapter:
    accepts_images: ClassVar[bool] = True
    name: str = "groq"
    base_url: str = "https://api.groq.com/openai/v1"
    default_model: str = "llama-3.3-70b-versatile"

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=self.base_url,
            api_key_env="GROQ_API_KEY",
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
