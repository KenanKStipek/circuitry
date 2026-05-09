"""Adapter for llama.cpp's HTTP server (``llama-server``) — self-hosted
inference for GGUF models with an OpenAI-compatible chat completions
endpoint at ``/v1``.

No authentication by default. Override ``base_url`` (or set
``LLAMACPP_BASE_URL``) to point at the running server.
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
class LlamaCppAdapter:
    name: str = "llamacpp"
    # llama-server defaults to port 8080.
    base_url: str = "http://localhost:8080/v1"
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
