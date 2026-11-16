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

``curl_support.run_curl`` also captures a response's ``Retry-After`` header
on stderr (via ``--write-out``, curl >= 7.84 only — never argv or a temp
file) and :func:`classify_curl_exit` reads it from there via
``curl_support.extract_retry_after``, the same way :attr:`RetryInfo.status`
is read from the exit-code line. It's ``None`` on older curl, when the
response carried no such header, or when the failure never reached an HTTP
response at all (a connection failure, for instance).
"""

from __future__ import annotations

import random
import re
import urllib.error
from dataclasses import dataclass
from datetime import datetime, timezone

from ..curl_support import extract_retry_after

#: RFC 9110 section 10.2.3's `delay-seconds` grammar: one or more ASCII
#: digits, nothing else -- never a sign, a fraction, `inf`/`nan`, or
#: whitespace inside.
_RETRY_AFTER_SECONDS_RE = re.compile(r"^[0-9]+$", re.ASCII)

#: RFC 9110 section 5.6.7's three accepted `HTTP-date` forms, matched
#: strictly -- unlike :func:`email.utils.parsedate_to_datetime`, which
#: this module used to delegate to, each of these rejects anything that
#: deviates from the exact grammar at all (a missing or different time
#: zone text, a one-digit year, ...), rather than guessing (issue #778).
_WEEKDAY_SHORT = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")
_WEEKDAY_LONG = (
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
)
_MONTH_NAMES = (
    "Jan",
    "Feb",
    "Mar",
    "Apr",
    "May",
    "Jun",
    "Jul",
    "Aug",
    "Sep",
    "Oct",
    "Nov",
    "Dec",
)
_MONTH_INDEX = {name: index + 1 for index, name in enumerate(_MONTH_NAMES)}

#: `Sun, 06 Nov 1994 08:49:37 GMT`. `re.ASCII` throughout this module's
#: date grammars: without it, `\d` matches any Unicode decimal digit
#: (Arabic-Indic digits included), not only the ASCII ones RFC 9110's
#: own grammar means (issue #778).
_IMF_FIXDATE_RE = re.compile(
    r"^(?:"
    + "|".join(_WEEKDAY_SHORT)
    + r"), (\d{2}) ("
    + "|".join(_MONTH_NAMES)
    + r") (\d{4}) (\d{2}):(\d{2}):(\d{2}) GMT$",
    re.ASCII,
)
#: The obsolete RFC 850 date: `Sunday, 06-Nov-94 08:49:37 GMT`.
_RFC850_DATE_RE = re.compile(
    r"^(?:"
    + "|".join(_WEEKDAY_LONG)
    + r"), (\d{2})-("
    + "|".join(_MONTH_NAMES)
    + r")-(\d{2}) (\d{2}):(\d{2}):(\d{2}) GMT$",
    re.ASCII,
)
#: The asctime format: `Sun Nov  6 08:49:37 1994` (a one-digit day of
#: month keeps the leading space, never zero-padded).
_ASCTIME_RE = re.compile(
    r"^(?:"
    + "|".join(_WEEKDAY_SHORT)
    + r") ("
    + "|".join(_MONTH_NAMES)
    + r") ([ 0-9]\d) (\d{2}):(\d{2}):(\d{2}) (\d{4})$",
    re.ASCII,
)

#: Exponential-backoff ceiling for a retryable failure's wait, including one
#: driven by a provider's own ``Retry-After`` — a provider asking for longer
#: than this is still only waited out this long, so a single effect's
#: retries can't stall a run indefinitely. Shared by every retrying effect
#: (prompt, tool, use), not just adapter dispatch.
RETRY_BACKOFF_CAP_MS = 60_000

#: curl exit codes that mean the request never got a (complete) reply at
#: all — DNS failure (6), couldn't connect (7), operation timeout (28), SSL
#: connect error (35), the server stopped responding mid-transfer (52), the
#: network failed partway through (56), a premature EOF partway through the
#: body with fewer bytes delivered than promised (18 — the shape a streamed
#: reply's own mid-stream disconnect takes, #674). Every one of these is a
#: transient, infrastructure-level failure worth retrying regardless of
#: what the request was. Shared with electricity's own native-HTTP
#: transport (``transport::is_retryable_exit``), which must reproduce this
#: exact set.
RETRYABLE_CURL_EXIT_CODES = frozenset({6, 7, 18, 28, 35, 52, 56})

#: ``--fail-with-body`` always exits 22 on an HTTP error; curl still writes
#: the status to stderr in this form.
_CURL_STATUS_RE = re.compile(r"returned error:\s*(\d{3})")


def _status_is_retryable(status: int) -> bool:
    return status == 429 or status == 408 or 500 <= status <= 599


#: Public name for the same check — an HTTP-family tool effect (core.tool)
#: classifies its own status code the same way an adapter dispatch does.
status_is_retryable = _status_is_retryable


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
    retry_after = extract_retry_after(stderr)
    if returncode in RETRYABLE_CURL_EXIT_CODES:
        return RetryInfo(retryable=True, retry_after=retry_after)
    if returncode == 22:
        match = _CURL_STATUS_RE.search(stderr or "")
        if match:
            status = int(match.group(1))
            return RetryInfo(
                retryable=_status_is_retryable(status), status=status, retry_after=retry_after
            )
        return RetryInfo(retryable=False, retry_after=retry_after)
    return RetryInfo(retryable=False, retry_after=retry_after)


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
    exception instead falls back to :func:`_classify_cause_chain`, which
    reads the stdlib exception it wrapped (see ``classify_exception``).
    """

    def __init__(
        self,
        message: str,
        *,
        retry_info: RetryInfo = NOT_RETRYABLE,
        error_code: str | None = None,
    ) -> None:
        super().__init__(message)
        self.retry_info = retry_info
        self.error_code = error_code


