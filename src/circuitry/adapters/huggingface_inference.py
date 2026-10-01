"""Adapter for Hugging Face Inference Providers — multi-provider router
exposing OpenAI-compatible chat completions across many backends
(together, replicate, hyperbolic, fal, fireworks, etc).

Authentication: ``HF_TOKEN`` (https://huggingface.co/settings/tokens).
Models follow the ``<owner>/<repo>`` slug convention; some providers
require a per-provider suffix (``meta-llama/Llama-3.3-70B-Instruct:cerebras``).
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
class HuggingFaceInferenceAdapter:
    accepts_images: ClassVar[bool] = True
    name: str = "huggingface-inference"
    base_url: str = "https://router.huggingface.co/v1"
    default_model: str = "meta-llama/Llama-3.3-70B-Instruct"

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=self.base_url,
            api_key_env="HF_TOKEN",
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
