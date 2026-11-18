//! `curl_support.py`'s own failure-message/masking helpers, ported --
//! `curl_failure_message`, `parse_error_body`, and the curl exit-code-
//! to-[`RetryInfo`] mapping. Every adapter in this crate that calls
//! [`crate::transport::run_curl`] builds its own failure message
//! through [`curl_failure_message`], never directly: this is the one
//! place a secret gets masked out of a message before it is ever
//! returned (issue #503's own "Security" section -- no request/
//! response body, header value or key is ever logged, and every
//! failure text passes through this module's own masking before
//! `exec/prompt.rs`'s `redact()` ever sees it). A request URL is named
//! only through [`electricity_tools::http_conventions::redact_request_
//! url`] (issue #778) -- never the weaker userinfo/query maskers that
//! function itself is built from, which stay private to
//! `electricity-tools::http_conventions`.

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

/// `curl_support.py::parse_error_code` -- the provider's own error code
/// from a JSON error body: `error.code` when that is a non-empty
/// string, else `error.type` when that is a non-empty string; `None`
/// otherwise, including an integer code (it duplicates the HTTP
/// status) or a flat `{"error": "<text>"}` body.
pub fn parse_error_code(stdout: &str) -> Option<String> {
    let text = electricity_value::pycompat::py_strip(stdout);
    if text.is_empty() {
        return None;
    }
    let parsed = electricity_json::loads(text).ok()?;
    let error_dict = parsed
        .as_dict()?
        .get(&Value::Str("error".to_string()))?
        .as_dict()?;
    for field in ["code", "type"] {
        if let Some(Value::Str(value)) = error_dict.get(&Value::Str(field.to_string())) {
            if !value.is_empty() {
                return Some(value.clone());
            }
        }
    }
    None
}

/// `curl_support.py::curl_error_code` -- [`parse_error_code`] of
/// *stdout*, with *secrets* masked first.
pub fn curl_error_code(stdout: &str, secrets: &[&str]) -> Option<String> {
    let secrets: Vec<&str> = secrets.iter().copied().filter(|s| !s.is_empty()).collect();
    parse_error_code(&mask_secrets(stdout, &secrets))
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

/// `curl_support.py::redact_host_url` -- masks a URL for a preflight
/// `host:{url}` requirement text: userinfo stripped, then any
/// credential-like query parameter masked, the same two masks
/// [`curl_failure_message`] already applies to a request URL. Shared
/// by every `check()` that reports `host:{base_url}` -- `ollama`'s own
/// (`ollama.rs`) and `cyberdiner`'s. Uses
/// `http_conventions::redact_request_url`, not `redact_url` directly:
/// a password hidden behind a `/`, `?` or `#` in `url` (issue #778)
/// must not reach this preflight text either.
pub fn redact_host_url(url: &str) -> String {
    electricity_tools::http_conventions::redact_request_url(url)
}

/// `curl_support.py::curl_failure_message` -- renders a curl failure
/// without ever including the request body/headers. *secrets* are the
/// raw credential values this request sent; masked out of *stdout*/
/// *stderr* before the body is parsed, and out of the final message
/// again afterward, so a secret split by truncation can't survive as a
/// partial match. *url* is named through [`electricity_tools::
/// http_conventions::redact_request_url`] -- the one function that
/// redacts a request URL for error text on either engine -- so a
/// password hidden anywhere in it, including behind a `/`, `?` or `#`,
/// never reaches the message. *hint* is never redacted here: a caller
/// that builds one naming a URL (`ollama.rs::ollama_hint`) must redact
/// it itself before passing it in, through the same function.
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
    // A response body a provider echoes back may itself contain a URL
    // with userinfo (issue #819) -- scrubbed before it's ever shown,
    // the same as every other surface that names a request URL.
    let detail = electricity_tools::http_conventions::scrub_embedded_urls(&detail);
    let model_part = match model {
        Some(m) if !m.is_empty() => format!(" model={}", Value::Str(m.to_string()).py_repr()),
        _ => String::new(),
    };
    let mut message = format!(
        "{source} request failed (curl exit {returncode}){model_part} url={}: {detail}",
        electricity_tools::http_conventions::redact_request_url(url)
    );
    if !hint.is_empty() {
        message.push(' ');
        message.push_str(hint);
    }
    mask_secrets(&message, &secrets)
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
            error_code: None,
        };
    }
    if outcome.returncode == 22 {
        if let Some(status) = outcome.status {
            return RetryInfo {
                retryable: status_is_retryable(status),
                status: Some(status),
                retry_after: outcome.retry_after.clone(),
                error_code: None,
            };
        }
    }
    RetryInfo {
        retryable: false,
        status: None,
        retry_after: outcome.retry_after.clone(),
        error_code: None,
    }
}