def _classify_cause_chain(exc: BaseException) -> RetryInfo:
    """``RetryInfo`` for an exception that isn't an :class:`AdapterCallError`.

    Adapters built on ``urllib.request`` (cyberdiner, watsonx's IAM token
    exchange) raise a plain ``RuntimeError(...) from exc`` rather than
    classifying themselves — the retry-worthy information is still there,
    attached as ``__cause__`` (or ``__context__`` for an implicit chain).
    Walk it: ``urllib.error.HTTPError`` (checked before ``URLError``, which
    it subclasses) classifies by its ``.code`` the same way a curl/litellm
    status does, with ``Retry-After`` read off its headers when present;
    ``URLError`` with no HTTP status attached, a ``TimeoutError`` (the
    stdlib alias used for both ``socket.timeout`` and a bare timeout since
    Python 3.10), or a ``ConnectionError`` all mean the request never got a
    reply at all —
    the same transient, infrastructure-level condition
    :data:`RETRYABLE_CURL_EXIT_CODES` covers for curl. Anything else, or a
    chain that runs out without finding one of these, is not retryable.
    """
    seen: set[int] = set()
    current: BaseException | None = exc
    while current is not None and id(current) not in seen:
        seen.add(id(current))
        if isinstance(current, urllib.error.HTTPError):
            status = current.code
            retry_after = None
            headers = getattr(current, "headers", None)
            if headers is not None:
                try:
                    retry_after = headers.get("Retry-After") or headers.get("retry-after")
                except AttributeError:
                    retry_after = None
            return RetryInfo(
                retryable=_status_is_retryable(status), status=status, retry_after=retry_after
            )
        if isinstance(current, (urllib.error.URLError, TimeoutError, ConnectionError)):
            return RetryInfo(retryable=True)
        current = current.__cause__ or current.__context__
    return NOT_RETRYABLE


def classify_exception(exc: BaseException | None) -> RetryInfo:
    """``RetryInfo`` for any exception a dispatch attempt raised.

    ``None`` (nothing raised) classifies as not retryable. An
    :class:`AdapterCallError` carries its own classification. Anything else
    is walked via :func:`_classify_cause_chain` — an adapter that raises a
    plain exception around a retryable stdlib cause (a connection drop, an
    HTTP error urllib surfaces) still gets retried; one with no such cause
    does not.
    """
    if exc is None:
        return NOT_RETRYABLE
    if isinstance(exc, AdapterCallError):
        return exc.retry_info
    return _classify_cause_chain(exc)


