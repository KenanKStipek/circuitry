"""Adapter for Replicate (replicate.com) — community-hosted models with
its own asynchronous predictions API.

Authentication: ``REPLICATE_API_TOKEN`` (https://replicate.com/account/api-tokens).

Replicate's wire format isn't OpenAI-compatible: predictions are POSTed
to ``/v1/models/{owner}/{name}/predictions`` (or ``/v1/predictions``
with a ``version`` field for pinned model versions). The default async
flow returns ``status: starting`` and requires polling. This adapter
uses the ``Prefer: wait=N`` header so the response holds open until the
prediction completes (or the wait window elapses) — yielding a
synchronous-feeling call within ``timeout_seconds`` without explicit
polling. If the prediction is still running at the wait deadline the
adapter raises with the prediction id so callers can resume.

Models are identified as ``{owner}/{name}`` (e.g.
``meta/meta-llama-3-70b-instruct``). Output is concatenated from the
prediction's ``output`` array (Replicate streams completion tokens).
"""

from __future__ import annotations

import json
import os
import shlex
import shutil
import subprocess
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import GenerateResult


@dataclass(frozen=True)
class ReplicateAdapter:
    name: str = "replicate"
    base_url: str = "https://api.replicate.com/v1"
    default_model: str = "meta/meta-llama-3-70b-instruct"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        api_token = os.environ.get("REPLICATE_API_TOKEN", "")
        if not api_token:
            raise RuntimeError(
                "replicate: REPLICATE_API_TOKEN not set."
            )
        target_model = (model or self.default_model).strip()
        if "/" not in target_model:
            raise ValueError(
                f"replicate: model must be 'owner/name' (got {target_model!r})."
            )

        # Cap the synchronous wait at the lesser of timeout and 60s
        # (Replicate's own per-request hold limit).
        wait = max(1, min(int(timeout_seconds), 60))
        url = f"{self.base_url.rstrip('/')}/models/{target_model}/predictions"
        payload = {"input": {"prompt": prompt}}

        cmd = [
            "curl",
            "--silent",
            "--show-error",
            "--fail-with-body",
            "--max-time",
            str(int(timeout_seconds)),
            "-H",
            "Content-Type: application/json",
            "-H",
            f"Authorization: Bearer {api_token}",
            "-H",
            f"Prefer: wait={wait}",
            "-d",
            json.dumps(payload),
            url,
        ]

        try:
            proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
        except FileNotFoundError as exc:
            raise RuntimeError("curl is not installed or not on PATH") from exc

        if proc.returncode != 0:
            masked_cmd = " ".join(shlex.quote(c) for c in cmd).replace(api_token, "***")
            err = (proc.stderr or proc.stdout or "").strip()
            raise RuntimeError(
                f"Replicate request failed (curl exit {proc.returncode}): {err} "
                f"cmd={masked_cmd}"
            )
        try:
            raw = json.loads(proc.stdout)
        except json.JSONDecodeError as exc:
            raise RuntimeError(
                f"Replicate returned non-JSON response: {proc.stdout[:200]}"
            ) from exc

        status = raw.get("status")
        if status not in ("succeeded", "processing", "starting"):
            err = raw.get("error") or status
            raise RuntimeError(f"Replicate prediction failed: {err}")
        if status != "succeeded":
            pred_id = raw.get("id", "?")
            raise RuntimeError(
                f"Replicate prediction {pred_id!r} did not complete within "
                f"wait={wait}s (status={status!r}). Re-issue with a longer "
                "timeout or poll /v1/predictions/{id}."
            )

        output = raw.get("output", [])
        if isinstance(output, list):
            text = "".join(str(chunk) for chunk in output)
        else:
            text = str(output) if output is not None else ""

        # Replicate doesn't expose token counts on the prediction.
        return GenerateResult(
            text=text.strip(),
            raw=raw,
            tokens_sent=None,
            tokens_received=None,
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if not os.environ.get("REPLICATE_API_TOKEN"):
            missing.append("env:REPLICATE_API_TOKEN")
        if shutil.which("curl") is None:
            missing.append("binary:curl")
        return CheckResult(ok=not missing, missing=missing)
