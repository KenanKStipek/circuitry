//! `adapters/openai.py::OpenAIAdapter`, ported -- its own file (not
//! folded into [`crate::openai_compat`]): `openai` is Circuitry's own
//! default/reference provider, with its own response-format handling
//! the generic OpenAI-compatible shape doesn't cover (the `seed`
//! placement, `max_completion_tokens`, and a looser `usage` coercion).

use crate::config::{cfg_str_or, cfg_str_or_rstrip_target};
use crate::curl_text::{classify_curl_outcome, curl_failure_message};
use crate::openai_compat::{chat_messages, parse_chat_response, sampling_fields};
use crate::transport::{TransportFailure, run_curl};
use async_trait::async_trait;
use electricity_json::WriteMode;
use electricity_value::{Dict, Value};
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult, PyExc,
};
use electricity_vm::retry::RetryInfo;

const DETERMINISTIC_SEED: i64 = 0;

/// `adapters/openai.py::OpenAIAdapter.generate`'s own payload assembly
/// -- `model`/`messages` first, then `sampling_fields` (with
/// `max_completion_tokens`, not `max_tokens`), then a deterministic
/// `seed` only if one wasn't already set via `params`. `pub` so
/// `tests/adapter_corpus.rs` replays `generate_adapter_corpus.py`'s
/// own request-building cases through this exact function (issue
/// #515 finding 5's first item -- `openai` had no request-building
/// case at all).
pub fn build_request(model: &str, prompt: &str, options: &GenerateOptions) -> Dict {
    let mut payload = Dict::new();
    payload.insert(
        Value::Str("model".to_string()),
        Value::Str(model.to_string()),
    );
    payload.insert(
        Value::Str("messages".to_string()),
        Value::List(chat_messages(prompt, options)),
    );
    for (key, value) in sampling_fields(options, "max_completion_tokens") {
        payload.insert(key, value);
    }
    if options.deterministic {
        payload
            .entry(Value::Str("seed".to_string()))
            .or_insert(Value::from(DETERMINISTIC_SEED));
    }
    payload
}

fn truncate_chars(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((byte_index, _)) => &text[..byte_index],
        None => text,
    }
}