def _build_datetime(
    year: int, month_name: str, day: str, hour: str, minute: str, second: str
) -> datetime | None:
    """A UTC :class:`datetime` from an HTTP-date's own already-matched
    pieces, or ``None`` for a calendar value no month actually has (day
    31 of a 30-day month, a non-leap-year Feb 29, ...)."""
    try:
        return datetime(
            year,
            _MONTH_INDEX[month_name],
            int(day),
            int(hour),
            int(minute),
            int(second),
            tzinfo=timezone.utc,
        )
    except ValueError:
        return None


def _parse_http_date(value: str) -> datetime | None:
    """*value* as one of RFC 9110 section 5.6.7's three accepted
    `HTTP-date` forms (IMF-fixdate, the obsolete RFC 850 date, asctime),
    matched strictly — ``None`` for anything else, including a value
    :func:`email.utils.parsedate_to_datetime` would itself accept but
    that deviates from the exact grammar (a missing ``GMT``, a
    different time zone). A two-digit RFC 850 year follows the common
    ``strptime``-style window: 00-68 means 20xx, 69-99 means 19xx.
    """
    match = _IMF_FIXDATE_RE.match(value)
    if match:
        day, month_name, year, hour, minute, second = match.groups()
        return _build_datetime(int(year), month_name, day, hour, minute, second)
    match = _RFC850_DATE_RE.match(value)
    if match:
        day, month_name, yy, hour, minute, second = match.groups()
        year = 2000 + int(yy) if int(yy) <= 68 else 1900 + int(yy)
        return _build_datetime(year, month_name, day, hour, minute, second)
    match = _ASCTIME_RE.match(value)
    if match:
        month_name, day, hour, minute, second, year = match.groups()
        return _build_datetime(int(year), month_name, day.strip(), hour, minute, second)
    return None


def parse_retry_after_seconds(value: str, *, now: datetime | None = None) -> float | None:
    """A ``Retry-After`` header value as seconds from *now* (``datetime.
    now(timezone.utc)`` when unset), per RFC 9110 section 10.2.3.

    All-ASCII-digits means that many seconds. Otherwise the value is
    parsed as an HTTP-date (:func:`_parse_http_date`'s own strict
    IMF-fixdate/RFC 850/asctime grammar), and the delay is that date
    minus *now*, floored at 0. Anything else — a sign, a fraction,
    ``inf``/``nan``, whitespace inside, or an unparseable date — returns
    ``None``, never "wait forever" or a crash.
    """
    value = (value or "").strip()
    if not value:
        return None
    if _RETRY_AFTER_SECONDS_RE.fullmatch(value):
        return float(value)
    when = _parse_http_date(value)
    if when is None:
        return None
    return max(0.0, (when - (now or datetime.now(timezone.utc))).total_seconds())


def next_backoff_delay_ms(retry_info: RetryInfo, *, attempt_index: int, base_ms: int) -> int:
    """How long to wait before the next retry: exponential backoff with full
    jitter, starting from ``base_ms`` and capped at :data:`RETRY_BACKOFF_CAP_MS`.

    ``attempt_index`` is the 0-based attempt that just failed, so the first
    retry's ceiling is ``base_ms`` and each subsequent one roughly doubles.
    A provider's own ``Retry-After`` (when the failing call could supply
    one) overrides the computed wait outright, still capped — the provider
    knows its own rate limit better than a generic curve does. Shared by
    every retrying effect (prompt, tool, use).
    """
    if retry_info.retry_after:
        retry_after_seconds = parse_retry_after_seconds(retry_info.retry_after)
        if retry_after_seconds is not None:
            # Capped in float space *before* the `int()`
            # conversion -- a huge `Retry-After` (309+ digits) makes
            # `retry_after_seconds` an `inf` float; `int(inf * 1000)`
            # raises `OverflowError`, which this cap must never let
            # through.
            return int(min(float(RETRY_BACKOFF_CAP_MS), retry_after_seconds * 1000))
    ceiling = min(RETRY_BACKOFF_CAP_MS, base_ms * (2**attempt_index))
    return int(random.uniform(0, ceiling))


def failure_fields(exc: BaseException) -> dict[str, object]:
    """``error_code``/``http_status`` for a failed adapter call, only the
    ones known for this exception."""
    fields: dict[str, object] = {}
    code = exc.error_code if isinstance(exc, AdapterCallError) else None
    if code:
        fields["error_code"] = code
    status = classify_exception(exc).status
    if status is not None:
        fields["http_status"] = status
    return fields
