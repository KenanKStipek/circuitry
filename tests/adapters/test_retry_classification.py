"""Issue #263 part 2: classify a failed adapter call as retryable or not.

Every curl-based adapter invokes curl with ``--fail-with-body``, which exits
22 for *any* HTTP 4xx/5xx response with no status in ``proc.returncode`` —
but curl still writes the status to stderr
(``curl: (22) The requested URL returned error: 429``), verified against the
installed curl (8.7.1) rather than assumed from the manpage.
"""

from __future__ import annotations

from circuitry.adapters._retry import (
    NOT_RETRYABLE,
    AdapterCallError,
    RetryInfo,
    classify_curl_exit,
    classify_exception,
    classify_litellm_exception,
)

# ---------------------------------------------------------------------------
# classify_curl_exit
# ---------------------------------------------------------------------------


def test_curl_exit_22_with_a_429_status_line_is_retryable() -> None:
    stderr = "curl: (22) The requested URL returned error: 429\n"
    info = classify_curl_exit(22, stderr)
    assert info.retryable is True
    assert info.status == 429


def test_curl_exit_22_with_an_unparseable_stderr_is_not_retryable() -> None:
    """``--fail-with-body`` always exits 22 on an HTTP error; when stderr
    doesn't match the known status line, guessing retryable could spin on a
    request that will never succeed, so the conservative read wins."""
    info = classify_curl_exit(22, "curl: (22) something unexpected\n")
    assert info.retryable is False
    assert info.status is None


def test_curl_exit_22_with_a_404_status_line_is_not_retryable() -> None:
    info = classify_curl_exit(22, "curl: (22) The requested URL returned error: 404\n")
    assert info.retryable is False
    assert info.status == 404


def test_curl_exit_22_with_a_408_status_line_is_retryable() -> None:
    info = classify_curl_exit(22, "curl: (22) The requested URL returned error: 408\n")
    assert info.retryable is True
    assert info.status == 408


def test_curl_exit_22_with_a_5xx_status_line_is_retryable() -> None:
    info = classify_curl_exit(22, "curl: (22) The requested URL returned error: 503\n")
    assert info.retryable is True
    assert info.status == 503


def test_curl_connection_level_exit_codes_are_retryable_without_a_status() -> None:
    for code in (6, 7, 28, 35, 52, 56):
        info = classify_curl_exit(code, "")
        assert info.retryable is True, code
        assert info.status is None


def test_curl_unknown_nonzero_exit_is_not_retryable() -> None:
    info = classify_curl_exit(1, "")
    assert info.retryable is False


def test_curl_exit_22_with_a_429_status_line_and_retry_after_is_captured() -> None:
    """Regression for #319: the Retry-After value `run_curl` captures via
    `--write-out` on stderr (`circuitry-retry-after:<value>`) must reach
    `RetryInfo.retry_after` alongside the classification, without disturbing
    the exit-code status line this function already parses."""
    stderr = (
        "curl: (22) The requested URL returned error: 429\n"
        "circuitry-retry-after:2\n"
    )
    info = classify_curl_exit(22, stderr)
    assert info.retryable is True
    assert info.status == 429
    assert info.retry_after == "2"


def test_curl_connection_level_exit_also_captures_retry_after() -> None:
    info = classify_curl_exit(7, "circuitry-retry-after:5\n")
    assert info.retryable is True
    assert info.retry_after == "5"


def test_curl_exit_with_no_retry_after_marker_leaves_it_none() -> None:
    info = classify_curl_exit(22, "curl: (22) The requested URL returned error: 429\n")
    assert info.retry_after is None


# ---------------------------------------------------------------------------
# classify_litellm_exception
# ---------------------------------------------------------------------------


class _FakeLitellmStatusError(Exception):
    def __init__(self, status_code: int, retry_after: str | None = None) -> None:
        super().__init__("boom")
        self.status_code = status_code
        if retry_after is not None:
            self.response = _FakeResponse({"Retry-After": retry_after})


class _FakeResponse:
    def __init__(self, headers: dict[str, str]) -> None:
        self.headers = headers


class Timeout(Exception):
    pass


def test_litellm_status_code_429_is_retryable_with_retry_after() -> None:
    info = classify_litellm_exception(_FakeLitellmStatusError(429, retry_after="12"))
    assert info.retryable is True
    assert info.status == 429
    assert info.retry_after == "12"


