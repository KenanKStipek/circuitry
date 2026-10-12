//! `curl_support.py`'s own failure-message/masking helpers, ported --
//! `curl_failure_message`, `parse_error_body`, the three maskers, and
//! the curl exit-code-to-[`RetryInfo`] mapping. Every adapter in this
//! crate that calls [`crate::transport::run_curl`] builds its own
//! failure message through [`curl_failure_message`], never directly:
//! this is the one place a secret gets masked out of a message before
//! it is ever returned (issue #503's own "Security" section -- no
//! request/response body, header value or key is ever logged, and
//! every failure text passes through this module's own masking before
//! `exec/prompt.rs`'s `redact()` ever sees it).

use crate::transport::CurlOutcome;
use electricity_value::Value;
use electricity_vm::retry::{RetryInfo, status_is_retryable};

/// `curl_support.py::parse_error_body` -- a best-effort short
/// explanation from a provider's JSON error body. `None` for an empty
/// body; the raw body (truncated to 200 *characters*, never bytes)
/// when it isn't JSON, or is JSON but not shaped like either an
/// OpenAI/Anthropic/Azure `{"error": {"message": ...}}` or a flat
/// Ollama-style `{"error": "..."}`.
pub fn parse_error_body(stdout: &str) -> Option<String> {
    let text = electricity_value::pycompat::py_strip(stdout);
    if text.is_empty() {
        return None;
    }
    let parsed = match electricity_json::loads(text) {
        Ok(value) => value,
        Err(_) => return Some(truncate_chars(text, 200)),
    };
    if let Some(dict) = parsed.as_dict() {
        if let Some(error) = dict.get(&Value::Str("error".to_string())) {
            if let Some(error_dict) = error.as_dict() {
                if let Some(Value::Str(message)) =
                    error_dict.get(&Value::Str("message".to_string()))
                {
                    if !message.is_empty() {
                        return Some(message.clone());
                    }
                }
            } else if let Value::Str(message) = error {
                if !message.is_empty() {
                    return Some(message.clone());
                }
            }
        }
    }
    Some(truncate_chars(text, 200))
}

/// `%.200s`-equivalent truncation by *character* count, never bytes --
/// a multi-byte codepoint is never split.
fn truncate_chars(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((byte_index, _)) => text[..byte_index].to_string(),
        None => text.to_string(),
    }
}

fn mask_secrets(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        if !secret.is_empty() {
            out = out.replace(secret, "***");
        }
    }
    out
}

/// `curl_support.py::_CREDENTIAL_QUERY_KEY_RE` -- a case-insensitive
/// *search* (not a full match) for any of these words anywhere in a
/// query key.
fn looks_like_credential_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    ["key", "token", "secret", "auth", "password", "pwd"]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// `urllib.parse.parse_qsl(keep_blank_values=True)` -- `&`-separated
/// `name=value` pairs (a bare `name` with no `=` keeps an empty
/// value), each percent-decoded with `+` read as a space (the
/// `application/x-www-form-urlencoded` convention `parse_qsl` uses by
/// default, `plus` left `True`).
fn parse_qsl(query: &str) -> Vec<(String, String)> {
    if query.is_empty() {
        return Vec::new();
    }
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (percent_decode_plus(k), percent_decode_plus(v)),
            None => (percent_decode_plus(pair), String::new()),
        })
        .collect()
}

