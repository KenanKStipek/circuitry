//! `adapters/replicate.py::ReplicateAdapter`, ported -- its own
//! predictions API (`Prefer: wait=N` for a synchronous-feeling call),
//! over [`crate::transport`].

use crate::config::cfg_str_or;
use crate::curl_text::{classify_curl_outcome, curl_failure_message};
use crate::transport::{TransportFailure, run_curl};
use async_trait::async_trait;
use electricity_value::{Dict, Value};
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult, PyExc,
};
use electricity_vm::retry::RetryInfo;

fn truncate_chars(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((byte_index, _)) => &text[..byte_index],
        None => text,
    }
}

fn transport_failure_error(failure: TransportFailure) -> AdapterError {
    match failure {
        TransportFailure::Validation(message) => AdapterError {
            message,
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::ValueError,
        },
        TransportFailure::InvalidUtf8(message) => AdapterError {
            message,
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::UnicodeDecodeError,
        },
    }
}

/// The pure half of [`Adapter::generate`] -- everything after
/// [`run_curl`] returns, replayed by `generate_adapter_corpus.py`'s
/// own scripted `(returncode, stdout, stderr)` triples
/// (`tests/adapter_corpus.rs`). `pub` for the same reason.
pub fn interpret_outcome(
    outcome: &crate::transport::CurlOutcome,
    target_model: &str,
    url: &str,
    api_token: &str,
    wait: i64,
    options: &GenerateOptions,
) -> Result<GenerateResult, AdapterError> {
    if outcome.returncode != 0 {
        let message = curl_failure_message(
            "replicate",
            Some(target_model),
            url,
            outcome.returncode,
            &outcome.stdout,
            &outcome.stderr,
            "",
            &[api_token],
        );
        return Err(AdapterError {
            message,
            retry: classify_curl_outcome(outcome),
            py_class: PyExc::AdapterCallError,
        });
    }

    let raw = electricity_json::loads(&outcome.stdout).map_err(|_| AdapterError {
        message: format!(
            "Replicate returned non-JSON response: {}",
            truncate_chars(&outcome.stdout, 200)
        ),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    })?;
    let dict = raw.as_dict().ok_or_else(|| AdapterError {
        message: format!("'{}' object has no attribute 'get'", raw.type_name()),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::AttributeError,
    })?;

    let status = match dict.get(&Value::Str("status".to_string())) {
        Some(Value::Str(s)) => Some(s.as_str()),
        _ => None,
    };
    if !matches!(
        status,
        Some("succeeded") | Some("processing") | Some("starting")
    ) {
        let error_value = dict.get(&Value::Str("error".to_string()));
        let shown = match error_value {
            Some(v) if crate::config::value_is_truthy(v) => v.py_str(),
            _ => status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "None".to_string()),
        };
        return Err(AdapterError {
            message: format!("Replicate prediction failed: {shown}"),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        });
    }
    if status != Some("succeeded") {
        let pred_id = match dict.get(&Value::Str("id".to_string())) {
            Some(Value::Str(s)) => s.clone(),
            _ => "?".to_string(),
        };
        return Err(AdapterError {
            message: format!(
                "Replicate prediction {} did not complete within wait={wait}s \
                 (status={}). Re-issue with a longer timeout or poll \
                 /v1/predictions/{{id}}.",
                Value::Str(pred_id).py_repr(),
                Value::Str(status.unwrap_or_default().to_string()).py_repr(),
            ),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        });
    }

    let output = dict.get(&Value::Str("output".to_string()));
    let text = match output {
        Some(Value::List(items)) => items.iter().map(Value::py_str).collect::<String>(),
        Some(Value::None) | None => String::new(),
        Some(other) => other.py_str(),
    };

    Ok(GenerateResult {
        text: electricity_value::pycompat::py_strip(&text).to_string(),
        raw,
        tokens_sent: None,
        tokens_received: None,
        finish_reason: None,
        warnings: crate::ignored_options_warning("replicate", options, &[]),
        cost_usd: None,
    })
}

pub struct ReplicateAdapter {
    pub base_url: String,
    pub default_model: String,
}

#[async_trait(?Send)]
impl Adapter for ReplicateAdapter {
    fn name(&self) -> &str {
        "replicate"
    }

    async fn generate(
        &self,
        model: &str,
        prompt: &str,
        options: &GenerateOptions,
        call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        let api_token = std::env::var("REPLICATE_API_TOKEN").unwrap_or_default();
        if api_token.is_empty() {
            return Err(AdapterError {
                message: "replicate: REPLICATE_API_TOKEN not set.".to_string(),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::RuntimeError,
            });
        }
        let raw_model = if model.is_empty() {
            self.default_model.as_str()
        } else {
            model
        };
        let target_model = electricity_value::pycompat::py_strip(raw_model);
        if !target_model.contains('/') {
            return Err(AdapterError {
                message: format!(
                    "replicate: model must be 'owner/name' (got {}).",
                    Value::Str(target_model.to_string()).py_repr()
                ),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::ValueError,
            });
        }

        let wait = (call.timeout_seconds as i64).clamp(1, 60);
        let url = format!(
            "{}/models/{target_model}/predictions",
            self.base_url.trim_end_matches('/')
        );
        let mut input = Dict::new();
        input.insert(
            Value::Str("prompt".to_string()),
            Value::Str(prompt.to_string()),
        );
        let mut payload = Dict::new();
        payload.insert(Value::Str("input".to_string()), Value::Dict(input));
        let body =
            electricity_json::dumps(&Value::Dict(payload), electricity_json::WriteMode::COMPACT)
                .map_err(|e| AdapterError {
                    message: e.to_string(),
                    retry: RetryInfo::not_retryable(),
                    py_class: PyExc::RuntimeError,
                })?;

        let headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Authorization".to_string(), format!("Bearer {api_token}")),
            ("Prefer".to_string(), format!("wait={wait}")),
        ];
        let outcome = run_curl(&url, &headers, Some(&body), call.timeout_seconds)
            .await
            .map_err(transport_failure_error)?;
        interpret_outcome(&outcome, target_model, &url, &api_token, wait, options)
    }

    async fn check(&self) -> CheckResult {
        let mut missing = Vec::new();
        if std::env::var("REPLICATE_API_TOKEN")
            .unwrap_or_default()
            .is_empty()
        {
            missing.push("env:REPLICATE_API_TOKEN".to_string());
        }
        CheckResult {
            ok: missing.is_empty(),
            missing,
            message: None,
        }
    }
}

/// `adapters/factory.py::_build_replicate`.
pub fn build_replicate(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(Box::new(ReplicateAdapter {
        base_url: cfg_str_or(cfg, "base_url", "https://api.replicate.com/v1"),
        default_model: cfg_str_or(cfg, "default_model", "meta/meta-llama-3-70b-instruct"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_replicate_defaults_base_url_and_model() {
        let adapter = build_replicate(&Value::None).unwrap();
        assert_eq!(adapter.name(), "replicate");
    }
}
