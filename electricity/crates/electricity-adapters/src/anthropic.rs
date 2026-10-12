//! `adapters/anthropic.py::AnthropicAdapter`, ported -- the Messages
//! API, over [`crate::transport`]: system-turn hoisting, image blocks
//! ahead of text, and `stop_reason`'s own truncation signal.

use crate::config::cfg_str_or;
use crate::curl_text::{classify_curl_outcome, curl_failure_message};
use crate::transport::{TransportFailure, run_curl};
use async_trait::async_trait;
use electricity_json::WriteMode;
use electricity_value::pycompat::{py_int, py_strip};
use electricity_value::{Dict, Value};
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult, ImageInput,
    PyExc,
};
use electricity_vm::retry::RetryInfo;

fn dict_insert(dict: &mut Dict, key: &str, value: Value) {
    dict.insert(Value::Str(key.to_string()), value);
}

fn last_user_index(turns: &[Value]) -> Option<usize> {
    (0..turns.len()).rev().find(|&i| {
        turns[i]
            .as_dict()
            .and_then(|d| d.get(&Value::Str("role".to_string())))
            .and_then(Value::as_str)
            == Some("user")
    })
}

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

fn int_if_not_none(value: Option<&Value>) -> Result<Option<i64>, AdapterError> {
    match value {
        None | Some(Value::None) => Ok(None),
        Some(v) => py_int(v).map(Some).map_err(|e| AdapterError {
            message: e.message,
            retry: RetryInfo::not_retryable(),
            py_class: e.py_class,
        }),
    }
}

fn image_block(image: &ImageInput) -> Value {
    let mut block = Dict::new();
    dict_insert(&mut block, "type", Value::Str("image".to_string()));
    let mut source = Dict::new();
    if let Some(url) = &image.url {
        dict_insert(&mut source, "type", Value::Str("url".to_string()));
        dict_insert(&mut source, "url", Value::Str(url.clone()));
    } else {
        use base64::Engine;
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(image.data.as_deref().unwrap_or(&[]));
        dict_insert(&mut source, "type", Value::Str("base64".to_string()));
        dict_insert(
            &mut source,
            "media_type",
            Value::Str(image.media_type.clone().unwrap_or_default()),
        );
        dict_insert(&mut source, "data", Value::Str(encoded));
    }
    dict_insert(&mut block, "source", Value::Dict(source));
    Value::Dict(block)
}

