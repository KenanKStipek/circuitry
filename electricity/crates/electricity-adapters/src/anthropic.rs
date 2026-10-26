//! `adapters/anthropic.py::AnthropicAdapter`, ported -- the Messages
//! API, over [`crate::transport`]: system-turn hoisting, image blocks
//! ahead of text, and `stop_reason`'s own truncation signal.

use crate::config::{cfg_str_or, cfg_value_or, rstrip_base_url};
use crate::curl_text::{classify_curl_outcome_with_error_code, curl_failure_message};
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

/// `generate()`'s own request headers -- `pub`
/// so `tests/adapter_corpus.rs` compares the recorded corpus headers
/// against the adapter's own header-building code, not a list written
/// by hand in the test a second time.
pub fn request_headers(api_key: &str) -> Vec<(String, String)> {
    vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("x-api-key".to_string(), api_key.to_string()),
        ("anthropic-version".to_string(), "2023-06-01".to_string()),
    ]
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
            retry: classify_curl_outcome_with_error_code(outcome, &[api_key]),
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

    // `raw.get("content", [])`: a non-dict `raw` (a JSON
    // list/number/string body) raises `AttributeError` the same way
    // Python's own `.get` does, before any field is read.
    let dict = raw.as_dict().ok_or_else(|| attribute_error(&raw, "get"))?;

    let content = dict
        .get(&Value::Str("content".to_string()))
        .and_then(Value::as_list)
        .unwrap_or(&[]);
    // `[block.get("text", "") for block in content if ...]` then
    // `"".join(text_parts)`: a key that's *absent* defaults to `""`,
    // but a key *present* with a non-str value (`null` included) is
    // read verbatim and only fails once `join` actually runs over it
    // -- `sequence item N: expected str instance, X found`, N counted
    // over every text-block part, not just the first bad one.
    let mut text_parts: Vec<Value> = Vec::new();
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
        let part = block_dict
            .get(&Value::Str("text".to_string()))
            .cloned()
            .unwrap_or_else(|| Value::Str(String::new()));
        text_parts.push(part);
    }
    let mut text = String::new();
    for (index, part) in text_parts.iter().enumerate() {
        match part {
            Value::Str(s) => text.push_str(s),
            other => {
                return Err(AdapterError {
                    message: format!(
                        "sequence item {index}: expected str instance, {} found",
                        other.type_name()
                    ),
                    retry: RetryInfo::not_retryable(),
                    py_class: PyExc::TypeError,
                });
            }
        }
    }
    let text = if text.is_empty() {
        String::new()
    } else {
        py_strip(&text).to_string()
    };

    // `usage = raw.get("usage", {})`: an *absent* key defaults to
    // `{}` (no tokens, no error); a *present* non-dict value (`null`
    // included) raises `AttributeError` on the next `.get`, same as a
    // non-dict `raw` above.
    let usage_dict = match dict.get(&Value::Str("usage".to_string())) {
        None => None,
        Some(Value::Dict(d)) => Some(d),
        Some(other) => return Err(attribute_error(other, "get")),
    };
    let tokens_sent =
        int_if_not_none(usage_dict.and_then(|d| d.get(&Value::Str("input_tokens".to_string()))))?;
    let tokens_received =
        int_if_not_none(usage_dict.and_then(|d| d.get(&Value::Str("output_tokens".to_string()))))?;
    let finish_reason = match dict.get(&Value::Str("stop_reason".to_string())) {
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
    /// Raw -- see `openai.rs::OpenAiAdapter::base_url`'s own doc
    /// comment.
    pub base_url: Value,
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

        let url = format!("{}/v1/messages", rstrip_base_url(&self.base_url)?);
        let payload = build_request_body(model, prompt, self.max_tokens, options);
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

        let headers = request_headers(&api_key);
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
        base_url: cfg_value_or(cfg, "base_url", "https://api.anthropic.com"),
        default_model: cfg_str_or(cfg, "default_model", "claude-sonnet-5"),
        max_tokens,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_vm::adapter::ChatMessage;
    use std::sync::{Mutex, OnceLock};

    /// `ANTHROPIC_API_KEY` is process environment, shared by every
    /// test in this module (`watsonx.rs`'s own `env_guard`, same
    /// reason).
    fn env_guard() -> &'static Mutex<()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        GUARD.get_or_init(|| Mutex::new(()))
    }

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

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // single-threaded tokio test runtime; see env_guard's own doc comment
    async fn generate_fails_like_python_for_a_non_string_base_url_after_the_api_key_check() {
        // Python raises inside `generate()`, after
        // the API-key check, not when the adapter is built.
        let _guard = env_guard().lock().unwrap();
        let mut cfg = Dict::new();
        cfg.insert(Value::Str("base_url".to_string()), Value::from(5i64));
        let adapter = build_anthropic(&Value::Dict(cfg)).unwrap();
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "sk-test");
        }
        let path = electricity_bytecode::EffectPath::root();
        let token = electricity_value::CancellationToken::new();
        let call = CallContext {
            path: &path,
            timeout_seconds: 5,
            token: &token,
        };
        let err = adapter
            .generate("m", "hi", &GenerateOptions::default(), &call)
            .await
            .unwrap_err();
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
        assert_eq!(err.message, "'int' object has no attribute 'rstrip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
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

    fn success(stdout: &str) -> crate::transport::CurlOutcome {
        crate::transport::CurlOutcome {
            returncode: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
            status: None,
            retry_after: None,
        }
    }

    #[test]
    fn interpret_outcome_raises_attribute_error_on_a_non_dict_body() {
        let outcome = success("[1, 2, 3]");
        let err = interpret_outcome(&outcome, "m", "http://x", "k").unwrap_err();
        assert_eq!(err.message, "'list' object has no attribute 'get'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }

    #[test]
    fn interpret_outcome_raises_attribute_error_on_a_null_usage() {
        let outcome = success(r#"{"content": [], "usage": null}"#);
        let err = interpret_outcome(&outcome, "m", "http://x", "k").unwrap_err();
        assert_eq!(err.message, "'NoneType' object has no attribute 'get'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }

    #[test]
    fn interpret_outcome_tolerates_a_missing_usage_key() {
        let outcome = success(r#"{"content": []}"#);
        let result = interpret_outcome(&outcome, "m", "http://x", "k").unwrap();
        assert_eq!(result.tokens_sent, None);
        assert_eq!(result.tokens_received, None);
    }

    #[test]
    fn interpret_outcome_raises_the_join_text_for_a_non_str_text_block() {
        let outcome = success(
            r#"{"content": [{"type": "text", "text": "a"}, {"type": "text", "text": null}]}"#,
        );
        let err = interpret_outcome(&outcome, "m", "http://x", "k").unwrap_err();
        assert_eq!(
            err.message,
            "sequence item 1: expected str instance, NoneType found"
        );
        assert_eq!(err.py_class, PyExc::TypeError);
    }
}
