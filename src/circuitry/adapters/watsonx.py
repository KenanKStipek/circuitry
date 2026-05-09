"""Adapter for IBM watsonx.ai — Granite / Llama models via IBM Cloud.

Authentication is two-step: an API key is exchanged with IBM Cloud IAM
for a short-lived (~60 min) bearer token, which is then sent on the
generation request. Tokens are cached in-process keyed by the API key,
so successive runs in the same Python session reuse the token until
its expiry.

Required env:
  - ``WATSONX_API_KEY`` — IBM Cloud API key.
  - ``WATSONX_PROJECT_ID`` — watsonx.ai project the request runs against.

Optional:
  - ``WATSONX_REGION`` (default ``us-south``) — selects the ML endpoint.
    Override ``base_url`` for non-region or staging endpoints.
  - ``WATSONX_API_VERSION`` (default ``2024-03-14``).

Wire format is watsonx-native (``/ml/v1/text/generation``). The ``model``
parameter maps to ``model_id`` and the prompt becomes ``input``.
"""

from __future__ import annotations

import json
import os
import shlex
import shutil
import subprocess
import threading
import time
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import GenerateResult


# Module-level token cache: api_key -> (token, expires_at_epoch_seconds).
# A 5-minute buffer is subtracted from the IAM-reported lifetime so we
# never use a token that's about to expire mid-flight.
_TOKEN_CACHE: dict[str, tuple[str, float]] = {}
_TOKEN_LOCK = threading.Lock()
_TOKEN_TTL_BUFFER_SECONDS = 300


def _exchange_iam_token(api_key: str, *, timeout_seconds: int) -> tuple[str, float]:
    cmd = [
        "curl",
        "--silent",
        "--show-error",
        "--fail-with-body",
        "--max-time",
        str(int(timeout_seconds)),
        "-X",
        "POST",
        "https://iam.cloud.ibm.com/identity/token",
        "-H",
        "Content-Type: application/x-www-form-urlencoded",
        "-d",
        "grant_type=urn:ibm:params:oauth:grant-type:apikey&apikey=" + api_key,
    ]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    except FileNotFoundError as exc:
        raise RuntimeError("curl is not installed or not on PATH") from exc
    if proc.returncode != 0:
        err = (proc.stderr or proc.stdout or "").strip()
        # Mask the raw api_key out of any error trace.
        masked = err.replace(api_key, "***")
        raise RuntimeError(f"watsonx IAM token exchange failed: {masked}")
    try:
        raw = json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(
            f"watsonx IAM returned non-JSON response: {proc.stdout[:200]}"
        ) from exc
    token = raw.get("access_token")
    expires_in = int(raw.get("expires_in", 3600))
    if not isinstance(token, str) or not token:
        raise RuntimeError(f"watsonx IAM response missing access_token: {raw}")
    expires_at = time.time() + max(60, expires_in - _TOKEN_TTL_BUFFER_SECONDS)
    return (token, expires_at)


def _get_token(api_key: str, *, timeout_seconds: int) -> str:
    with _TOKEN_LOCK:
        cached = _TOKEN_CACHE.get(api_key)
        if cached is not None and cached[1] > time.time():
            return cached[0]
        token, expires_at = _exchange_iam_token(
            api_key, timeout_seconds=timeout_seconds
        )
        _TOKEN_CACHE[api_key] = (token, expires_at)
        return token


@dataclass(frozen=True)
class WatsonXAdapter:
    name: str = "watsonx"
    base_url: str = ""  # resolved from WATSONX_REGION when empty
    default_model: str = "meta-llama/llama-3-3-70b-instruct"
    api_version: str = "2024-03-14"

    def _resolve_base_url(self) -> str:
        if self.base_url:
            return self.base_url.rstrip("/")
        region = os.environ.get("WATSONX_REGION", "us-south").strip() or "us-south"
        return f"https://{region}.ml.cloud.ibm.com"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        api_key = os.environ.get("WATSONX_API_KEY", "")
        if not api_key:
            raise RuntimeError("watsonx: WATSONX_API_KEY not set.")
        project_id = os.environ.get("WATSONX_PROJECT_ID", "")
        if not project_id:
            raise RuntimeError("watsonx: WATSONX_PROJECT_ID not set.")

        token = _get_token(api_key, timeout_seconds=timeout_seconds)
        target_model = model or self.default_model
        api_version = (
            os.environ.get("WATSONX_API_VERSION") or self.api_version
        )

        url = (
            f"{self._resolve_base_url()}/ml/v1/text/generation"
            f"?version={api_version}"
        )
        payload: dict[str, Any] = {
            "model_id": target_model,
            "input": prompt,
            "project_id": project_id,
        }

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
            f"Authorization: Bearer {token}",
            "-d",
            json.dumps(payload),
            url,
        ]

        try:
            proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
        except FileNotFoundError as exc:
            raise RuntimeError("curl is not installed or not on PATH") from exc

        if proc.returncode != 0:
            masked = " ".join(shlex.quote(c) for c in cmd).replace(token, "***")
            if api_key:
                masked = masked.replace(api_key, "***")
            err = (proc.stderr or proc.stdout or "").strip()
            raise RuntimeError(
                f"watsonx request failed (curl exit {proc.returncode}): {err} "
                f"cmd={masked}"
            )

        try:
            raw = json.loads(proc.stdout)
        except json.JSONDecodeError as exc:
            raise RuntimeError(
                f"watsonx returned non-JSON response: {proc.stdout[:200]}"
            ) from exc

        results = raw.get("results") or []
        text = ""
        tokens_sent: int | None = None
        tokens_received: int | None = None
        if isinstance(results, list) and results:
            first = results[0] if isinstance(results[0], dict) else {}
            text = str(first.get("generated_text", "") or "")
            tokens_sent = first.get("input_token_count")
            tokens_received = first.get("generated_token_count")

        return GenerateResult(
            text=text.strip() if text else "",
            raw=raw,
            tokens_sent=int(tokens_sent) if isinstance(tokens_sent, int) else None,
            tokens_received=int(tokens_received)
            if isinstance(tokens_received, int)
            else None,
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if not os.environ.get("WATSONX_API_KEY"):
            missing.append("env:WATSONX_API_KEY")
        if not os.environ.get("WATSONX_PROJECT_ID"):
            missing.append("env:WATSONX_PROJECT_ID")
        if shutil.which("curl") is None:
            missing.append("binary:curl")
        return CheckResult(ok=not missing, missing=missing)
