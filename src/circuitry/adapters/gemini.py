"""Adapter for Google's Gemini models via the OpenAI compatibility layer
exposed at ``generativelanguage.googleapis.com/v1beta/openai/``.

Authentication: ``GOOGLE_API_KEY`` (the same key used elsewhere in
Google's AI stack). Generated with `aistudio.google.com/apikey`.

Config options (under ``runtime.adapters.gemini``):
  - ``base_url``: defaults to the OpenAI-compat endpoint; override for
    a region-specific or proxied deployment.
  - ``default_model``: defaults to ``gemini-2.5-flash``.
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
class GeminiAdapter:
    name: str = "gemini"
    base_url: str = "https://generativelanguage.googleapis.com/v1beta/openai"
    default_model: str = "gemini-2.5-flash"

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=self.base_url,
            api_key_env="GOOGLE_API_KEY",
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
