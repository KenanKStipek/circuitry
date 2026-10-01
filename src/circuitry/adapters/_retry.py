"""Shared retry classification for adapter call failures.

``core.prompt``'s retry loop needs to know, for a failed attempt, whether
trying again is worth it at all: a 429 or a dropped connection might
succeed on the next try, a 401 or a 404 never will. Adapters that can tell
the difference raise :class:`AdapterCallError` instead of a bare
``RuntimeError`` so that classification travels with the exception instead
of being re-derived from its message text.

Curl-based adapters (openai, anthropic, ollama, watsonx, replicate, and the
~20 OpenAI-compatible providers via ``_openai_compat``) all invoke curl with
``--fail-with-body``, which collapses every HTTP 4xx/5xx response into the
same generic exit code (22) — the status itself never reaches
``proc.returncode``. Reading it would normally mean adding a
``-w``/``--write-out`` flag to the curl invocation, but curl still prints it
to stderr on its own (``curl: (22) The requested URL returned error: 429``),
so :func:`classify_curl_exit` reads it from there instead. When that line
isn't present (stderr suppressed, an unexpected curl build, ...) the status
is unknown and the failure is *not* retried — guessing wrong in the
retryable direction could spin on a request that will never succeed (a bad
API key, a malformed body), so the conservative read wins.

None of these adapters capture response headers today, so
:attr:`RetryInfo.retry_after` is always ``None`` for them — honouring a
provider's ``Retry-After`` header needs that capture, which is itself a curl
argv change (tracked separately, alongside the header/body hardening that
moves credentials off the command line).
"""

from __future__ import annotations

import re
from dataclasses import dataclass

#: curl exit codes that mean the request never got a reply at all — DNS
#: failure (6), couldn't connect (7), operation timeout (28), SSL connect
#: error (35), the server stopped responding mid-transfer (52), the network
#: failed partway through (56). Every one of these is a transient,
#: infrastructure-level failure worth retrying regardless of what the
#: request was.
RETRYABLE_CURL_EXIT_CODES = frozenset({6, 7, 28, 35, 52, 56})

#: ``--fail-with-body`` always exits 22 on an HTTP error; curl still writes
#: the status to stderr in this form.
_CURL_STATUS_RE = re.compile(r"returned error:\s*(\d{3})")


def _status_is_retryable(status: int) -> bool:
    return status == 429 or status == 408 or 500 <= status <= 599


@dataclass(frozen=True)
class RetryInfo:
    """What a failed adapter call tells ``core.prompt``'s retry loop.

    ``retryable`` is the classification itself: 429, 408, 5xx and
    connection-level failures are retryable; everything else (400/401/403/
    404/422, a missing key, a status that couldn't be read) is not.
    ``status`` is the provider's HTTP status when known. ``retry_after`` is
    the provider's own ``Retry-After`` header value, verbatim (seconds or an
    HTTP date) — only set by an adapter that can see response headers.
    """

    retryable: bool
    status: int | None = None
    retry_after: str | None = None


NOT_RETRYABLE = RetryInfo(retryable=False)


def classify_curl_exit(returncode: int, stderr: str) -> RetryInfo:
    """Classify a failed curl-based adapter call from its exit code/stderr."""
    if returncode in RETRYABLE_CURL_EXIT_CODES:
        return RetryInfo(retryable=True)
    if returncode == 22:
        match = _CURL_STATUS_RE.search(stderr or "")
        if match:
            status = int(match.group(1))
            return RetryInfo(retryable=_status_is_retryable(status), status=status)
        return NOT_RETRYABLE
    return NOT_RETRYABLE


def classify_litellm_exception(exc: BaseException) -> RetryInfo:
    """Classify a litellm SDK exception by its ``status_code``/type.

    litellm's exception hierarchy mirrors the OpenAI SDK's: most carry a
    ``status_code`` attribute, and connection/timeout failures that precede
    any HTTP response (``APIConnectionError``, ``Timeout``,
    ``ServiceUnavailableError``) don't. Read by attribute name rather than
    ``isinstance`` against ``litellm.exceptions`` so this works whether or
    not litellm is importable in the current process (adapters/litellm.py
    imports it lazily, the same way).
    """
    status = getattr(exc, "status_code", None)
    if isinstance(status, int) and not isinstance(status, bool):
        retry_after = None
        response = getattr(exc, "response", None)
        headers = getattr(response, "headers", None)
        if headers is not None:
            try:
                retry_after = headers.get("Retry-After") or headers.get("retry-after")
            except AttributeError:
                retry_after = None
        return RetryInfo(
            retryable=_status_is_retryable(status), status=status, retry_after=retry_after
        )
    if type(exc).__name__ in ("Timeout", "APIConnectionError", "ServiceUnavailableError"):
        return RetryInfo(retryable=True)
    return NOT_RETRYABLE


class AdapterCallError(RuntimeError):
    """An adapter call failure carrying its :class:`RetryInfo`.

    Raised by adapters that can classify their own failure (curl-based ones
    via :func:`classify_curl_exit`, litellm via
    :func:`classify_litellm_exception`) in place of a bare ``RuntimeError``,
    so ``core.prompt``'s retry loop can read ``retry_info`` directly instead
    of re-deriving it from the message text. An adapter that raises a plain
    exception instead is treated as not retryable — the conservative default
    matches today's behaviour of never retrying anything.
    """

    def __init__(self, message: str, *, retry_info: RetryInfo = NOT_RETRYABLE) -> None:
        super().__init__(message)
        self.retry_info = retry_info


def classify_exception(exc: BaseException | None) -> RetryInfo:
    """``RetryInfo`` for any exception a dispatch attempt raised.

    ``None`` (nothing raised) and anything that isn't an
    :class:`AdapterCallError` classify as not retryable.
    """
    if isinstance(exc, AdapterCallError):
        return exc.retry_info
    return NOT_RETRYABLE
