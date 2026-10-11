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
