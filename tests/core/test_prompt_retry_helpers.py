"""Unit tests for the small helpers backing #263 part 2's retry/backoff and
#281's raw-reply capping, isolated from the full dispatch loop.
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone

from circuitry.adapters._retry import (
    RETRY_BACKOFF_CAP_MS,
    RetryInfo,
    next_backoff_delay_ms,
    parse_retry_after_seconds,
)
from circuitry.core.prompt import _RAW_REPLY_CAP_CHARS, _cap_reply_text


def test_cap_reply_text_leaves_short_text_untouched() -> None:
    assert _cap_reply_text("short") == "short"


def test_cap_reply_text_truncates_and_notes_total_length() -> None:
    text = "x" * (_RAW_REPLY_CAP_CHARS + 500)
    capped = _cap_reply_text(text)
    assert capped.startswith("x" * _RAW_REPLY_CAP_CHARS)
    assert str(len(text)) in capped
    assert len(capped) < len(text)


_FIXED_NOW = datetime(2026, 1, 1, 0, 0, 0, tzinfo=timezone.utc)


def test_parse_retry_after_seconds_reads_a_bare_integer() -> None:
    assert parse_retry_after_seconds("30") == 30.0


def test_parse_retry_after_seconds_reads_an_imf_fixdate() -> None:
    http_date = (_FIXED_NOW + timedelta(seconds=120)).strftime("%a, %d %b %Y %H:%M:%S GMT")
    assert parse_retry_after_seconds(http_date, now=_FIXED_NOW) == 120.0


def test_parse_retry_after_seconds_reads_an_rfc_850_date() -> None:
    http_date = (_FIXED_NOW + timedelta(seconds=120)).strftime("%A, %d-%b-%y %H:%M:%S GMT")
    assert parse_retry_after_seconds(http_date, now=_FIXED_NOW) == 120.0


def test_parse_retry_after_seconds_reads_an_asctime_date() -> None:
    http_date = (_FIXED_NOW + timedelta(seconds=120)).strftime("%a %b %e %H:%M:%S %Y")
    assert parse_retry_after_seconds(http_date, now=_FIXED_NOW) == 120.0


def test_parse_retry_after_seconds_floors_a_past_date_at_zero() -> None:
    http_date = (_FIXED_NOW - timedelta(seconds=120)).strftime("%a, %d %b %Y %H:%M:%S GMT")
    assert parse_retry_after_seconds(http_date, now=_FIXED_NOW) == 0.0


def test_parse_retry_after_seconds_returns_none_for_garbage() -> None:
    assert parse_retry_after_seconds("not a date or a number") is None
    assert parse_retry_after_seconds("") is None


def test_parse_retry_after_seconds_rejects_a_sign() -> None:
    assert parse_retry_after_seconds("+30") is None
    assert parse_retry_after_seconds("-30") is None


def test_parse_retry_after_seconds_rejects_a_fraction() -> None:
    assert parse_retry_after_seconds("1.5") is None


def test_parse_retry_after_seconds_rejects_inf_and_nan() -> None:
    assert parse_retry_after_seconds("inf") is None
    assert parse_retry_after_seconds("Infinity") is None
    assert parse_retry_after_seconds("nan") is None


def test_parse_retry_after_seconds_rejects_digit_grouping() -> None:
    assert parse_retry_after_seconds("1_000") is None


def test_parse_retry_after_seconds_rejects_internal_whitespace() -> None:
    assert parse_retry_after_seconds("1 2") is None
    assert parse_retry_after_seconds("1\t2") is None


def test_next_backoff_delay_uses_retry_after_when_present() -> None:
    info = RetryInfo(retryable=True, retry_after="15")
    assert next_backoff_delay_ms(info, attempt_index=0, base_ms=1000) == 15_000


def test_next_backoff_delay_caps_a_very_long_retry_after() -> None:
    info = RetryInfo(retryable=True, retry_after="999999")
    assert next_backoff_delay_ms(info, attempt_index=0, base_ms=1000) == RETRY_BACKOFF_CAP_MS


def test_next_backoff_delay_without_retry_after_is_bounded_by_the_exponential_ceiling() -> None:
    info = RetryInfo(retryable=True)
    for attempt_index, expected_ceiling in ((0, 1000), (1, 2000), (2, 4000)):
        delay = next_backoff_delay_ms(info, attempt_index=attempt_index, base_ms=1000)
        assert 0 <= delay <= expected_ceiling
