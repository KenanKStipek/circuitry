"""Shared error-message formatting for curl-based adapters.

A failed curl call must never echo its own argv back to the caller: the
argv is exactly where a Bearer token, an ``api-key`` header, or Azure's
`extra_headers` secret lives. Every adapter that shells out to curl builds
its failure message through :func:`curl_failure_message` instead, which
reports what a caller needs (adapter, model, target URL with no
credentials, curl's exit status, and the provider's own explanation) and
nothing that could leak one. Callers pass the known credential values as
``secrets``; the helper masks those and also strips any userinfo
(``user:pass@host``) it finds in the rendered URL or hint text, so a
credential embedded in a configured base_url can't leak either.
"""

from __future__ import annotations

import json
import re
from collections.abc import Iterable
from typing import Any

# Strips userinfo (`user[:pass]@`) out of any `scheme://user:pass@host`
# substring in the final message, not just a `url=` field on its own: a
# configured base_url can also show up unmasked in adapter-built hint text
# (e.g. ollama's "not reachable" hint), and `re.sub` catches it wherever it
# lands. `cli/redaction.py`'s `_redact_url` can't be reused here because it
# requires the *entire* string to be a URL, which a prose error message never is.
_URL_USERINFO_RE = re.compile(r"://[^\s/@]+(?::[^\s/@]*)?@")


def _strip_url_userinfo(text: str) -> str:
    return _URL_USERINFO_RE.sub("://", text)


def _mask_secrets(text: str, secrets: Iterable[str]) -> str:
    for secret in secrets:
        if secret:
            text = text.replace(secret, "***")
    return text


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

    ``secrets`` are the raw credential values sent on this request (API key,
    bearer token, ...); they are masked out of ``stdout``/``stderr`` before
    the body is parsed and truncated, so a secret split by truncation can't
    survive as a partial match. Any URL embedded in ``url`` or ``hint`` that
    carries userinfo (``user:pass@host``) — e.g. a configured base_url — is
    stripped too, even though callers should not be passing one.
    """
    secrets = [s for s in secrets if s]
    stdout = _mask_secrets(stdout, secrets)
    stderr = _mask_secrets(stderr, secrets)
    detail = parse_error_body(stdout) or stderr.strip() or "no response body"
    model_part = f" model={model!r}" if model else ""
    message = (
        f"{adapter} request failed (curl exit {returncode}){model_part} "
        f"url={url}: {detail}"
    )
    if hint:
        message += f" {hint}"
    message = _mask_secrets(message, secrets)
    return _strip_url_userinfo(message)
