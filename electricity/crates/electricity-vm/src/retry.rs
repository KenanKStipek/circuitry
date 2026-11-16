//! `adapters/_retry.py`, ported -- shared retry classification for any
//! retrying effect (prompt, tool, `use`), issue #449's gate lane item 5.
//! Moved out of `exec/tool.rs`'s own private copy (`RETRY_BACKOFF_CAP_MS`,
//! `full_jitter_backoff_ms`) so lane E's HTTP-family tool branch and
//! lane F2's adapter dispatch share one implementation instead of each
//! growing their own.

use rand::Rng;

/// `adapters/_retry.py::RETRY_BACKOFF_CAP_MS` -- the exponential-backoff
/// ceiling for a retryable failure's wait, including one driven by a
/// provider's own `Retry-After`.
pub const RETRY_BACKOFF_CAP_MS: u32 = 60_000;

/// `adapters/_retry.py::_status_is_retryable`/`status_is_retryable` --
/// 429, 408, and every 5xx.
pub fn status_is_retryable(status: u16) -> bool {
    status == 429 || status == 408 || (500..=599).contains(&status)
}

/// `core/tool.py::_HTTP_FAMILY_PROVIDERS` -- the only providers whose
/// own retry decision this crate ever classifies by status/cause
/// rather than retrying unconditionally.
fn is_http_family_provider(provider: &str) -> bool {
    matches!(
        provider,
        "http" | "web_fetch" | "webhook" | "linear" | "systemone"
    )
}

/// `core/tool.py::ToolRuntime._is_retryable_failure` (PR #454) --
/// *provider* already normalized (`.strip().lower()`);
/// *status_code* is a status an *unraised*, `ok: false` result itself
/// carried (lane E's own HTTP tool -- no M0/M1-A provider ever
/// produces one); *cause* is the raised [`electricity_tools::ToolError`]'s
/// own [`electricity_tools::ErrorCause`], when the failure was raised
/// rather than returned. A non-HTTP-family provider (every provider
/// this crate can build today) always retries, exactly as every M0-H
/// `json` failure already did.
///
/// *is_expect_failure* is
/// `core/tool.py:838-840`'s own override: a failed `expect:` (raised
/// or not) is *always* retryable, regardless of what *provider*/
/// *status_code*/*cause* would otherwise say -- unlike an HTTP status,
/// a failed `expect:` carries no classification of its own, and the
/// motivating case (a flaky image-generation result) is exactly "try
/// again". Checked before the HTTP-family short-circuit below, not
/// just folded into one of *cause*'s own arms, so lane E's own
/// HTTP-family expect check doesn't have to invent a dedicated
/// [`electricity_tools::ErrorCause`] variant just to reach it.
pub fn is_tool_failure_retryable(
    provider: &str,
    status_code: Option<u16>,
    cause: Option<&electricity_tools::ErrorCause>,
    is_expect_failure: bool,
) -> bool {
    if is_expect_failure {
        return true;
    }
    if !is_http_family_provider(provider) {
        return true;
    }
    if let Some(status) = status_code {
        return status_is_retryable(status);
    }
    match cause {
        Some(electricity_tools::ErrorCause::Http { status, .. }) => status_is_retryable(*status),
        Some(electricity_tools::ErrorCause::NoReply) => true,
        // Issue #522: a prompt's own bare `SchemaValidationError` cause
        // carries the planner's rejected reply for `raw_payload`, never a
        // retry classification of its own -- this `http`-provider-only
        // branch never even runs for a `prompt` effect's own decode/schema
        // failure (that retry decision is `exec/prompt.rs`'s own
        // `decode_failed`/`schema_invalid` rule, independent of *cause*),
        // but an exhaustive match still has to place it somewhere, and
        // Python has no classification for it either.
        Some(electricity_tools::ErrorCause::SchemaRejected { .. })
        | Some(electricity_tools::ErrorCause::None)
        | None => false,
    }
}

