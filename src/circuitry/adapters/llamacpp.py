"""Adapter for llama.cpp's HTTP server (``llama-server``) — self-hosted
inference for GGUF models with an OpenAI-compatible chat completions
endpoint at ``/v1``.

No authentication by default. Override ``base_url`` via
``runtime.adapters.llamacpp.base_url`` in config.json to point at the
running server.
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
class LlamaCppAdapter:
    accepts_images: ClassVar[bool] = True
    name: str = "llamacpp"
    # llama-server defaults to port 8080.
    base_url: str = "http://localhost:8080/v1"
    default_model: str = ""

    #: Self-hosted; no auth by default. ``runtime.adapters.llamacpp.api_key_env``
    #: can name a variable to send as a Bearer token (a proxy in front of
    #: llama-server that does check one).
    api_key_env: str = ""

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=self.base_url,
            api_key_env=self.api_key_env,
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
