"""Shared curl plumbing for every adapter and plugin that shells out to curl.

A curl invocation here must never put a secret or a request body on its own
command line, and must never let a local user's ``~/.curlrc`` silently
change its behaviour:

- :func:`run_curl` always puts ``-q`` first (so ``~/.curlrc`` is ignored),
  sends headers through an inherited pipe fd via ``--config`` instead of
  ``-H`` (so a Bearer token or an ``x-api-key`` never shows up in ``ps``),
  and sends the request body, if any, on stdin via ``--data-binary @-``
  (so neither argv's 128 KiB-per-argument limit on Linux nor ``ps``
  visibility is a concern for a large prompt or image). Headers and the
  body use two different channels because both can't come from stdin at
  once.
- :func:`curl_failure_message` builds a failure message that reports what a
  caller needs — source name, target URL with userinfo and credential-like
  query values masked, curl's exit status, and the provider's own
  explanation — and never the argv or raw header/body content. Callers pass
  the known credential values as ``secrets``; the helper masks those too.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import urllib.parse
from collections.abc import Iterable, Mapping, Sequence
from typing import Any

# Strips userinfo (`user[:pass]@`) out of any `scheme://user:pass@host`
# substring in the final message, not just a `url=` field on its own: a
# configured base_url can also show up unmasked in adapter-built hint text
# (e.g. ollama's "not reachable" hint), and `re.sub` catches it wherever it
# lands. `cli/redaction.py`'s `_redact_url` can't be reused here because it
# requires the *entire* string to be a URL, which a prose error message never is.
_URL_USERINFO_RE = re.compile(r"://[^\s/@]+(?::[^\s/@]*)?@")

# Query-string keys that commonly carry a credential (search APIs, some
# webhook/trigger URLs). Matched case-insensitively against the whole key,
# so `api_key`, `apiKey`, `access_token` and `X-Auth-Token` all match.
_CREDENTIAL_QUERY_KEY_RE = re.compile(
    r"(key|token|secret|auth|password|pwd)", re.IGNORECASE
)


def _strip_url_userinfo(text: str) -> str:
    return _URL_USERINFO_RE.sub("://", text)


def _mask_credential_query_params(url: str) -> str:
    """Mask the value of any query-string parameter whose key looks like a
    credential, e.g. ``https://api.example.com/search?key=secret`` becomes
    ``...?key=%2A%2A%2A``. A caller passing a non-credential value through
    ``extra_params`` under a matching key is the rare false positive; a
    credential leaking into persisted run records is the common true one."""
    parsed = urllib.parse.urlsplit(url)
    if not parsed.query:
        return url
    pairs = urllib.parse.parse_qsl(parsed.query, keep_blank_values=True)
    masked = [
        (k, "***" if _CREDENTIAL_QUERY_KEY_RE.search(k) else v) for k, v in pairs
    ]
    new_query = urllib.parse.urlencode(masked)
    return urllib.parse.urlunsplit(parsed._replace(query=new_query))


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
    source: str,
    model: str | None = None,
    url: str,
    returncode: int,
    stdout: str,
    stderr: str,
    hint: str = "",
    secrets: Iterable[str] = (),
) -> str:
    """Render a curl failure without ever including the curl command line.

    ``source`` is the adapter or plugin name. ``secrets`` are the raw
    credential values sent on this request (API key, bearer token, ...);
    they are masked out of ``stdout``/``stderr`` before the body is parsed
    and truncated, so a secret split by truncation can't survive as a
    partial match. Any URL embedded in ``url`` or ``hint`` that carries
    userinfo (``user:pass@host``) or a credential-like query parameter
    (``?key=...``, ``?api_token=...``) is masked too, even though callers
    should not be passing one on purpose.
    """
    secrets = [s for s in secrets if s]
    stdout = _mask_secrets(stdout, secrets)
    stderr = _mask_secrets(stderr, secrets)
    detail = parse_error_body(stdout) or stderr.strip() or "no response body"
    model_part = f" model={model!r}" if model else ""
    message = (
        f"{source} request failed (curl exit {returncode}){model_part} "
        f"url={_mask_credential_query_params(url)}: {detail}"
    )
    if hint:
        message += f" {hint}"
    message = _mask_secrets(message, secrets)
    return _strip_url_userinfo(message)


# Curl's config-file format is line-oriented: a bare CR or LF inside a
# value ends that line early, letting the rest be read as a new option
# (`output`, `upload-file`, `url`, ...). No header name or value in this
# codebase is operator/document-controlled today, but `run_curl` is the
# shared entry point for every curl call, so this is enforced rather than
# assumed.
_FORBIDDEN_HEADER_CHARS = ("\r", "\n", "\0")


def _curl_config_file(headers: Sequence[tuple[str, str]]) -> str:
    """The ``--config`` file content for ``headers``.

    Curl's config-file quoting recognises ``\\\\`` and ``\\"`` inside a
    double-quoted value; both are escaped here so a header value containing
    either can't break out of its line.
    """
    lines = []
    for name, value in headers:
        for part, label in ((name, "name"), (value, "value")):
            if any(ch in part for ch in _FORBIDDEN_HEADER_CHARS):
                raise ValueError(f"header {label} contains a forbidden control character")
        escaped = value.replace("\\", "\\\\").replace('"', '\\"')
        lines.append(f'header = "{name}: {escaped}"')
    return "\n".join(lines) + ("\n" if lines else "")


def run_curl(
    *,
    url: str,
    headers: Mapping[str, str] | Sequence[tuple[str, str]] = (),
    data: str | None = None,
    timeout_seconds: int,
) -> subprocess.CompletedProcess[str]:
    """Run curl with ``-q`` first, headers off argv, and ``data`` (if given)
    on stdin.

    Headers travel through a pipe this process creates and keeps open just
    long enough for curl to read it via ``--config /dev/fd/<n>`` (an
    inherited file descriptor, passed with ``pass_fds`` — never a temp
    file). The body, when present, travels on the *actual* stdin via
    ``--data-binary @-``; headers can't use that channel too, since a
    request can have both. May raise ``FileNotFoundError`` if curl is not
    on PATH — callers translate that into their own error.

    Windows' ``Popen`` rejects a non-empty ``pass_fds`` and has no
    ``/dev/fd``, so a header-carrying call raises ``RuntimeError`` there
    instead of silently losing the headers.
    """
    header_items = list(headers.items()) if isinstance(headers, Mapping) else list(headers)
    if header_items and os.name == "nt":
        raise RuntimeError(
            "run_curl: sending headers via a pipe fd is not supported on Windows"
        )
    config_bytes = _curl_config_file(header_items).encode() if header_items else b""
    cmd = [
        "curl",
        "-q",
        "--silent",
        "--show-error",
        "--fail-with-body",
        "--max-time",
        str(int(timeout_seconds)),
    ]

    read_fd: int | None = None
    try:
        if header_items:
            read_fd, write_fd = os.pipe()
            os.write(write_fd, config_bytes)
            os.close(write_fd)
            cmd += ["--config", f"/dev/fd/{read_fd}"]
        if data is not None:
            cmd += ["--data-binary", "@-"]
        cmd.append(url)

        run_kwargs: dict[str, Any] = {}
        if read_fd is not None:
            run_kwargs["pass_fds"] = (read_fd,)

        return subprocess.run(
            cmd,
            input=data,
            capture_output=True,
            text=True,
            check=False,
            **run_kwargs,
        )
    finally:
        if read_fd is not None:
            os.close(read_fd)