/// `adapters/_retry.py::RetryInfo` -- what a failed call tells a retry
/// loop: whether it's worth trying again, the provider's HTTP status
/// when known, and the provider's own `Retry-After` header value
/// verbatim (seconds or an HTTP date) when one was seen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetryInfo {
    pub retryable: bool,
    pub status: Option<u16>,
    pub retry_after: Option<String>,
    /// `adapters/_retry.py::AdapterCallError.error_code` -- the
    /// provider's own error code (`error.code`, else `error.type`,
    /// each only when a non-empty string). Lives on `RetryInfo` rather
    /// than a separate field on [`crate::adapter::AdapterError`]
    /// (issue #660): every construction site here already carries a
    /// `RetryInfo`, and `AdapterError` has far more of them.
    pub error_code: Option<String>,
}

impl RetryInfo {
    pub fn not_retryable() -> Self {
        RetryInfo::default()
    }

    pub fn retryable() -> Self {
        RetryInfo {
            retryable: true,
            ..Default::default()
        }
    }
}

/// RFC 9110 section 10.2.3's `delay-seconds` grammar: one or more ASCII
/// digits, nothing else.
fn is_delay_seconds(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// `circuitry.http_conventions.parse_retry_after_seconds`, ported (issue
/// #778 step 7, H2) -- a `Retry-After` header value as seconds from
/// *now*, per RFC 9110 section 10.2.3: all-ASCII-digits means that many
/// seconds; otherwise the value is parsed as an HTTP-date (IMF-fixdate
/// via [`chrono::DateTime::parse_from_rfc2822`], plus the obsolete RFC
/// 850 and asctime forms a recipient must still accept,
/// [`parse_rfc850_date`]/[`parse_asctime_date`]), and the delay is that
/// date minus *now*, floored at 0. `None` for anything else -- a sign, a
/// fraction, `inf`/`nan`, whitespace inside, or an unparseable date --
/// never "wait forever" or a crash.
pub fn parse_retry_after_seconds_at(
    value: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<f64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if is_delay_seconds(trimmed) {
        return trimmed.parse::<f64>().ok().map(|seconds| seconds.max(0.0));
    }
    let when = chrono::DateTime::parse_from_rfc2822(trimmed)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .or_else(|| parse_rfc850_date(trimmed))
        .or_else(|| parse_asctime_date(trimmed))?;
    let seconds = (when - now).num_milliseconds() as f64 / 1000.0;
    Some(seconds.max(0.0))
}

/// [`parse_retry_after_seconds_at`] against the real current time --
/// every production call site; tests use the deterministic form
/// directly, on a fixed `now`.
pub fn parse_retry_after_seconds(value: &str) -> Option<f64> {
    parse_retry_after_seconds_at(value, chrono::Utc::now())
}

const MONTH_ABBREVIATIONS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn month_number(abbreviation: &str) -> Option<u32> {
    MONTH_ABBREVIATIONS
        .iter()
        .position(|candidate| *candidate == abbreviation)
        .map(|index| index as u32 + 1)
}

/// `email.utils._parsedate_tz`'s own two-digit-year century guess --
/// confirmed directly against `email.utils.parsedate_to_datetime`: 69
/// and above is 19xx, 68 and below is 20xx (the standard POSIX
/// `strptime`-style pivot).
fn four_digit_year(two_digit: u32) -> i32 {
    if two_digit >= 69 {
        1900 + two_digit as i32
    } else {
        2000 + two_digit as i32
    }
}

/// The obsolete RFC 850 HTTP-date grammar recipients must still accept:
/// `Sunday, 06-Nov-94 08:49:37 GMT` -- a long weekday name (ignored, never
/// cross-checked against the date), `DD-Mon-YY`, `HH:MM:SS`, and a
/// literal `GMT` (`split_whitespace` collapses the comma's own trailing
/// space the same way it collapses asctime's double space before a
/// single-digit day).
fn parse_rfc850_date(trimmed: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let mut tokens = trimmed.split_whitespace();
    let weekday = tokens.next()?;
    if !weekday.ends_with(',') {
        return None;
    }
    let date = tokens.next()?;
    let time = tokens.next()?;
    let zone = tokens.next()?;
    if tokens.next().is_some() || zone != "GMT" {
        return None;
    }
    let mut date_parts = date.split('-');
    let day: u32 = date_parts.next()?.parse().ok()?;
    let month = month_number(date_parts.next()?)?;
    let two_digit_year: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() {
        return None;
    }
    let year = four_digit_year(two_digit_year);
    let (hour, minute, second) = parse_hms(time)?;
    build_utc(year, month, day, hour, minute, second)
}

/// The obsolete asctime HTTP-date grammar recipients must still accept:
/// `Sun Nov  6 08:49:37 1994` -- no weekday-comma, no explicit zone (HTTP's
/// own asctime form is always implicitly GMT), and a day field that may be
/// space-padded for a single digit (`split_whitespace` collapses that
/// extra space, so this never needs to check for it specially).
fn parse_asctime_date(trimmed: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let mut tokens = trimmed.split_whitespace();
    let _weekday = tokens.next()?;
    let month = month_number(tokens.next()?)?;
    let day: u32 = tokens.next()?.parse().ok()?;
    let time = tokens.next()?;
    let year: i32 = tokens.next()?.parse().ok()?;
    if tokens.next().is_some() {
        return None;
    }
    let (hour, minute, second) = parse_hms(time)?;
    build_utc(year, month, day, hour, minute, second)
}

fn parse_hms(time: &str) -> Option<(u32, u32, u32)> {
    let mut parts = time.split(':');
    let hour: u32 = parts.next()?.parse().ok()?;
    let minute: u32 = parts.next()?.parse().ok()?;
    let second: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((hour, minute, second))
}

fn build_utc(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone;
    chrono::Utc
        .with_ymd_and_hms(year, month, day, hour, minute, second)
        .single()
}

/// `adapters/_retry.py::next_backoff_delay_ms` -- exponential backoff
/// with full jitter, starting from *base_ms* and capped at
/// [`RETRY_BACKOFF_CAP_MS`]; *attempt_index* is the 0-based attempt that
/// just failed. A provider's own `Retry-After` (via *retry_info*)
/// overrides the computed wait outright, still capped.
pub fn next_backoff_delay_ms(retry_info: &RetryInfo, attempt_index: u32, base_ms: u32) -> u32 {
    if let Some(retry_after) = &retry_info.retry_after {
        if let Some(seconds) = parse_retry_after_seconds(retry_after) {
            let ms = (seconds * 1000.0) as u64;
            return ms.min(u64::from(RETRY_BACKOFF_CAP_MS)) as u32;
        }
    }
    // `min(RETRY_BACKOFF_CAP_MS, base_ms * (2**attempt_index))` -- `u64`
    // throughout so `base_ms * 2**attempt_index` can't overflow `u32`
    // for any `attempt_index` this crate's own `RetryPolicy::max_attempts`
    // allows before the `.min` clamps it back down.
    let scaled = u64::from(base_ms).saturating_mul(1u64 << attempt_index.min(32));
    let ceiling = scaled.min(u64::from(RETRY_BACKOFF_CAP_MS)) as u32;
    if ceiling == 0 {
        return 0;
    }
    rand::thread_rng().gen_range(0..=ceiling)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_now() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
    }

    #[test]
    fn non_http_family_providers_always_retry() {
        assert!(is_tool_failure_retryable("json", None, None, false));
        assert!(is_tool_failure_retryable(
            "shell",
            None,
            Some(&electricity_tools::ErrorCause::None),
            false
        ));
    }

    #[test]
    fn http_family_classifies_by_status_code_first() {
        assert!(is_tool_failure_retryable("http", Some(500), None, false));
        assert!(!is_tool_failure_retryable("http", Some(404), None, false));
    }

    #[test]
    fn http_family_falls_back_to_the_raised_causes_classification() {
        assert!(is_tool_failure_retryable(
            "http",
            None,
            Some(&electricity_tools::ErrorCause::NoReply),
            false
        ));
        assert!(is_tool_failure_retryable(
            "webhook",
            None,
            Some(&electricity_tools::ErrorCause::Http {
                status: 503,
                retry_after: None,
            }),
            false
        ));
        assert!(!is_tool_failure_retryable(
            "webhook",
            None,
            Some(&electricity_tools::ErrorCause::Http {
                status: 400,
                retry_after: None,
            }),
            false
        ));
        assert!(!is_tool_failure_retryable(
            "linear",
            None,
            Some(&electricity_tools::ErrorCause::None),
            false
        ));
        assert!(!is_tool_failure_retryable("linear", None, None, false));
    }

    #[test]
    fn an_expect_failure_always_retries_even_for_an_http_family_tool() {
        // `core/tool.py:838-840`'s
        // own `retryable = True` on any failed `expect:` overrides even
        // an HTTP-family provider's own non-retryable status/cause --
        // 400 alone would never retry (the test above pins that), but a
        // failed `expect:` on that same call always does.
        assert!(is_tool_failure_retryable(
            "webhook",
            None,
            Some(&electricity_tools::ErrorCause::Http {
                status: 400,
                retry_after: None,
            }),
            true
        ));
        assert!(is_tool_failure_retryable("linear", Some(404), None, true));
    }

    #[test]
    fn status_is_retryable_matches_pythons_own_set() {
        assert!(status_is_retryable(429));
        assert!(status_is_retryable(408));
        assert!(status_is_retryable(500));
        assert!(status_is_retryable(599));
        assert!(!status_is_retryable(404));
        assert!(!status_is_retryable(401));
    }

    #[test]
    fn parse_retry_after_seconds_reads_a_plain_number() {
        assert_eq!(parse_retry_after_seconds("5"), Some(5.0));
    }

    #[test]
    fn parse_retry_after_seconds_rejects_garbage() {
        assert_eq!(parse_retry_after_seconds("not a date"), None);
    }

    #[test]
    fn parse_retry_after_seconds_rejects_empty() {
        assert_eq!(parse_retry_after_seconds(""), None);
        assert_eq!(parse_retry_after_seconds("   "), None);
    }

    #[test]
    fn next_backoff_delay_ms_is_bounded_by_the_cap() {
        for attempt in 0..40 {
            let delay = next_backoff_delay_ms(&RetryInfo::retryable(), attempt, 1_000);
            assert!(delay <= RETRY_BACKOFF_CAP_MS);
        }
    }

    #[test]
    fn parse_retry_after_seconds_rejects_digit_grouping() {
        assert_eq!(parse_retry_after_seconds_at("1_000", fixed_now()), None);
    }

    #[test]
    fn parse_retry_after_seconds_rejects_a_sign_and_a_fraction() {
        assert_eq!(parse_retry_after_seconds_at("+30", fixed_now()), None);
        assert_eq!(parse_retry_after_seconds_at("-30", fixed_now()), None);
        assert_eq!(parse_retry_after_seconds_at("1.5", fixed_now()), None);
    }

    #[test]
    fn parse_retry_after_seconds_rejects_inf_and_nan() {
        assert_eq!(parse_retry_after_seconds_at("inf", fixed_now()), None);
        assert_eq!(parse_retry_after_seconds_at("Infinity", fixed_now()), None);
        assert_eq!(parse_retry_after_seconds_at("nan", fixed_now()), None);
    }

    #[test]
    fn parse_retry_after_seconds_reads_an_imf_fixdate() {
        assert_eq!(
            parse_retry_after_seconds_at("Thu, 01 Jan 2026 00:02:00 GMT", fixed_now()),
            Some(120.0)
        );
    }

    #[test]
    fn parse_retry_after_seconds_reads_an_rfc_850_date() {
        assert_eq!(
            parse_retry_after_seconds_at("Thursday, 01-Jan-26 00:02:00 GMT", fixed_now()),
            Some(120.0)
        );
    }

    #[test]
    fn parse_retry_after_seconds_reads_an_asctime_date() {
        assert_eq!(
            parse_retry_after_seconds_at("Thu Jan  1 00:02:00 2026", fixed_now()),
            Some(120.0)
        );
    }

    #[test]
    fn parse_retry_after_seconds_floors_a_past_date_at_zero() {
        assert_eq!(
            parse_retry_after_seconds_at("Wed, 31 Dec 2025 23:58:00 GMT", fixed_now()),
            Some(0.0)
        );
    }

    #[test]
    fn next_backoff_delay_ms_honours_retry_after_capped() {
        let info = RetryInfo {
            retryable: true,
            status: Some(429),
            retry_after: Some("999999".to_string()),
            error_code: None,
        };
        assert_eq!(next_backoff_delay_ms(&info, 0, 0), RETRY_BACKOFF_CAP_MS);
    }
}