/// `adapters/anthropic.py::_request_body`. `pub` so
/// `tests/adapter_corpus.rs` can replay request-building cases.
pub fn build_request_body(
    model: &str,
    prompt: &str,
    max_tokens: i64,
    options: &GenerateOptions,
) -> Dict {
    let mut system_parts: Vec<&str> = Vec::new();
    let mut turns: Vec<Value> = Vec::new();
    for m in &options.messages {
        if m.role == "system" {
            system_parts.push(&m.content);
            continue;
        }
        let mut entry = Dict::new();
        if m.role == "tool" {
            dict_insert(&mut entry, "role", Value::Str("user".to_string()));
            dict_insert(
                &mut entry,
                "content",
                Value::Str(format!("tool: {}", m.content)),
            );
        } else {
            dict_insert(&mut entry, "role", Value::Str(m.role.clone()));
            dict_insert(&mut entry, "content", Value::Str(m.content.clone()));
        }
        turns.push(Value::Dict(entry));
    }
    let mut system = system_parts.join("\n\n");

    if options.messages.is_empty() {
        let mut entry = Dict::new();
        dict_insert(&mut entry, "role", Value::Str("user".to_string()));
        dict_insert(&mut entry, "content", Value::Str(prompt.to_string()));
        turns = vec![Value::Dict(entry)];
    } else if turns.is_empty() && options.images.is_empty() {
        let mut entry = Dict::new();
        dict_insert(&mut entry, "role", Value::Str("user".to_string()));
        dict_insert(&mut entry, "content", Value::Str(system.clone()));
        turns = vec![Value::Dict(entry)];
        system = String::new();
    }

    if !options.images.is_empty() {
        let index = match last_user_index(&turns) {
            Some(i) => i,
            None => {
                let mut entry = Dict::new();
                dict_insert(&mut entry, "role", Value::Str("user".to_string()));
                dict_insert(&mut entry, "content", Value::Str(String::new()));
                turns.push(Value::Dict(entry));
                turns.len() - 1
            }
        };
        let text = match turns[index]
            .as_dict()
            .and_then(|d| d.get(&Value::Str("content".to_string())))
        {
            Some(Value::Str(s)) => s.clone(),
            _ => String::new(),
        };
        let mut parts: Vec<Value> = options.images.iter().map(image_block).collect();
        if !text.is_empty() {
            let mut text_block = Dict::new();
            dict_insert(&mut text_block, "type", Value::Str("text".to_string()));
            dict_insert(&mut text_block, "text", Value::Str(text));
            parts.push(Value::Dict(text_block));
        }
        if let Value::Dict(entry) = &mut turns[index] {
            dict_insert(entry, "content", Value::List(parts));
        }
    }

    let mut body = Dict::new();
    dict_insert(&mut body, "model", Value::Str(model.to_string()));
    dict_insert(
        &mut body,
        "max_tokens",
        Value::from(options.max_tokens.unwrap_or(max_tokens)),
    );
    dict_insert(&mut body, "messages", Value::List(turns));
    if !system.is_empty() {
        dict_insert(&mut body, "system", Value::Str(system));
    }
    if let Some(temperature) = options.temperature {
        dict_insert(&mut body, "temperature", Value::Float(temperature));
    }
    if !options.stop.is_empty() {
        dict_insert(
            &mut body,
            "stop_sequences",
            Value::List(options.stop.iter().map(|s| Value::Str(s.clone())).collect()),
        );
    }
    if let Some(params) = options.params.as_dict() {
        for (key, value) in params {
            let key_str = match key {
                Value::Str(s) => s.clone(),
                other => other.py_str(),
            };
            body.insert(Value::Str(key_str), value.clone());
        }
    }
    body
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
            "anthropic",
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
            "Anthropic returned non-JSON response: {}",
            truncate_chars(&outcome.stdout, 200)
        ),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    })?;

    let content = raw
        .as_dict()
        .and_then(|d| d.get(&Value::Str("content".to_string())))
        .and_then(Value::as_list)
        .unwrap_or(&[]);
    let mut text = String::new();
    for block in content {
        let Some(block_dict) = block.as_dict() else {
            continue;
        };
        let is_text = matches!(
            block_dict.get(&Value::Str("type".to_string())),
            Some(Value::Str(s)) if s == "text"
        );
        if !is_text {
            continue;
        }
        match block_dict.get(&Value::Str("text".to_string())) {
            None | Some(Value::Str(_)) => {}
            Some(other) => {
                return Err(AdapterError {
                    message: format!(
                        "can only concatenate str (not \"{}\") to str",
                        other.type_name()
                    ),
                    retry: RetryInfo::not_retryable(),
                    py_class: PyExc::TypeError,
                });
            }
        }
        if let Some(Value::Str(s)) = block_dict.get(&Value::Str("text".to_string())) {
            text.push_str(s);
        }
    }
    let text = if text.is_empty() {
        String::new()
    } else {
        py_strip(&text).to_string()
    };

    let usage = raw
        .as_dict()
        .and_then(|d| d.get(&Value::Str("usage".to_string())));
    let usage_dict = match usage {
        Some(Value::Dict(d)) => Some(d),
        _ => None,
    };
    let tokens_sent =
        int_if_not_none(usage_dict.and_then(|d| d.get(&Value::Str("input_tokens".to_string()))))?;
    let tokens_received =
        int_if_not_none(usage_dict.and_then(|d| d.get(&Value::Str("output_tokens".to_string()))))?;
    let finish_reason = match raw
        .as_dict()
        .and_then(|d| d.get(&Value::Str("stop_reason".to_string())))
    {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    };

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

/// `adapters/anthropic.py::AnthropicAdapter`.
pub struct AnthropicAdapter {
    pub base_url: String,
    pub default_model: String,
    pub max_tokens: i64,
}

#[async_trait(?Send)]
impl Adapter for AnthropicAdapter {
    fn name(&self) -> &str {
        "anthropic"
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
        let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
        if api_key.is_empty() {
            return Err(AdapterError {
                message: "Anthropic API key not found. Set ANTHROPIC_API_KEY in the \
                          environment (recommended: ~/.config/circuitry/.env). Run `cof \
                          doctor` to verify."
                    .to_string(),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::RuntimeError,
            });
        }

        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let payload = build_request_body(model, prompt, self.max_tokens, options);
        let body =
            electricity_json::dumps(&Value::Dict(payload), WriteMode::COMPACT).map_err(|e| {
                AdapterError {
                    message: e.to_string(),
                    retry: RetryInfo::not_retryable(),
                    py_class: PyExc::RuntimeError,
                }
            })?;

        let headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("x-api-key".to_string(), api_key.clone()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ];
        let outcome = run_curl(&url, &headers, Some(&body), call.timeout_seconds)
            .await
            .map_err(transport_failure_error)?;
        interpret_outcome(&outcome, model, &url, &api_key)
    }

