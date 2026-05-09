"""Adapter for Hugging Face Text Generation Inference (TGI) — self-hosted
inference server with an OpenAI-compatible chat completions endpoint.

No authentication by default (TGI deployments behind a reverse proxy may
add auth at the network layer, in which case set ``runtime.adapters.tgi.
api_key_env`` and the bearer token will be added). Override ``base_url``
or set ``TGI_BASE_URL`` to point at the deployed instance.
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
class TgiAdapter:
    name: str = "tgi"
    base_url: str = "http://localhost:3000/v1"
    default_model: str = ""
    api_key_env: str = ""

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=self.base_url,
            api_key_env=self.api_key_env,
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
