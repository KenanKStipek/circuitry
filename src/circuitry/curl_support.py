"""Shared curl plumbing for every adapter and plugin that shells out to curl.

A curl invocation here must never put a secret or a request body on its own
command line, and must never let a local user's ``~/.curlrc`` silently
change its behaviour:

- :func:`run_curl` always puts ``-q`` first (so ``~/.curlrc`` is ignored),
  sends the URL and headers through a ``--config`` file (so a Bearer token,
  an ``x-api-key``, a query-string credential, or ``user:pass@`` in the URL
  never shows up in ``ps`` — argv carries no request data at all), and
  sends the request body, if any, on stdin via ``--data-binary @-`` (so
  neither argv's 128 KiB-per-argument limit on Linux nor ``ps`` visibility
  is a concern for a large prompt or image). The config file travels
  through an inherited pipe fd on POSIX (``--config /dev/fd/<n>``, never a
  temp file); Windows has no such fd, so there it's written to a file in
  the per-user temp directory and removed in a ``finally``.
- :func:`curl_failure_message` builds a failure message that reports what a
  caller needs — source name, target URL with userinfo and credential-like
  query values masked, curl's exit status, and the provider's own
  explanation — and never the argv or raw header/body content. Callers pass
  the known credential values as ``secrets``; the helper masks those too.
"""

from __future__ import annotations

import functools
import json
import os
import re
import subprocess
import tempfile
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
    stderr = _strip_retry_after_marker(stderr)
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
# (`output`, `upload-file`, `url`, ...). No URL, header name or header
# value in this codebase is operator/document-controlled today, but
# `run_curl` is the shared entry point for every curl call, so this is
# enforced rather than assumed.
_FORBIDDEN_HEADER_CHARS = ("\r", "\n", "\0")

# A write to _run_curl_posix's pipe blocks once its kernel buffer fills
# (64 KiB on Linux, 16-64 KiB on macOS), before curl has even started, so
# --max-time can't save it; an oversized config would hang the call
# forever instead of failing. Comfortably under the smallest common
# buffer size.
_MAX_CONFIG_BYTES = 60_000


def _validate_no_control_chars(value: str, label: str) -> None:
    if any(ch in value for ch in _FORBIDDEN_HEADER_CHARS):
        raise ValueError(f"{label} contains a forbidden control character")


def _escape_config_value(value: str) -> str:
    """Curl's config-file quoting recognises ``\\\\`` and ``\\"`` inside a
    double-quoted value; both are escaped here so a value containing either
    can't break out of its line."""
    return value.replace("\\", "\\\\").replace('"', '\\"')


def _curl_config_file(url: str, headers: Sequence[tuple[str, str]]) -> str:
    """The ``--config`` file content for ``url`` and ``headers`` — the
    whole request, so argv never carries any of it (#314: a query-string
    credential or ``user:pass@`` in the URL is otherwise visible in ``ps``
    just like a header would be).
    """
    _validate_no_control_chars(url, "url")
    lines = [f'url = "{_escape_config_value(url)}"']
    for name, value in headers:
        _validate_no_control_chars(name, "header name")
        _validate_no_control_chars(value, "header value")
        lines.append(f'header = "{name}: {_escape_config_value(value)}"')
    return "\n".join(lines) + "\n"


# curl's `--write-out` format for capturing a response's `Retry-After`
# header (#319) without putting anything on argv or in a temp file:
# `%{stderr}` redirects the rest of the format to stderr instead of curl's
# default of stdout, which here carries the response body and must stay
# clean for `json.loads`. `%header{retry-after}` was added in curl 7.84.0
# (`_curl_supports_retry_after_header` gates it so older curl just doesn't
# get the flag). The marker prefix lets `extract_retry_after` find this
# specific line among whatever else curl writes to stderr (its own
# `curl: (22) ...` error line is also there on a failure).
_RETRY_AFTER_MARKER = "circuitry-retry-after:"
_RETRY_AFTER_WRITE_OUT_FORMAT = (
    "%{stderr}" + _RETRY_AFTER_MARKER + "%header{retry-after}\n"
)
_RETRY_AFTER_LINE_RE = re.compile(re.escape(_RETRY_AFTER_MARKER) + r"(.*)")

#: The real `subprocess.run`, captured at import time. The curl-version
#: probe below must always exercise the actual installed curl, never a
#: test's faked response for the request call `run_curl` itself makes —
#: tests commonly `monkeypatch.setattr("subprocess.run", fake_run)` for
#: exactly one call's shape, and a second, unexpected `curl --version` call
#: routed through that same fake would break them.
_real_subprocess_run = subprocess.run


