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
    matches!(provider, "http" | "web_fetch" | "webhook" | "linear")
}

/// `core/tool.py::ToolRuntime._is_retryable_failure` (review finding 1
/// on PR #454) -- *provider* already normalized (`.strip().lower()`);
/// *status_code* is a status an *unraised*, `ok: false` result itself
/// carried (lane E's own HTTP tool -- no M0/M1-A provider ever
/// produces one); *cause* is the raised [`electricity_tools::ToolError`]'s
/// own [`electricity_tools::ErrorCause`], when the failure was raised
/// rather than returned. A non-HTTP-family provider (every provider
/// this crate can build today) always retries, exactly as every M0-H
/// `json` failure already did.
///
/// *is_expect_failure* (review P2-8 on PR #454's second review) is
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
        Some(electricity_tools::ErrorCause::None) | None => false,
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

/// `adapters/_retry.py::parse_retry_after_seconds` -- a `Retry-After`
/// header value (seconds, or an HTTP-date) as seconds from now. `None`
/// when *value* is neither.
pub fn parse_retry_after_seconds(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(seconds) = trimmed.parse::<f64>() {
        return Some(seconds.max(0.0));
    }
    // RFC 2822 is the HTTP-date form `email.utils.parsedate_to_datetime`
    // (and so `adapters/_retry.py`) actually parses in practice --
    // RFC 850/asctime are both obsolete HTTP-date grammars no real
    // `Retry-After` sender still emits.
    let when = chrono::DateTime::parse_from_rfc2822(trimmed).ok()?;
    let now = chrono::Utc::now();
    let seconds = (when.with_timezone(&chrono::Utc) - now).num_milliseconds() as f64 / 1000.0;
    Some(seconds.max(0.0))
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
        // Review P2-8 on PR #454's second review: `core/tool.py:838-840`'s
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
    fn next_backoff_delay_ms_honours_retry_after_capped() {
        let info = RetryInfo {
            retryable: true,
            status: Some(429),
            retry_after: Some("120".to_string()),
        };
        assert_eq!(next_backoff_delay_ms(&info, 0, 0), RETRY_BACKOFF_CAP_MS);
    }
}
