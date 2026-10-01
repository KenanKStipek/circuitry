"""Adapter for Alibaba Cloud's DashScope — Qwen models via the
OpenAI-compatibility endpoint.

Authentication: ``DASHSCOPE_API_KEY`` (https://dashscope.console.aliyun.com).
The international-region endpoint is exposed at
``dashscope-intl.aliyuncs.com``; the Mainland China endpoint is
``dashscope.aliyuncs.com``. The default targets the international
endpoint — override via ``runtime.adapters.qwen-dashscope.base_url``
to switch regions.
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
class QwenDashScopeAdapter:
    accepts_images: ClassVar[bool] = True
    name: str = "qwen-dashscope"
    base_url: str = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"
    default_model: str = "qwen-max"

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=self.base_url,
            api_key_env="DASHSCOPE_API_KEY",
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
