"""Adapter for Azure OpenAI — OpenAI models hosted in your Azure
subscription via per-deployment endpoints.

URL shape:
  ``{endpoint}/openai/deployments/{deployment}/chat/completions?api-version=...``

Where ``{deployment}`` is whatever you named your model deployment in
the Azure portal — Azure routes by deployment name, not OpenAI model
name. The orchestration's ``model:`` field flows in as the deployment.

Authentication: ``AZURE_OPENAI_API_KEY`` (or Entra ID token, not yet
supported here). Azure uses the ``api-key`` header rather than
``Authorization: Bearer``, so the helper's built-in Bearer path is
bypassed (``api_key_env=""``) and the key is added via ``extra_headers``.

Required env: ``AZURE_OPENAI_API_KEY`` and ``AZURE_OPENAI_ENDPOINT``
(your resource endpoint, e.g. ``https://my-resource.openai.azure.com``).
Override ``api_version`` via ``runtime.adapters.azure-openai.api_version``.
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


@dataclass(frozen=True)
class AzureOpenAIAdapter:
    name: str = "azure-openai"
    endpoint: str = ""  # e.g. "https://my-resource.openai.azure.com"
    api_version: str = "2024-10-21"
    default_model: str = ""  # deployment name

    def _resolve_endpoint(self) -> str:
        return self.endpoint or os.environ.get("AZURE_OPENAI_ENDPOINT", "")

    def _cfg(self) -> OpenAICompatibleConfig:
        endpoint = self._resolve_endpoint().rstrip("/")
        # The helper's api_key_env is left empty; we add the api-key
        # header via extra_headers below since Azure's auth scheme is
        # not Bearer.
        return OpenAICompatibleConfig(
            base_url=endpoint,
            api_key_env="",
            default_model=self.default_model,
            chat_completions_path=(
                "/openai/deployments/{model}/chat/completions"
                f"?api-version={self.api_version}"
            ),
        )

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        cfg = self._cfg()
        if not cfg.base_url:
            raise RuntimeError(
                "azure-openai: endpoint not configured. Set "
                "AZURE_OPENAI_ENDPOINT or runtime.adapters.azure-openai.endpoint."
            )
        api_key = os.environ.get("AZURE_OPENAI_API_KEY", "")
        if not api_key:
            raise RuntimeError(
                "azure-openai: AZURE_OPENAI_API_KEY not set."
            )
        return chat_completion(
            cfg=cfg,
            model=model,
            prompt=prompt,
            timeout_seconds=timeout_seconds,
            extra_headers={"api-key": api_key},
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if not os.environ.get("AZURE_OPENAI_API_KEY"):
            missing.append("env:AZURE_OPENAI_API_KEY")
        if not self._resolve_endpoint():
            missing.append("env:AZURE_OPENAI_ENDPOINT")
        # Reuse the helper's curl + key-env machinery for the binary
        # check; the env-key check above already covers Azure's flavour.
        base = check_dependencies(self._cfg())
        if not base.ok:
            for m in base.missing:
                if m.startswith("binary:") and m not in missing:
                    missing.append(m)
        return CheckResult(ok=not missing, missing=missing)
