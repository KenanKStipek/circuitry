"""Adapter for Groq (groq.com) — OpenAI-compatible chat completions on
specialized LPU inference hardware.

Authentication: ``GROQ_API_KEY`` (https://console.groq.com/keys).
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
class GroqAdapter:
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