fn attribute_error(value: &Value, attribute: &str) -> AdapterError {
    AdapterError {
        message: format!(
            "'{}' object has no attribute '{attribute}'",
            value.type_name()
        ),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::AttributeError,
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

/// `openai.py`'s own token coercion (`:100-113`): `int(x)` whenever
/// `x is not None` -- `"7"` gives 7, `7.9` gives 7, `true` gives 1,
/// and `"x"` raises CPython's own `ValueError` text, after a
/// successful HTTP call.
fn coerce_token_count(value: Option<&Value>) -> Result<Option<i64>, AdapterError> {
    match value {
        None | Some(Value::None) => Ok(None),
        Some(v) => electricity_value::pycompat::py_int(v)
            .map(Some)
            .map_err(|e| AdapterError {
                message: e.message,
                retry: RetryInfo::not_retryable(),
                py_class: e.py_class,
            }),
    }
}

/// The pure half of [`Adapter::generate`] -- everything after
/// [`run_curl`] returns, replayed by `generate_adapter_corpus.py`'s
/// own scripted `(returncode, stdout, stderr)` triples
/// (`tests/adapter_corpus.rs`). `pub` for the same reason.
pub fn interpret_outcome(
    outcome: &crate::transport::CurlOutcome,
    model: &str,
    url: &str,
    api_key: &str,
) -> Result<GenerateResult, AdapterError> {
    if outcome.returncode != 0 {
        let message = curl_failure_message(
            "openai",
            Some(model),
            url,
            outcome.returncode,
            &outcome.stdout,
            &outcome.stderr,
            "",
            &[api_key],
        );
        return Err(AdapterError {
            message,
            retry: classify_curl_outcome(outcome),
            py_class: PyExc::AdapterCallError,
        });
    }

    let raw = electricity_json::loads(&outcome.stdout).map_err(|_| AdapterError {
        message: format!(
            "OpenAI returned non-JSON response: {}",
            truncate_chars(&outcome.stdout, 200)
        ),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    })?;
    let (text, finish_reason) = parse_chat_response(&raw)?;

    let usage = raw
        .as_dict()
        .and_then(|d| d.get(&Value::Str("usage".to_string())));
    let usage_dict = match usage {
        None => None,
        Some(Value::Dict(d)) => Some(d),
        Some(other) => return Err(attribute_error(other, "get")),
    };
    let tokens_sent = coerce_token_count(
        usage_dict.and_then(|d| d.get(&Value::Str("prompt_tokens".to_string()))),
    )?;
    let tokens_received = coerce_token_count(
        usage_dict.and_then(|d| d.get(&Value::Str("completion_tokens".to_string()))),
    )?;

    Ok(GenerateResult {
        text,
        raw,
        tokens_sent,
        tokens_received,
        finish_reason,
        warnings: Vec::new(),
        cost_usd: None,
    })
}

/// `adapters/openai.py::OpenAIAdapter`.
pub struct OpenAiAdapter {
    pub base_url: String,
    pub default_model: String,
}

#[async_trait(?Send)]
impl Adapter for OpenAiAdapter {
    fn name(&self) -> &str {
        "openai"
    }

    async fn generate(
        &self,
        model: &str,
        prompt: &str,
        options: &GenerateOptions,
        call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        let model = if model.is_empty() {
            self.default_model.as_str()
        } else {
            model
        };
        let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
        if api_key.is_empty() {
            return Err(AdapterError {
                message: "OpenAI API key not found. Set OPENAI_API_KEY in the environment \
                          (recommended: ~/.config/circuitry/.env). Run `cof doctor` to verify."
                    .to_string(),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::RuntimeError,
            });
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let payload = build_request(model, prompt, options);
        let body =
            // `json.dumps(payload)` raises Python's own `TypeError` for a
            // value it can't serialize, uncaught -- never `RuntimeError`.
            electricity_json::dumps(&Value::Dict(payload), WriteMode::COMPACT).map_err(|e| {
                AdapterError {
                    message: e.to_string(),
                    retry: RetryInfo::not_retryable(),
                    py_class: PyExc::TypeError,
                }
            })?;

        let headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Authorization".to_string(), format!("Bearer {api_key}")),
        ];
        let outcome = run_curl(&url, &headers, Some(&body), call.timeout_seconds)
            .await
            .map_err(transport_failure_error)?;
        interpret_outcome(&outcome, model, &url, &api_key)
    }

    async fn check(&self) -> CheckResult {
        let mut missing = Vec::new();
        if std::env::var("OPENAI_API_KEY")
            .unwrap_or_default()
            .is_empty()
        {
            missing.push("env:OPENAI_API_KEY".to_string());
        }
        CheckResult {
            ok: missing.is_empty(),
            missing,
            message: None,
        }
    }

    fn accepts_images(&self) -> bool {
        true
    }
}

/// `adapters/factory.py::_build_openai`.
pub fn build_openai(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(Box::new(OpenAiAdapter {
        base_url: cfg_str_or_rstrip_target(cfg, "base_url", "https://api.openai.com/v1")?,
        default_model: cfg_str_or(cfg, "default_model", "gpt-4o-mini"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coerce_token_count_parses_a_numeric_string() {
        assert_eq!(
            coerce_token_count(Some(&Value::Str("7".to_string()))).unwrap(),
            Some(7)
        );
    }

    #[test]
    fn coerce_token_count_truncates_a_float() {
        assert_eq!(
            coerce_token_count(Some(&Value::Float(7.9))).unwrap(),
            Some(7)
        );
    }

    #[test]
    fn coerce_token_count_reads_a_bool_as_zero_or_one() {
        assert_eq!(
            coerce_token_count(Some(&Value::Bool(true))).unwrap(),
            Some(1)
        );
    }

    #[test]
    fn coerce_token_count_raises_cpythons_text_for_a_bad_string() {
        let err = coerce_token_count(Some(&Value::Str("x".to_string()))).unwrap_err();
        assert_eq!(err.message, "invalid literal for int() with base 10: 'x'");
        assert_eq!(err.py_class, PyExc::ValueError);
    }

    #[test]
    fn coerce_token_count_is_none_only_for_none_itself() {
        assert_eq!(coerce_token_count(None).unwrap(), None);
        assert_eq!(coerce_token_count(Some(&Value::None)).unwrap(), None);
    }

    #[test]
    fn build_openai_defaults_base_url_and_model() {
        let adapter = build_openai(&Value::None).unwrap();
        assert_eq!(adapter.name(), "openai");
    }

    #[test]
    fn build_openai_fails_like_python_for_a_non_string_base_url() {
        let mut cfg = Dict::new();
        cfg.insert(Value::Str("base_url".to_string()), Value::from(5i64));
        let err = match build_openai(&Value::Dict(cfg)) {
            Err(err) => err,
            Ok(_) => panic!("expected build_openai to fail"),
        };
        assert_eq!(err.message, "'int' object has no attribute 'rstrip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }
}
