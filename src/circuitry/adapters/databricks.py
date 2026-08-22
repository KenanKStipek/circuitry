"""Adapter for Databricks Model Serving — workspace-hosted LLM endpoints
with an OpenAI-compatible chat completions interface.

Authentication: ``DATABRICKS_TOKEN`` (a Databricks PAT or service
principal token). The base URL is per-workspace; set ``DATABRICKS_HOST``
or ``runtime.adapters.databricks.base_url``. For your own
workspace ``https://adb-NNNN.NN.azuredatabricks.net/serving-endpoints``
or ``https://dbc-NNNN.cloud.databricks.com/serving-endpoints``.

Models are served by named *serving endpoints*, which Databricks treats
as the OpenAI ``model`` parameter (e.g. ``databricks-meta-llama-3-3-70b-instruct``).
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


def _resolve_base_url(explicit: str) -> str:
    if explicit:
        return explicit
    host = os.environ.get("DATABRICKS_HOST", "").rstrip("/")
    if not host:
        return ""
    if not host.startswith(("http://", "https://")):
        host = f"https://{host}"
    return f"{host}/serving-endpoints"


@dataclass(frozen=True)
class DatabricksAdapter:
    name: str = "databricks"
    base_url: str = ""
    default_model: str = "databricks-meta-llama-3-3-70b-instruct"

    def _cfg(self) -> OpenAICompatibleConfig:
        return OpenAICompatibleConfig(
            base_url=_resolve_base_url(self.base_url),
            api_key_env="DATABRICKS_TOKEN",
            default_model=self.default_model,
        )

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        cfg = self._cfg()
        if not cfg.base_url:
            raise RuntimeError(
                "databricks: workspace host not configured. Set DATABRICKS_HOST "
                "or runtime.adapters.databricks.base_url to your workspace URL."
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
                missing=[*list(result.missing), "env:DATABRICKS_HOST"],
                message="set DATABRICKS_HOST to your workspace.",
            )
        return result