@functools.lru_cache(maxsize=1)
def _curl_supports_retry_after_header() -> bool:
    """Whether the installed curl is new enough for ``%header{...}`` in
    ``--write-out`` (added in curl 7.84.0). Cached: this runs curl once per
    process, not once per request."""
    try:
        proc = _real_subprocess_run(
            ["curl", "--version"], capture_output=True, text=True, check=False, timeout=5
        )
    except (OSError, subprocess.SubprocessError):
        return False
    match = re.match(r"curl (\d+)\.(\d+)\.(\d+)", proc.stdout or "")
    if not match:
        return False
    version = tuple(int(g) for g in match.groups())
    return version >= (7, 84, 0)


def extract_retry_after(stderr: str) -> str | None:
    """The ``Retry-After`` header value :func:`run_curl` captured on
    ``stderr`` (via ``_RETRY_AFTER_WRITE_OUT_FORMAT``), verbatim — seconds
    or an HTTP date, whichever the provider sent. ``None`` when curl was too
    old to capture it, the response had no such header, or the request
    never produced an HTTP response at all (a connection failure never
    reaches the write-out stage).
    """
    match = _RETRY_AFTER_LINE_RE.search(stderr or "")
    if not match:
        return None
    value = match.group(1).strip()
    return value or None


def _strip_retry_after_marker(text: str) -> str:
    """``text`` with any ``_RETRY_AFTER_MARKER`` line removed — so the
    marker :func:`run_curl` writes to stderr on every call never leaks into
    a failure message built from that stderr."""
    lines = [ln for ln in text.splitlines() if not ln.startswith(_RETRY_AFTER_MARKER)]
    return "\n".join(lines)


def _run_curl_posix(
    cmd: list[str], config_bytes: bytes, data: str | None
) -> subprocess.CompletedProcess[str]:
    """POSIX: the config travels through a pipe this process creates and
    keeps open just long enough for curl to read it via ``--config
    /dev/fd/<n>`` (an inherited file descriptor, passed with ``pass_fds``)
    — never a temp file."""
    read_fd, write_fd = os.pipe()
    try:
        os.write(write_fd, config_bytes)
        os.close(write_fd)
        return subprocess.run(
            [*cmd, "--config", f"/dev/fd/{read_fd}"],
            input=data,
            capture_output=True,
            text=True,
            check=False,
            pass_fds=(read_fd,),
        )
    finally:
        os.close(read_fd)


def _run_curl_windows(
    cmd: list[str], config_bytes: bytes, data: str | None
) -> subprocess.CompletedProcess[str]:
    """Windows: `Popen` rejects a non-empty ``pass_fds`` and there's no
    ``/dev/fd``, so the config is written to a file in the per-user temp
    directory (``%TEMP%``, private to the user by default ACL) instead, and
    always removed afterwards — including on error."""
    fd, path = tempfile.mkstemp(prefix="circuitry-curl-", suffix=".conf")
    try:
        with os.fdopen(fd, "wb") as config_file:
            config_file.write(config_bytes)
        return subprocess.run(
            [*cmd, "--config", path],
            input=data,
            capture_output=True,
            text=True,
            check=False,
        )
    finally:
        os.remove(path)


def run_curl(
    *,
    url: str,
    headers: Mapping[str, str] | Sequence[tuple[str, str]] = (),
    data: str | None = None,
    timeout_seconds: int,
) -> subprocess.CompletedProcess[str]:
    """Run curl with ``-q`` first, the URL and headers off argv, and
    ``data`` (if given) on stdin.

    The URL and headers both travel through ``--config`` (see
    :func:`_curl_config_file`) — on POSIX via an inherited pipe fd
    (:func:`_run_curl_posix`), on Windows via a per-user temp file removed
    in a ``finally`` (:func:`_run_curl_windows`), since Windows' `Popen`
    has neither `pass_fds` nor `/dev/fd`. The body, when present, travels
    on the *actual* stdin via ``--data-binary @-``; the config can't use
    that channel too, since a request can have both. Raises ``ValueError``
    if ``url`` plus ``headers`` exceed ``_MAX_CONFIG_BYTES`` (that config is
    written to a pipe before curl starts on POSIX, and an oversized write
    would block forever instead of respecting ``timeout_seconds``). May
    raise ``FileNotFoundError`` if curl is not on PATH — callers translate that
    into their own error.
    """
    header_items = list(headers.items()) if isinstance(headers, Mapping) else list(headers)
    config_bytes = _curl_config_file(url, header_items).encode()
    if len(config_bytes) > _MAX_CONFIG_BYTES:
        raise ValueError(
            f"url + headers are {len(config_bytes)} bytes, over the "
            f"{_MAX_CONFIG_BYTES}-byte limit for a single curl --config write"
        )
    cmd = [
        "curl",
        "-q",
        "--silent",
        "--show-error",
        "--fail-with-body",
        "--max-time",
        str(int(timeout_seconds)),
    ]
    if _curl_supports_retry_after_header():
        cmd += ["--write-out", _RETRY_AFTER_WRITE_OUT_FORMAT]
    if data is not None:
        cmd += ["--data-binary", "@-"]

    if os.name == "nt":
        return _run_curl_windows(cmd, config_bytes, data)
    return _run_curl_posix(cmd, config_bytes, data)
