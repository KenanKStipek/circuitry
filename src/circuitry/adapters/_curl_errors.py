"""Shared error-message formatting for curl-based adapters.

A failed curl call must never echo its own argv back to the caller: the
argv is exactly where a Bearer token, an ``api-key`` header, or Azure's
`extra_headers` secret lives. Every adapter that shells out to curl builds
its failure message through :func:`curl_failure_message` instead, which
reports what a caller needs (adapter, model, target URL with no
credentials, curl's exit status, and the provider's own explanation) and
nothing that could leak one.
"""

from __future__ import annotations

import json
from collections.abc import Iterable
from typing import Any


def parse_error_body(stdout: str) -> str | None:
    """Best-effort short explanation from a provider's JSON error body.

    OpenAI/Anthropic/Azure-shaped bodies nest the message under
    ``error.message``; Ollama and others use a flat ``error`` string.
    Falls back to the raw body (truncated) when neither shape matches, and
    to ``None`` when there is nothing to report.
    """
    text = (stdout or "").strip()
    if not text:
        return None
    try:
        parsed: Any = json.loads(text)
    except json.JSONDecodeError:
        return text[:200]
    if isinstance(parsed, dict):
        error = parsed.get("error")
        if isinstance(error, dict):
            message = error.get("message")
            if isinstance(message, str) and message:
                return message
        if isinstance(error, str) and error:
            return error
    return text[:200]


def curl_failure_message(
    *,
    adapter: str,
    model: str | None,
    url: str,
    returncode: int,
    stdout: str,
    stderr: str,
    hint: str = "",
    secrets: Iterable[str] = (),
) -> str:
    """Render a curl failure without ever including the curl command line.

    ``url`` must already be credential-free (query strings and paths, no
    userinfo or header values) — callers are responsible for that. ``secrets``
    are the raw credential values sent on this request (API key, bearer
    token, ...); they are stripped from the provider's response text too, in
    case a provider ever echoes back what it was sent.
    """
    detail = parse_error_body(stdout) or (stderr or "").strip() or "no response body"
    model_part = f" model={model!r}" if model else ""
    message = (
        f"{adapter} request failed (curl exit {returncode}){model_part} "
        f"url={url}: {detail}"
    )
    if hint:
        message += f" {hint}"
    for secret in secrets:
        if secret:
            message = message.replace(secret, "***")
    return message