fn percent_decode_plus(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `curl_support.py::_mask_credential_query_params` -- masks the
/// value of any query-string key matching [`looks_like_credential_key`],
/// then re-encodes the **whole** query with
/// [`electricity_value::pycompat::urlencode`] even when nothing
/// matched (so `%2A%2A%2A` for the mask, a space becomes `+`, exactly
/// mirrors `urllib.parse.urlencode`'s own default `quote_plus`
/// encoding).
fn mask_credential_query_params(url: &str) -> String {
    let Some(query_start) = url.find('?') else {
        return url.to_string();
    };
    let (before_query, rest) = url.split_at(query_start);
    let query_and_fragment = &rest[1..];
    let (query, fragment) = match query_and_fragment.find('#') {
        Some(idx) => (&query_and_fragment[..idx], Some(&query_and_fragment[idx..])),
        None => (query_and_fragment, None),
    };
    let pairs = parse_qsl(query);
    let masked: Vec<(String, Value)> = pairs
        .into_iter()
        .map(|(k, v)| {
            let value = if looks_like_credential_key(&k) {
                "***".to_string()
            } else {
                v
            };
            (k, Value::Str(value))
        })
        .collect();
    let new_query = electricity_value::pycompat::urlencode(&masked, false).unwrap_or_default();
    let mut result = format!("{before_query}?{new_query}");
    if let Some(fragment) = fragment {
        result.push_str(fragment);
    }
    result
}

/// `curl_support.py::_URL_USERINFO_RE` -- `://user[:pass]@` becomes
/// `://`, wherever it occurs in the text (hints included), never
/// requiring the whole string to be a URL.
fn strip_url_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(scheme_idx) = rest.find("://") else {
            out.push_str(rest);
            break;
        };
        let after_scheme = &rest[scheme_idx + 3..];
        let stop = after_scheme
            .find(|c: char| c.is_whitespace() || c == '/' || c == '@')
            .unwrap_or(after_scheme.len());
        let candidate = &after_scheme[..stop];
        if after_scheme.as_bytes().get(stop) == Some(&b'@') && !candidate.is_empty() {
            out.push_str(&rest[..scheme_idx + 3]);
            rest = &after_scheme[stop + 1..];
        } else {
            out.push_str(&rest[..scheme_idx + 3]);
            rest = after_scheme;
        }
    }
    out
}

/// `curl_support.py::curl_failure_message` -- renders a curl failure
/// without ever including the request body/headers. *secrets* are the
/// raw credential values this request sent; masked out of *stdout*/
/// *stderr* before the body is parsed, and out of the final message
/// (and its embedded URL/hint) again afterward, so a secret split by
/// truncation can't survive as a partial match.
#[allow(clippy::too_many_arguments)]
pub fn curl_failure_message(
    source: &str,
    model: Option<&str>,
    url: &str,
    returncode: i32,
    stdout: &str,
    stderr: &str,
    hint: &str,
    secrets: &[&str],
) -> String {
    let secrets: Vec<&str> = secrets.iter().copied().filter(|s| !s.is_empty()).collect();
    let stdout = mask_secrets(stdout, &secrets);
    let stderr = mask_secrets(stderr, &secrets);
    let detail = parse_error_body(&stdout)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let trimmed = electricity_value::pycompat::py_strip(&stderr);
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .unwrap_or_else(|| "no response body".to_string());
    let model_part = match model {
        Some(m) if !m.is_empty() => format!(" model={}", Value::Str(m.to_string()).py_repr()),
        _ => String::new(),
    };
    let mut message = format!(
        "{source} request failed (curl exit {returncode}){model_part} url={}: {detail}",
        mask_credential_query_params(url)
    );
    if !hint.is_empty() {
        message.push(' ');
        message.push_str(hint);
    }
    message = mask_secrets(&message, &secrets);
    strip_url_userinfo(&message)
}

