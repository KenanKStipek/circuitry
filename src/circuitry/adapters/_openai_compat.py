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
import shutil
import subprocess
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from ._curl_errors import curl_failure_message
from ._retry import AdapterCallError, classify_curl_exit
from .base import GenerateOptions, GenerateResult, last_user_index


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


def chat_messages(prompt: str, options: GenerateOptions) -> list[dict[str, Any]]:
    """The ``messages`` array for ``prompt`` and ``options``.

    Role-tagged turns go through as they are, except ``tool``: the Chat
    Completions API only accepts a tool turn that answers a ``tool_call_id``,
    so a bare one is sent as a user turn prefixed ``tool:``. Images become
    ``image_url`` parts (a ``data:`` URL for a local file) on the last user
    turn, which is added if the conversation has none.
    """
    messages: list[dict[str, Any]] = [
        {"role": "user", "content": f"tool: {m.content}"}
        if m.role == "tool"
        else {"role": m.role, "content": m.content}
        for m in options.messages
    ] or [{"role": "user", "content": prompt}]
    if options.images:
        index = last_user_index(messages)
        if index is None:
            messages.append({"role": "user", "content": ""})
            index = len(messages) - 1
        text = messages[index]["content"]
        messages[index]["content"] = [
            *([{"type": "text", "text": text}] if text else []),
            *(
                {"type": "image_url", "image_url": {"url": image.data_url()}}
                for image in options.images
            ),
        ]
    return messages


def sampling_fields(
    options: GenerateOptions, *, max_tokens_key: str = "max_tokens"
) -> dict[str, Any]:
    """Request-body fields for the portable knobs, then ``params`` verbatim."""
    fields: dict[str, Any] = {}
    if options.temperature is not None:
        fields["temperature"] = options.temperature
    if options.max_tokens is not None:
        fields[max_tokens_key] = options.max_tokens
    if options.stop:
        fields["stop"] = list(options.stop)
    fields.update(options.params)
    return fields


def parse_chat_response(raw: dict[str, Any]) -> tuple[str, str | None]:
    """``(text, finish_reason)`` from a Chat Completions response."""
    text = ""
    finish_reason = None
    choices = raw.get("choices") or []
    if choices and isinstance(choices, list) and isinstance(choices[0], dict):
        message = choices[0].get("message") or {}
        text = message.get("content") or ""
        reason = choices[0].get("finish_reason")
        finish_reason = reason if isinstance(reason, str) else None
    return (text.strip() if isinstance(text, str) else "", finish_reason)


def chat_completion(
    *,
    cfg: OpenAICompatibleConfig,
    model: str,
    prompt: str,
    timeout_seconds: int = 120,
    extra_headers: dict[str, str] | None = None,
    extra_body: dict[str, object] | None = None,
    options: GenerateOptions | None = None,
) -> GenerateResult:
    """Issue a single chat-completion request.

    ``extra_headers`` / ``extra_body`` cover provider-specific quirks
    (e.g. anthropic-version header, Azure deployment routing). ``options``
    carries the prompt effect's generation settings, turns and images; no
    seed is added for ``deterministic`` because not every provider in the
    family accepts one (``params: {seed: ...}`` sends one explicitly).
    """
    options = options or GenerateOptions()
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
        "messages": chat_messages(prompt, options),
    }
    if extra_body:
        payload.update(extra_body)
    payload.update(sampling_fields(options))

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
    # The body goes on stdin: with base64 images it can outgrow the argv size
    # limit (128 KiB per argument on Linux).
    cmd += ["--data-binary", "@-", url]

    try:
        proc = subprocess.run(
            cmd, input=json.dumps(payload), capture_output=True, text=True, check=False
        )
    except FileNotFoundError as e:
        raise RuntimeError("curl is not installed or not on PATH") from e

    if proc.returncode != 0:
        # Header values under 8 chars are treated as non-secret (a future
        # short header like "1" shouldn't get masked wherever it appears in
        # the message) — credentials are effectively never that short.
        secrets = [api_key] + [
            v for v in (extra_headers or {}).values() if len(v) >= 8
        ]
        raise AdapterCallError(
            curl_failure_message(
                adapter="OpenAI-compatible",
                model=model,
                url=url,
                returncode=proc.returncode,
                stdout=proc.stdout,
                stderr=proc.stderr,
                secrets=secrets,
            ),
            retry_info=classify_curl_exit(proc.returncode, proc.stderr),
        )

    try:
        raw = json.loads(proc.stdout)
    except json.JSONDecodeError as e:
        raise RuntimeError(
            f"Provider returned non-JSON response: {proc.stdout[:200]}"
        ) from e

    text, finish_reason = parse_chat_response(raw)

    usage = raw.get("usage") or {}
    tokens_sent = usage.get("prompt_tokens")
    tokens_received = usage.get("completion_tokens")

    return GenerateResult(
        text=text,
        raw=raw,
        tokens_sent=int(tokens_sent) if isinstance(tokens_sent, int) else None,
        tokens_received=int(tokens_received)
        if isinstance(tokens_received, int)
        else None,
        finish_reason=finish_reason,
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
