"""Adapter for Cyberdiner — an OpenAI-compatible LLM inference broker.

Authentication: ``CYBERDINER_TOKEN``. The endpoint is per-deployment —
set ``CYBERDINER_BASE_URL`` (or ``runtime.adapters.cyberdiner.base_url``)
to your installation's chat-completions root.
"""

from __future__ import annotations

import os
from dataclasses import dataclass

from ..preflight import CheckResult
from ._openai_compat import (
    OpenAICompatibleConfig,
    chat_completion,
    check_dependencies,
)
from .base import GenerateResult


def _default_base_url() -> str:
    return os.environ.get("CYBERDINER_BASE_URL") or ""


@dataclass(frozen=True)
class CyberdinerAdapter:
    name: str = "cyberdiner"
    # Endpoint must be supplied via env or config — there's no public
    # default URL since it's a self-hosted broker.
    base_url: str = ""
    default_model: str = ""

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=self.base_url or _default_base_url(),
            api_key_env="CYBERDINER_TOKEN",
            default_model=self.default_model,
        )

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        cfg = self._cfg()
        if not cfg.base_url:
            raise RuntimeError(
                "cyberdiner: base_url not configured. Set CYBERDINER_BASE_URL "
                "or runtime.adapters.cyberdiner.base_url to your broker's "
                "chat-completions root."
            )
        return chat_completion(
            cfg=cfg,
            model=model,
            prompt=prompt,
            timeout_seconds=timeout_seconds,
        )

    def check(self) -> CheckResult:
        cfg = self._cfg()
        result = check_dependencies(cfg)
        # Augment with the host requirement when not configured.
        if not cfg.base_url:
            return CheckResult(
                ok=False,
                missing=list(result.missing) + ["env:CYBERDINER_BASE_URL"],
                message="set CYBERDINER_BASE_URL to your broker endpoint.",
            )
        return result