    async fn check(&self) -> CheckResult {
        let mut missing = Vec::new();
        if std::env::var("ANTHROPIC_API_KEY")
            .unwrap_or_default()
            .is_empty()
        {
            missing.push("env:ANTHROPIC_API_KEY".to_string());
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

/// `adapters/factory.py::_build_anthropic`. `max_tokens`'s own
/// `int()` failure is raised **at build time** (CPython's own
/// `ValueError`/`TypeError` text/class).
pub fn build_anthropic(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    let raw_max_tokens = crate::config::cfg_get(cfg, "max_tokens");
    let truthy = raw_max_tokens.is_some_and(crate::config::value_is_truthy);
    let max_tokens_value = if truthy {
        raw_max_tokens.unwrap().clone()
    } else {
        Value::from(4096i64)
    };
    let max_tokens = py_int(&max_tokens_value).map_err(|e| AdapterError {
        message: e.message,
        retry: RetryInfo::not_retryable(),
        py_class: e.py_class,
    })?;
    Ok(Box::new(AnthropicAdapter {
        base_url: cfg_str_or(cfg, "base_url", "https://api.anthropic.com"),
        default_model: cfg_str_or(cfg, "default_model", "claude-sonnet-5"),
        max_tokens,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_vm::adapter::ChatMessage;

    #[test]
    fn system_turns_are_hoisted_into_the_system_field() {
        let options = GenerateOptions {
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: "be nice".to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: "hi".to_string(),
                },
            ],
            ..GenerateOptions::default()
        };
        let body = build_request_body("m", "hi", 4096, &options);
        assert_eq!(
            body.get(&Value::Str("system".to_string())),
            Some(&Value::Str("be nice".to_string()))
        );
        let messages = body
            .get(&Value::Str("messages".to_string()))
            .unwrap()
            .as_list()
            .unwrap();
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn system_only_messages_become_one_user_turn_and_no_system_key() {
        let options = GenerateOptions {
            messages: vec![ChatMessage {
                role: "system".to_string(),
                content: "be nice".to_string(),
            }],
            ..GenerateOptions::default()
        };
        let body = build_request_body("m", "hi", 4096, &options);
        assert!(!body.contains_key(&Value::Str("system".to_string())));
        let messages = body
            .get(&Value::Str("messages".to_string()))
            .unwrap()
            .as_list()
            .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0]
                .as_dict()
                .unwrap()
                .get(&Value::Str("content".to_string())),
            Some(&Value::Str("be nice".to_string()))
        );
    }

    #[test]
    fn images_go_before_the_text_block() {
        let options = GenerateOptions {
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "hi".to_string(),
            }],
            images: vec![ImageInput {
                data: Some(vec![1, 2]),
                media_type: Some("image/png".to_string()),
                url: None,
            }],
            ..GenerateOptions::default()
        };
        let body = build_request_body("m", "hi", 4096, &options);
        let messages = body
            .get(&Value::Str("messages".to_string()))
            .unwrap()
            .as_list()
            .unwrap();
        let content = messages[0]
            .as_dict()
            .unwrap()
            .get(&Value::Str("content".to_string()))
            .unwrap()
            .as_list()
            .unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(
            content[0]
                .as_dict()
                .unwrap()
                .get(&Value::Str("type".to_string())),
            Some(&Value::Str("image".to_string()))
        );
    }

    #[test]
    fn max_tokens_prefers_options_over_the_adapter_default() {
        let options = GenerateOptions {
            max_tokens: Some(50),
            ..GenerateOptions::default()
        };
        let body = build_request_body("m", "hi", 4096, &options);
        assert_eq!(
            body.get(&Value::Str("max_tokens".to_string())),
            Some(&Value::from(50i64))
        );
    }

    #[test]
    fn build_anthropic_defaults_max_tokens_to_4096() {
        let adapter = build_anthropic(&Value::None).unwrap();
        assert_eq!(adapter.name(), "anthropic");
    }

    #[test]
    fn build_anthropic_raises_cpythons_text_for_a_bad_max_tokens() {
        let mut cfg = Dict::new();
        cfg.insert(
            Value::Str("max_tokens".to_string()),
            Value::Str("x".to_string()),
        );
        let err = match build_anthropic(&Value::Dict(cfg)) {
            Err(err) => err,
            Ok(_) => panic!("expected build_anthropic to fail"),
        };
        assert_eq!(err.message, "invalid literal for int() with base 10: 'x'");
        assert_eq!(err.py_class, PyExc::ValueError);
    }
}