/// [`classify_curl_outcome`] with [`curl_error_code`]'s own
/// *secrets*-masked `error.code`/`error.type` read filled into the
/// result -- every real adapter's own failure branch calls this
/// instead of the two separately, the same way `adapters/_retry.py`'s
/// `classify_curl_exit` call always sits next to a `curl_error_code`
/// call at the Python raise site.
pub fn classify_curl_outcome_with_error_code(outcome: &CurlOutcome, secrets: &[&str]) -> RetryInfo {
    let mut retry = classify_curl_outcome(outcome);
    retry.error_code = curl_error_code(&outcome.stdout, secrets);
    retry
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
    fn parse_error_code_reads_error_code() {
        let body = r#"{"error": {"code": "guardrail_violation", "message": "nope"}}"#;
        assert_eq!(
            parse_error_code(body),
            Some("guardrail_violation".to_string())
        );
    }

    #[test]
    fn parse_error_code_falls_back_to_error_type_when_code_is_null() {
        let body = r#"{"error": {"code": null, "type": "overloaded_error"}}"#;
        assert_eq!(parse_error_code(body), Some("overloaded_error".to_string()));
    }

    #[test]
    fn parse_error_code_ignores_an_integer_code() {
        let body = r#"{"error": {"code": 5}}"#;
        assert_eq!(parse_error_code(body), None);
    }

    #[test]
    fn parse_error_code_is_none_for_a_flat_string_body() {
        let body = r#"{"error": "model not found"}"#;
        assert_eq!(parse_error_code(body), None);
    }

    #[test]
    fn parse_error_code_is_none_for_non_json() {
        assert_eq!(parse_error_code("not json at all"), None);
    }

    #[test]
    fn parse_error_code_is_none_for_an_empty_body() {
        assert_eq!(parse_error_code(""), None);
    }

    #[test]
    fn curl_error_code_masks_secrets_before_parsing() {
        let secret = "sk-canary-secret-123";
        let body = format!(r#"{{"error": {{"code": "{secret}"}}}}"#);
        assert_eq!(curl_error_code(&body, &[secret]), Some("***".to_string()));
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
    fn redact_host_url_strips_userinfo_for_a_preflight_host_text() {
        assert_eq!(
            redact_host_url("http://user:canarypw@example.test:11434"),
            "http://example.test:11434"
        );
    }

    #[test]
    fn redact_host_url_masks_a_credential_query_param() {
        assert_eq!(
            redact_host_url("https://example.test/rpc?api_key=canarypw"),
            "https://example.test/rpc?api_key=%2A%2A%2A"
        );
    }

    #[test]
    fn redact_host_url_leaves_a_plain_url_untouched() {
        assert_eq!(
            redact_host_url("http://localhost:11434"),
            "http://localhost:11434"
        );
    }

    #[test]
    fn redact_host_url_masks_a_password_hidden_behind_a_slash_question_mark_or_hash() {
        // `urlsplit`'s own netloc for these ends at the first `/`,
        // `?` or `#`, so it never sees the `@` that follows -- the
        // password must still come out fully removed (issue #778).
        for sep in ['/', '?', '#'] {
            let url = format!("http://user:pa{sep}ss@example.test:11434");
            let redacted = redact_host_url(&url);
            assert_eq!(redacted, "http://example.test:11434");
            assert!(!redacted.contains("pa"));
            assert!(!redacted.contains("ss"));
        }
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
    fn curl_failure_message_scrubs_a_url_with_userinfo_in_the_response_body() {
        // #819 item 8: a response body a provider echoes back may itself
        // contain a URL with userinfo -- scrubbed the same as every other
        // surface that names a request URL.
        let password = "pw_value_pw_value";
        let body = format!(
            r#"{{"error": "could not verify callback at https://alice:{password}@callback.example.test/hook"}}"#
        );
        let message = curl_failure_message(
            "webhook",
            None,
            "https://api.example.test/v1/send",
            22,
            &body,
            "",
            "",
            &[],
        );
        assert!(!message.contains(password));
        assert!(!message.contains("alice"));
        assert!(message.contains("https://callback.example.test/hook"));
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
    fn classify_curl_outcome_is_not_retryable_for_exit_60() {
        let outcome = CurlOutcome {
            returncode: 60,
            stdout: String::new(),
            stderr: String::new(),
            status: None,
            retry_after: None,
        };
        assert!(!classify_curl_outcome(&outcome).retryable);
    }

    #[test]
    fn classify_curl_outcome_is_retryable_for_exit_18() {
        // #674: a premature EOF partway through the body is retryable in
        // both engines -- `transport::is_retryable_exit`.
        let outcome = CurlOutcome {
            returncode: 18,
            stdout: String::new(),
            stderr: String::new(),
            status: None,
            retry_after: None,
        };
        assert!(classify_curl_outcome(&outcome).retryable);
    }
}
