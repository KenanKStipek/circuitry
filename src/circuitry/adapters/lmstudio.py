"""Adapter for LM Studio (lmstudio.ai) — desktop application with a local
OpenAI-compatible inference server. No auth by default.

LM Studio's server runs on ``http://localhost:1234/v1`` by default;
override via ``runtime.adapters.lmstudio.base_url`` (or
``LMSTUDIO_BASE_URL``) for non-default ports.
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
class LMStudioAdapter:
    name: str = "lmstudio"
    base_url: str = "http://localhost:1234/v1"
    default_model: str = ""

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=self.base_url,
            api_key_env="",
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
