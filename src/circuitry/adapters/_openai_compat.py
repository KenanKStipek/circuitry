"""Shared helper for adapters that speak the OpenAI Chat Completions
wire format.

A growing number of providers expose ``POST /chat/completions`` with
the same request shape as OpenAI (Bearer auth, ``messages`` array,
``choices[0].message.content`` response, ``usage.{prompt,completion}_tokens``).
Rather than duplicate the same ~80 lines of curl plumbing across each
provider's adapter, those adapters delegate to this helper.

The helper is intentionally curl-based to match existing OpenAI/
Anthropic adapter conventions (no extra Python deps). Each adapter
file remains small (~40 lines) — its job is to declare the per-provider
defaults (base URL, env var name, default model) and surface a stable
class for the factory to construct.
"""

from __future__ import annotations

import json
import os
import shlex
import shutil
import subprocess
from dataclasses import dataclass

from ..preflight import CheckResult
from .base import GenerateResult


@dataclass(frozen=True)
class OpenAICompatibleConfig:
    """Per-provider configuration for the OpenAI Chat Completions helper.

    ``api_key_env`` may be empty for self-hosted endpoints (vllm, llama.cpp,
    LM Studio) that don't require auth. When empty, ``check()`` reports
    only host-level readiness.

    ``chat_completions_path`` may contain ``{model}`` for providers whose
    URL embeds the deployment / model name (notably Azure OpenAI:
    ``/openai/deployments/{model}/chat/completions?api-version=...``).
    A path without that placeholder formats unchanged.
    """

    base_url: str
    api_key_env: str
    default_model: str
    chat_completions_path: str = "/chat/completions"


def chat_completion(
    *,
    cfg: OpenAICompatibleConfig,
    model: str,
    prompt: str,
    timeout_seconds: int = 120,
    extra_headers: dict[str, str] | None = None,
    extra_body: dict[str, object] | None = None,
) -> GenerateResult:
    """Issue a single chat-completion request.

    ``extra_headers`` / ``extra_body`` cover provider-specific quirks
    (e.g. anthropic-version header, Azure deployment routing).
    """
    model = model or cfg.default_model
    api_key = os.environ.get(cfg.api_key_env, "") if cfg.api_key_env else ""

    if cfg.api_key_env and not api_key:
        raise RuntimeError(
            f"API key not found for {cfg.api_key_env}. "
            f"Export {cfg.api_key_env}=... in the environment."
        )

    # ``str.format(model=...)`` substitutes the placeholder when present
    # (Azure deployments) and is a no-op otherwise. urllib.parse.quote
    # would be safer in principle but the model names that flow here are
    # already constrained by the orchestration schema name pattern.
    path = cfg.chat_completions_path.format(model=model)
    url = f"{cfg.base_url.rstrip('/')}{path}"

    payload: dict[str, object] = {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
    }
    if extra_body:
        payload.update(extra_body)

    cmd = [
        "curl",
        "--silent",
        "--show-error",
        "--fail-with-body",
        "--max-time",
        str(int(timeout_seconds)),
        "-H",
        "Content-Type: application/json",
    ]
    if api_key:
        cmd += ["-H", f"Authorization: Bearer {api_key}"]
    for k, v in (extra_headers or {}).items():
        cmd += ["-H", f"{k}: {v}"]
    cmd += ["-d", json.dumps(payload), url]

    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    except FileNotFoundError as e:
        raise RuntimeError("curl is not installed or not on PATH") from e

    if proc.returncode != 0:
        # Mask api_key in any echoed cmd string.
        masked_cmd = " ".join(shlex.quote(c) for c in cmd)
        if api_key:
            masked_cmd = masked_cmd.replace(api_key, "***")
        err = (proc.stderr or proc.stdout or "").strip()
        raise RuntimeError(
            f"OpenAI-compatible request failed (curl exit {proc.returncode}): "
            f"{err} cmd={masked_cmd}"
        )

    try:
        raw = json.loads(proc.stdout)
    except json.JSONDecodeError as e:
        raise RuntimeError(
            f"Provider returned non-JSON response: {proc.stdout[:200]}"
        ) from e

    text = ""
    choices = raw.get("choices") or []
    if choices and isinstance(choices, list):
        message = choices[0].get("message") or {}
        text = message.get("content") or ""

    usage = raw.get("usage") or {}
    tokens_sent = usage.get("prompt_tokens")
    tokens_received = usage.get("completion_tokens")

    return GenerateResult(
        text=text.strip() if isinstance(text, str) else "",
        raw=raw,
        tokens_sent=int(tokens_sent) if isinstance(tokens_sent, int) else None,
        tokens_received=int(tokens_received)
        if isinstance(tokens_received, int)
        else None,
    )


def check_dependencies(cfg: OpenAICompatibleConfig) -> CheckResult:
    """Standard preflight check for OpenAI-compatible adapters: curl on
    PATH and (when required) the API-key env var set."""
    missing: list[str] = []
    if shutil.which("curl") is None:
        missing.append("binary:curl")
    if cfg.api_key_env and not os.environ.get(cfg.api_key_env):
        missing.append(f"env:{cfg.api_key_env}")
    return CheckResult(ok=not missing, missing=missing)