/// `adapters/_retry.py::classify_curl_exit`, re-shaped around
/// [`CurlOutcome`]'s own `status`/`retry_after` fields instead of
/// Python's own stderr-text scrape (`transport.rs`'s own doc comment:
/// electricity never needs curl's `--write-out` trick, since it reads
/// the real response directly) -- same classification either way.
pub fn classify_curl_outcome(outcome: &CurlOutcome) -> RetryInfo {
    if crate::transport::is_retryable_exit(outcome.returncode) {
        return RetryInfo {
            retryable: true,
            status: None,
            retry_after: outcome.retry_after.clone(),
        };
    }
    if outcome.returncode == 22 {
        if let Some(status) = outcome.status {
            return RetryInfo {
                retryable: status_is_retryable(status),
                status: Some(status),
                retry_after: outcome.retry_after.clone(),
            };
        }
    }
    RetryInfo {
        retryable: false,
        status: None,
        retry_after: outcome.retry_after.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_error_body_reads_nested_error_message() {
        let body = r#"{"error": {"message": "Invalid API Key"}}"#;
        assert_eq!(parse_error_body(body), Some("Invalid API Key".to_string()));
    }

    #[test]
    fn parse_error_body_reads_a_flat_error_string() {
        let body = r#"{"error": "model \"m\" not found, try pulling it first"}"#;
        assert_eq!(
            parse_error_body(body),
            Some("model \"m\" not found, try pulling it first".to_string())
        );
    }

    #[test]
    fn parse_error_body_falls_back_to_a_truncated_raw_body() {
        let body = "x".repeat(250);
        assert_eq!(parse_error_body(&body), Some("x".repeat(200)));
    }

    #[test]
    fn parse_error_body_is_none_for_an_empty_body() {
        assert_eq!(parse_error_body(""), None);
        assert_eq!(parse_error_body("   "), None);
    }

    #[test]
    fn parse_error_body_handles_non_json_text() {
        assert_eq!(
            parse_error_body("<html>oops</html>"),
            Some("<html>oops</html>".to_string())
        );
    }

    #[test]
    fn mask_credential_query_params_masks_matching_keys_only() {
        let url = "https://api.example.com/search?key=secret&q=weather";
        assert_eq!(
            mask_credential_query_params(url),
            "https://api.example.com/search?key=%2A%2A%2A&q=weather"
        );
    }

    #[test]
    fn mask_credential_query_params_leaves_a_url_with_no_query_unchanged() {
        let url = "https://api.example.com/search";
        assert_eq!(mask_credential_query_params(url), url);
    }

    #[test]
    fn strip_url_userinfo_removes_credentials_wherever_they_occur() {
        let text = "connect to http://user:pass@127.0.0.1:1 failed";
        assert_eq!(
            strip_url_userinfo(text),
            "connect to http://127.0.0.1:1 failed"
        );
    }

    #[test]
    fn curl_failure_message_masks_the_key_in_the_body_url_and_message() {
        let message = curl_failure_message(
            "OpenAI-compatible",
            Some("m"),
            "http://user:s3cr3t@127.0.0.1/v1/chat/completions?api_key=s3cr3t",
            22,
            r#"{"error": {"message": "bad key s3cr3t"}}"#,
            "",
            "",
            &["s3cr3t"],
        );
        assert_eq!(
            message,
            "OpenAI-compatible request failed (curl exit 22) model='m' \
             url=http://127.0.0.1/v1/chat/completions?api_key=%2A%2A%2A: bad key ***"
        );
    }

    #[test]
    fn curl_failure_message_falls_back_through_stdout_then_stderr_then_no_body() {
        let msg = curl_failure_message("ollama", None, "http://x/y", 7, "", "", "", &[]);
        assert!(msg.ends_with(": no response body"));
        let msg = curl_failure_message("ollama", None, "http://x/y", 7, "", "boom", "", &[]);
        assert!(msg.ends_with(": boom"));
    }

    #[test]
    fn curl_failure_message_appends_a_hint_after_masking() {
        let msg = curl_failure_message(
            "ollama",
            None,
            "http://localhost:11434/api/generate",
            7,
            "",
            "",
            "Ollama at http://localhost:11434 is not reachable.",
            &[],
        );
        assert!(msg.ends_with("Ollama at http://localhost:11434 is not reachable."));
    }

    #[test]
    fn classify_curl_outcome_marks_connection_level_failures_retryable() {
        let outcome = CurlOutcome {
            returncode: 7,
            stdout: String::new(),
            stderr: "curl: (7) ...".to_string(),
            status: None,
            retry_after: None,
        };
        assert!(classify_curl_outcome(&outcome).retryable);
    }

    #[test]
    fn classify_curl_outcome_reads_status_for_exit_22() {
        let outcome = CurlOutcome {
            returncode: 22,
            stdout: String::new(),
            stderr: String::new(),
            status: Some(429),
            retry_after: Some("5".to_string()),
        };
        let info = classify_curl_outcome(&outcome);
        assert!(info.retryable);
        assert_eq!(info.status, Some(429));
        assert_eq!(info.retry_after, Some("5".to_string()));
    }

    #[test]
    fn classify_curl_outcome_is_not_retryable_for_a_4xx_status() {
        let outcome = CurlOutcome {
            returncode: 22,
            stdout: String::new(),
            stderr: String::new(),
            status: Some(401),
            retry_after: None,
        };
        assert!(!classify_curl_outcome(&outcome).retryable);
    }

    #[test]
    fn classify_curl_outcome_is_not_retryable_for_exit_18_or_60() {
        for code in [18, 60] {
            let outcome = CurlOutcome {
                returncode: code,
                stdout: String::new(),
                stderr: String::new(),
                status: None,
                retry_after: None,
            };
            assert!(!classify_curl_outcome(&outcome).retryable, "exit {code}");
        }
    }
}
