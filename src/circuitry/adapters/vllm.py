"""Adapter for vLLM (https://github.com/vllm-project/vllm) — high-throughput
self-hosted inference server with OpenAI-compatible chat completions.

No authentication by default. The caller specifies the model name
(matches whatever vLLM was launched with). Override ``base_url`` via
``runtime.adapters.vllm.base_url`` (or ``VLLM_BASE_URL`` env var, read
by the factory) to point at the deployed instance.
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
class VllmAdapter:
    name: str = "vllm"
    base_url: str = "http://localhost:8000/v1"
    default_model: str = ""

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=self.base_url,
            api_key_env="",  # self-hosted; no auth
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
