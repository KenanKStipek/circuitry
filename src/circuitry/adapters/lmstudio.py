"""Adapter for LM Studio (lmstudio.ai) — desktop application with a local
OpenAI-compatible inference server. No auth by default.

LM Studio's server runs on ``http://localhost:1234/v1`` by default;
override via ``runtime.adapters.lmstudio.base_url`` (or
``LMSTUDIO_BASE_URL``) for non-default ports.
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
class LMStudioAdapter:
    accepts_images: ClassVar[bool] = True
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
