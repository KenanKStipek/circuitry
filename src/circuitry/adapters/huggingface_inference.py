"""Adapter for Hugging Face Inference Providers — multi-provider router
exposing OpenAI-compatible chat completions across many backends
(together, replicate, hyperbolic, fal, fireworks, etc).

Authentication: ``HF_TOKEN`` (https://huggingface.co/settings/tokens).
Models follow the ``<owner>/<repo>`` slug convention; some providers
require a per-provider suffix (``meta-llama/Llama-3.3-70B-Instruct:cerebras``).
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
class HuggingFaceInferenceAdapter:
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