def test_litellm_status_code_401_is_not_retryable() -> None:
    info = classify_litellm_exception(_FakeLitellmStatusError(401))
    assert info.retryable is False
    assert info.status == 401


def test_litellm_timeout_exception_type_is_retryable() -> None:
    info = classify_litellm_exception(Timeout())
    assert info.retryable is True
    assert info.status is None


def test_litellm_unrecognized_exception_is_not_retryable() -> None:
    info = classify_litellm_exception(ValueError("boom"))
    assert info is NOT_RETRYABLE


# ---------------------------------------------------------------------------
# classify_exception / AdapterCallError
# ---------------------------------------------------------------------------


def test_classify_exception_reads_retry_info_off_adapter_call_error() -> None:
    info = RetryInfo(retryable=True, status=429)
    err = AdapterCallError("rate limited", retry_info=info)
    assert classify_exception(err) is info


def test_classify_exception_defaults_unclassified_errors_to_not_retryable() -> None:
    assert classify_exception(RuntimeError("boom")) is NOT_RETRYABLE
    assert classify_exception(None) is NOT_RETRYABLE


# ---------------------------------------------------------------------------
# classify_exception's __cause__/__context__ walk — for an adapter (cyberdiner,
# watsonx's IAM token exchange before this fix) that raises a plain exception
# around a urllib/socket failure instead of classifying itself.
# ---------------------------------------------------------------------------


def test_a_wrapped_http_error_classifies_by_its_code() -> None:
    import urllib.error

    cause = urllib.error.HTTPError(
        "https://example.com", 429, "Too Many Requests", {}, None
    )
    try:
        raise RuntimeError("cyberdiner: HTTP 429 from https://example.com") from cause
    except RuntimeError as exc:
        info = classify_exception(exc)
    assert info.retryable is True
    assert info.status == 429


def test_a_wrapped_http_error_with_a_4xx_code_is_not_retryable() -> None:
    import urllib.error

    cause = urllib.error.HTTPError("https://example.com", 401, "Unauthorized", {}, None)
    try:
        raise RuntimeError("cyberdiner: HTTP 401 from https://example.com") from cause
    except RuntimeError as exc:
        info = classify_exception(exc)
    assert info.retryable is False
    assert info.status == 401


def test_a_wrapped_http_error_reads_retry_after_from_its_headers() -> None:
    import email.message
    import urllib.error

    headers = email.message.Message()
    headers["Retry-After"] = "30"
    cause = urllib.error.HTTPError(
        "https://example.com", 429, "Too Many Requests", headers, None
    )
    try:
        raise RuntimeError("boom") from cause
    except RuntimeError as exc:
        info = classify_exception(exc)
    assert info.retry_after == "30"


def test_a_wrapped_url_error_without_an_http_status_is_retryable() -> None:
    import urllib.error

    cause = urllib.error.URLError("connection refused")
    try:
        raise RuntimeError("cyberdiner: request failed: connection refused") from cause
    except RuntimeError as exc:
        info = classify_exception(exc)
    assert info.retryable is True
    assert info.status is None


def test_a_wrapped_timeout_error_is_retryable() -> None:
    cause = TimeoutError("timed out")
    try:
        raise RuntimeError("cyberdiner: request failed") from cause
    except RuntimeError as exc:
        info = classify_exception(exc)
    assert info.retryable is True


def test_a_wrapped_connection_error_is_retryable() -> None:
    cause = ConnectionError("reset by peer")
    try:
        raise RuntimeError("cyberdiner: request failed") from cause
    except RuntimeError as exc:
        info = classify_exception(exc)
    assert info.retryable is True


def test_a_plain_exception_with_no_retryable_cause_is_not_retryable() -> None:
    try:
        raise RuntimeError("cyberdiner: no tier resolved") from ValueError("bad input")
    except RuntimeError as exc:
        info = classify_exception(exc)
    assert info is NOT_RETRYABLE


def test_the_cause_chain_walk_stops_at_a_cycle_instead_of_looping_forever() -> None:
    exc = RuntimeError("boom")
    exc.__cause__ = exc  # pathological, must not infinite-loop
    assert classify_exception(exc) is NOT_RETRYABLE
