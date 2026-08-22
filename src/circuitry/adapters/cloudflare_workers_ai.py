"""Adapter for Cloudflare Workers AI — multi-tenant inference at the edge,
exposed as OpenAI-compatible chat completions per Cloudflare account.

Authentication: ``CF_API_TOKEN`` (Workers AI scoped) plus ``CF_ACCOUNT_ID``
(your account UUID). The account_id is part of the URL path; this adapter
resolves it at construction time so the helper sees a fully-qualified
``base_url``.
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


def _resolve_base_url(explicit: str, account_id: str) -> str:
    if explicit:
        return explicit
    if not account_id:
        return ""
    return f"https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1"


@dataclass(frozen=True)
class CloudflareWorkersAIAdapter:
    name: str = "cloudflare-workers-ai"
    base_url: str = ""
    account_id: str = ""
    default_model: str = "@cf/meta/llama-3.3-70b-instruct-fp8-fast"

    def _cfg(self) -> OpenAICompatibleConfig:
        account_id = self.account_id or os.environ.get("CF_ACCOUNT_ID", "")
        return OpenAICompatibleConfig(
            base_url=_resolve_base_url(self.base_url, account_id),
            api_key_env="CF_API_TOKEN",
            default_model=self.default_model,
        )

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        cfg = self._cfg()
        if not cfg.base_url:
            raise RuntimeError(
                "cloudflare-workers-ai: account_id not configured. Set "
                "CF_ACCOUNT_ID or runtime.adapters.cloudflare-workers-ai."
                "account_id (or supply a fully-formed base_url)."
            )
        return chat_completion(
            cfg=cfg, model=model, prompt=prompt, timeout_seconds=timeout_seconds
        )

    def check(self) -> CheckResult:
        cfg = self._cfg()
        result = check_dependencies(cfg)
        if not cfg.base_url:
            return CheckResult(
                ok=False,
                missing=[*list(result.missing), "env:CF_ACCOUNT_ID"],
                message="set CF_ACCOUNT_ID to your Cloudflare account UUID.",
            )
        return result
