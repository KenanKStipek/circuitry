"""Adapter for NVIDIA NIM (build.nvidia.com) — Inference Microservices
exposing OpenAI-compatible endpoints. Both the cloud-hosted catalog at
``integrate.api.nvidia.com`` and self-hosted NIM containers share the
same wire format; switch between them by overriding ``base_url``.

Authentication: ``NIM_API_KEY`` for the cloud catalog. Self-hosted
deployments without auth can leave it unset and override
``runtime.adapters.nvidia-nim.api_key_env`` to disable the env-var
requirement (or simply ignore the missing-env warning since requests
without a Bearer header still go out).
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
class NvidiaNimAdapter:
    accepts_images: ClassVar[bool] = True
    name: str = "nvidia-nim"
    base_url: str = "https://integrate.api.nvidia.com/v1"
    default_model: str = "meta/llama-3.3-70b-instruct"
    # Cloud catalog requires auth; self-hosted deployments can override
    # to "" via runtime.adapters.nvidia-nim.api_key_env in config.json.
    api_key_env: str = "NIM_API_KEY"

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
