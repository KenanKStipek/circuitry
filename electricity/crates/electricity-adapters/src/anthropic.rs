//! `adapters/anthropic.py::AnthropicAdapter`, ported -- the Messages
//! API, over [`crate::transport`]: system-turn hoisting, image blocks
//! ahead of text, and `stop_reason`'s own truncation signal.

use crate::config::{cfg_str_or, cfg_value_or, rstrip_base_url, stream_flag};
use crate::curl_text::{classify_curl_outcome, curl_error_code, curl_failure_message};
use crate::transport::{TransportFailure, run_curl, run_curl_streaming};
use async_trait::async_trait;
use electricity_json::WriteMode;
use electricity_value::pycompat::{py_int, py_strip};
use electricity_value::{Dict, Value};
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateProgress,
    GenerateResult, ImageInput, PyExc,
};
use electricity_vm::retry::{RetryInfo, status_is_retryable};
use std::cell::Cell;

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
    stream: bool,
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
    if stream {
        dict_insert(&mut body, "stream", Value::Bool(true));
    }
    body
}

/// The pure half of [`Adapter::generate`] -- everything after
/// [`run_curl`] returns, replayed by `generate_adapter_corpus.py`'s
/// own scripted `(returncode, stdout, stderr)` triples
/// (`tests/adapter_corpus.rs`). `pub` for the same reason.
/// Shared by the streamed and non-streamed failure paths -- on the
/// streaming path, a non-22 failure leaves whatever Anthropic had
/// already answered in *outcome*'s own stdout -- only a complete HTTP
/// response (22) has a body worth reporting.
fn curl_failure_error(
    outcome: &crate::transport::CurlOutcome,
    model: &str,
    url: &str,
    api_key: &str,
    streaming: bool,
) -> AdapterError {
    let stdout_for_message: &str = if outcome.returncode == 22 || !streaming {
        &outcome.stdout
    } else {
        ""
    };
    let message = curl_failure_message(
        "anthropic",
        Some(model),
        url,
        outcome.returncode,
        stdout_for_message,
        &outcome.stderr,
        "",
        &[api_key],
    );
    let mut retry = classify_curl_outcome(outcome);
    retry.error_code = curl_error_code(stdout_for_message, &[api_key]);
    AdapterError {
        message,
        retry,
        py_class: PyExc::AdapterCallError,
    }
}

pub fn interpret_outcome(
    outcome: &crate::transport::CurlOutcome,
    model: &str,
    url: &str,
    api_key: &str,
) -> Result<GenerateResult, AdapterError> {
    if outcome.returncode != 0 {
        return Err(curl_failure_error(outcome, model, url, api_key, false));
    }

    let raw = electricity_json::loads(&outcome.stdout).map_err(|_| AdapterError {
        message: format!(
            "Anthropic returned non-JSON response: {}",
            truncate_chars(&outcome.stdout, 200)
        ),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    })?;
    interpret_body(raw)
}

/// The common "whole JSON body" interpretation both the non-streamed
/// path and a streaming fallback (a server that answers a streaming
/// request with one plain JSON body instead of SSE) share.
fn interpret_body(raw: Value) -> Result<GenerateResult, AdapterError> {
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

fn int_if_pyint(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(v @ (Value::Int(_) | Value::Bool(_))) => py_int(v).ok(),
        _ => None,
    }
}

fn event_type(dict: &Dict) -> &str {
    match dict.get(&Value::Str("type".to_string())) {
        Some(Value::Str(s)) => s.as_str(),
        _ => "",
    }
}

/// `adapters/anthropic.py::_AnthropicStream.on_line` -- drives
/// *on_progress* from each SSE delta as it arrives; never the source
/// of the final result, which is parsed from the complete body once
/// the call finishes ([`parse_anthropic_stream`]). *tokens_sent*
/// carries the most recently reported exact input-token count
/// (`message_start`) onto every later progress call, so
/// `meta.live.tokens_sent` stays populated once known -- updated
/// whether or not *on_progress* is set, the same as Python.
pub fn stream_on_line_progress(
    line: &str,
    on_progress: Option<&dyn Fn(GenerateProgress)>,
    tokens_sent: &Cell<Option<i64>>,
) {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("data:") else {
        return;
    };
    let payload = rest.trim();
    if payload.is_empty() {
        return;
    }
    let Ok(event) = electricity_json::loads(payload) else {
        return;
    };
    let Some(dict) = event.as_dict() else {
        return;
    };
    match event_type(dict) {
        "message_start" => {
            let Some(Value::Dict(message)) = dict.get(&Value::Str("message".to_string())) else {
                return;
            };
            let Some(input_tokens) = message
                .get(&Value::Str("usage".to_string()))
                .and_then(Value::as_dict)
                .and_then(|d| int_if_pyint(d.get(&Value::Str("input_tokens".to_string()))))
            else {
                return;
            };
            tokens_sent.set(Some(input_tokens));
            if let Some(on_progress) = on_progress {
                on_progress(GenerateProgress {
                    delta_chars: 0,
                    delta_count: 1,
                    tokens_sent: Some(input_tokens),
                    tokens_received: None,
                });
            }
        }
        "content_block_delta" => {
            let Some(on_progress) = on_progress else {
                return;
            };
            let Some(Value::Str(text)) = dict
                .get(&Value::Str("delta".to_string()))
                .and_then(Value::as_dict)
                .and_then(|d| d.get(&Value::Str("text".to_string())))
            else {
                return;
            };
            if !text.is_empty() {
                on_progress(GenerateProgress {
                    delta_chars: text.chars().count(),
                    delta_count: 1,
                    tokens_sent: tokens_sent.get(),
                    tokens_received: None,
                });
            }
        }
        _ => {}
    }
}

/// `adapters/anthropic.py::parse_anthropic_stream` -- a completed
/// Anthropic Messages API SSE stream, parsed in full -- the
/// authoritative result for a streamed call, built the same way the
/// non-streamed path builds its one JSON object's.
pub struct AnthropicStream {
    pub text: String,
    pub tokens_sent: Option<i64>,
    pub tokens_received: Option<i64>,
    pub stop_reason: Option<String>,
    pub saw_message_stop: bool,
    pub saw_event: bool,
    pub error_message: Option<String>,
    pub error_type: Option<String>,
}

pub fn parse_anthropic_stream(stdout: &str) -> AnthropicStream {
    let mut text = String::new();
    let mut tokens_sent: Option<i64> = None;
    let mut tokens_received: Option<i64> = None;
    let mut stop_reason: Option<String> = None;
    let mut saw_message_stop = false;
    let mut saw_event = false;
    let mut error_message: Option<String> = None;
    let mut error_type: Option<String> = None;
    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        let Some(rest) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = rest.trim();
        if payload.is_empty() {
            continue;
        }
        let Ok(event) = electricity_json::loads(payload) else {
            continue;
        };
        let Some(dict) = event.as_dict() else {
            continue;
        };
        saw_event = true;
        match event_type(dict) {
            "error" if error_message.is_none() => {
                let error = dict.get(&Value::Str("error".to_string()));
                let error_dict = error.and_then(Value::as_dict);
                error_message = Some(match error_dict.and_then(|d| d.get(&Value::Str("message".to_string()))) {
                    Some(Value::Str(s)) => s.clone(),
                    _ => error
                        .map(|e| electricity_json::dumps(e, WriteMode::COMPACT).unwrap_or_default())
                        .unwrap_or_default(),
                });
                error_type = match error_dict.and_then(|d| d.get(&Value::Str("type".to_string()))) {
                    Some(Value::Str(s)) => Some(s.clone()),
                    _ => None,
                };
            }
            "message_start" => {
                if let Some(input_tokens) = dict
                    .get(&Value::Str("message".to_string()))
                    .and_then(Value::as_dict)
                    .and_then(|m| m.get(&Value::Str("usage".to_string())))
                    .and_then(Value::as_dict)
                    .and_then(|u| int_if_pyint(u.get(&Value::Str("input_tokens".to_string()))))
                {
                    tokens_sent = Some(input_tokens);
                }
            }
            "content_block_delta" => {
                if let Some(Value::Str(delta_text)) = dict
                    .get(&Value::Str("delta".to_string()))
                    .and_then(Value::as_dict)
                    .and_then(|d| d.get(&Value::Str("text".to_string())))
                {
                    if !delta_text.is_empty() {
                        text.push_str(delta_text);
                    }
                }
            }
            "message_delta" => {
                if let Some(Value::Str(reason)) = dict
                    .get(&Value::Str("delta".to_string()))
                    .and_then(Value::as_dict)
                    .and_then(|d| d.get(&Value::Str("stop_reason".to_string())))
                {
                    stop_reason = Some(reason.clone());
                }
                if let Some(output_tokens) = dict
                    .get(&Value::Str("usage".to_string()))
                    .and_then(Value::as_dict)
                    .and_then(|u| int_if_pyint(u.get(&Value::Str("output_tokens".to_string()))))
                {
                    tokens_received = Some(output_tokens);
                }
            }
            "message_stop" => {
                saw_message_stop = true;
            }
            _ => {}
        }
    }
    AnthropicStream {
        text,
        tokens_sent,
        tokens_received,
        stop_reason,
        saw_message_stop,
        saw_event,
        error_message,
        error_type,
    }
}

/// `adapters/anthropic.py::_STREAM_ERROR_STATUS`.
fn stream_error_status(error_type: Option<&str>) -> Option<u16> {
    match error_type? {
        "invalid_request_error" => Some(400),
        "authentication_error" => Some(401),
        "permission_error" => Some(403),
        "not_found_error" => Some(404),
        "request_too_large" => Some(413),
        "rate_limit_error" => Some(429),
        "api_error" => Some(500),
        "overloaded_error" => Some(529),
        _ => None,
    }
}

/// The pure half of [`Adapter::generate`]'s streaming path.
pub fn interpret_streaming_outcome(
    outcome: &crate::transport::CurlOutcome,
    model: &str,
    url: &str,
    api_key: &str,
) -> Result<GenerateResult, AdapterError> {
    if outcome.returncode != 0 {
        return Err(curl_failure_error(outcome, model, url, api_key, true));
    }

    let parsed = parse_anthropic_stream(&outcome.stdout);
    if !parsed.saw_event {
        let raw = electricity_json::loads(&outcome.stdout).map_err(|_| AdapterError {
            message: format!(
                "Anthropic returned non-JSON response: {}",
                truncate_chars(&outcome.stdout, 200)
            ),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        })?;
        return interpret_body(raw);
    }

    if let Some(error_message) = parsed.error_message {
        let status = stream_error_status(parsed.error_type.as_deref());
        let mut error_dict = Dict::new();
        dict_insert(
            &mut error_dict,
            "type",
            parsed.error_type.clone().map(Value::Str).unwrap_or(Value::None),
        );
        dict_insert(&mut error_dict, "message", Value::Str(error_message));
        let mut body = Dict::new();
        dict_insert(&mut body, "error", Value::Dict(error_dict));
        let error_body =
            electricity_json::dumps(&Value::Dict(body), WriteMode::COMPACT).unwrap_or_default();
        let message = curl_failure_message(
            "anthropic",
            Some(model),
            url,
            22,
            &error_body,
            "",
            "",
            &[api_key],
        );
        return Err(AdapterError {
            message,
            retry: RetryInfo {
                retryable: status.is_some_and(status_is_retryable),
                status,
                retry_after: None,
                error_code: parsed.error_type,
            },
            py_class: PyExc::AdapterCallError,
        });
    }

    if !parsed.saw_message_stop {
        let message = curl_failure_message(
            "anthropic",
            Some(model),
            url,
            18,
            "",
            "curl: (18) transfer closed with outstanding read data remaining",
            "",
            &[api_key],
        );
        return Err(AdapterError {
            message,
            retry: RetryInfo::retryable(),
            py_class: PyExc::AdapterCallError,
        });
    }

    let mut content_block = Dict::new();
    dict_insert(&mut content_block, "type", Value::Str("text".to_string()));
    dict_insert(&mut content_block, "text", Value::Str(parsed.text.clone()));
    let mut usage = Dict::new();
    dict_insert(
        &mut usage,
        "input_tokens",
        parsed.tokens_sent.map(Value::from).unwrap_or(Value::None),
    );
    dict_insert(
        &mut usage,
        "output_tokens",
        parsed.tokens_received.map(Value::from).unwrap_or(Value::None),
    );
    let mut raw = Dict::new();
    dict_insert(&mut raw, "content", Value::List(vec![Value::Dict(content_block)]));
    dict_insert(&mut raw, "usage", Value::Dict(usage));
    dict_insert(
        &mut raw,
        "stop_reason",
        parsed.stop_reason.clone().map(Value::Str).unwrap_or(Value::None),
    );
    Ok(GenerateResult {
        text: py_strip(&parsed.text).to_string(),
        raw: Value::Dict(raw),
        tokens_sent: parsed.tokens_sent,
        tokens_received: parsed.tokens_received,
        finish_reason: parsed.stop_reason,
        warnings: Vec::new(),
        cost_usd: None,
    })
}

/// `adapters/anthropic.py::AnthropicAdapter`.
pub struct AnthropicAdapter {
    /// `anthropic`, or an alias's own name (#656).
    pub name: String,
    /// Raw -- see `openai.rs::OpenAiAdapter::base_url`'s own doc
    /// comment.
    pub base_url: Value,
    pub default_model: String,
    pub max_tokens: i64,
    /// Whether this instance sends `stream: true`; the instance-level
    /// override for `runtime.adapters.<name>.stream: false`.
    pub streams: bool,
}

#[async_trait(?Send)]
impl Adapter for AnthropicAdapter {
    fn name(&self) -> &str {
        &self.name
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
        let payload = build_request_body(model, prompt, self.max_tokens, options, self.streams);
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
        if self.streams {
            let tokens_sent = Cell::new(None);
            let on_progress = call.on_progress;
            let mut on_line = |line: &str| stream_on_line_progress(line, on_progress, &tokens_sent);
            let outcome =
                run_curl_streaming(&url, &headers, Some(&body), call.timeout_seconds, &mut on_line)
                    .await
                    .map_err(transport_failure_error)?;
            interpret_streaming_outcome(&outcome, model, &url, &api_key)
        } else {
            let outcome = run_curl(&url, &headers, Some(&body), call.timeout_seconds)
                .await
                .map_err(transport_failure_error)?;
            interpret_outcome(&outcome, model, &url, &api_key)
        }
    }

    fn streams(&self) -> bool {
        self.streams
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
pub fn build_anthropic(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
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
        name: name.to_string(),
        base_url: cfg_value_or(cfg, "base_url", "https://api.anthropic.com"),
        default_model: cfg_str_or(cfg, "default_model", "claude-sonnet-5"),
        max_tokens,
        streams: stream_flag(cfg, name)?,
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
        let body = build_request_body("m", "hi", 4096, &options, false);
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
        let body = build_request_body("m", "hi", 4096, &options, false);
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
        let body = build_request_body("m", "hi", 4096, &options, false);
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
        let body = build_request_body("m", "hi", 4096, &options, false);
        assert_eq!(
            body.get(&Value::Str("max_tokens".to_string())),
            Some(&Value::from(50i64))
        );
    }

    #[test]
    fn build_anthropic_defaults_max_tokens_to_4096() {
        let adapter = build_anthropic("anthropic", &Value::None).unwrap();
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
        let adapter = build_anthropic("anthropic", &Value::Dict(cfg)).unwrap();
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "sk-test");
        }
        let path = electricity_bytecode::EffectPath::root();
        let token = electricity_value::CancellationToken::new();
        let call = CallContext {
            path: &path,
            timeout_seconds: 5,
            token: &token,
            on_progress: None,
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
        let err = match build_anthropic("anthropic", &Value::Dict(cfg)) {
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

    // -- streaming (#676) --

    fn spawn_sse_server(body: &'static str) -> std::net::SocketAddr {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 65536];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(response.as_bytes());
        });
        addr
    }

    #[test]
    fn build_request_body_adds_stream_only_when_asked() {
        let body = build_request_body("m", "hi", 4096, &GenerateOptions::default(), true);
        assert_eq!(
            body.get(&Value::Str("stream".to_string())),
            Some(&Value::Bool(true))
        );
        let body = build_request_body("m", "hi", 4096, &GenerateOptions::default(), false);
        assert!(!body.contains_key(&Value::Str("stream".to_string())));
    }

    #[test]
    fn parse_anthropic_stream_assembles_text_tokens_and_stop_reason() {
        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5}}}\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hi \"}}\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"there\"}}\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},",
            "\"usage\":{\"output_tokens\":2}}\n",
            "data: {\"type\":\"message_stop\"}\n",
        );
        let parsed = parse_anthropic_stream(body);
        assert_eq!(parsed.text, "hi there");
        assert_eq!(parsed.tokens_sent, Some(5));
        assert_eq!(parsed.tokens_received, Some(2));
        assert_eq!(parsed.stop_reason, Some("end_turn".to_string()));
        assert!(parsed.saw_message_stop);
        assert!(parsed.saw_event);
        assert!(parsed.error_message.is_none());
    }

    #[test]
    fn parse_anthropic_stream_reports_an_error_event() {
        let body = concat!(
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hi\"}}\n",
            "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",",
            "\"message\":\"overloaded\"}}\n",
        );
        let parsed = parse_anthropic_stream(body);
        assert_eq!(parsed.error_message, Some("overloaded".to_string()));
        assert_eq!(parsed.error_type, Some("overloaded_error".to_string()));
        assert_eq!(stream_error_status(parsed.error_type.as_deref()), Some(529));
    }

    #[test]
    fn stream_on_line_progress_carries_tokens_sent_onto_later_deltas() {
        let tokens_sent = Cell::new(None);
        let seen = std::cell::RefCell::new(Vec::new());
        let on_progress = |p: GenerateProgress| seen.borrow_mut().push((p.delta_chars, p.tokens_sent));
        stream_on_line_progress(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7}}}",
            Some(&on_progress),
            &tokens_sent,
        );
        stream_on_line_progress(
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hi\"}}",
            Some(&on_progress),
            &tokens_sent,
        );
        assert_eq!(*seen.borrow(), vec![(0, Some(7)), (2, Some(7))]);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // single-threaded tokio test runtime; see env_guard's own doc comment
    async fn generate_streams_and_matches_the_non_streamed_shape() {
        let _guard = env_guard().lock().unwrap();
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "sk-test");
        }
        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5}}}\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hi \"}}\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"there\"}}\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},",
            "\"usage\":{\"output_tokens\":2}}\n",
            "data: {\"type\":\"message_stop\"}\n",
        );
        let addr = spawn_sse_server(body);
        let adapter = AnthropicAdapter {
            name: "anthropic".to_string(),
            base_url: Value::Str(format!("http://{addr}")),
            default_model: "claude-sonnet-5".to_string(),
            max_tokens: 4096,
            streams: true,
        };
        let path = electricity_bytecode::EffectPath::root();
        let token = electricity_value::CancellationToken::new();
        let seen = std::cell::RefCell::new(Vec::new());
        let on_progress = |p: GenerateProgress| seen.borrow_mut().push(p.delta_chars);
        let call = CallContext {
            path: &path,
            timeout_seconds: 5,
            token: &token,
            on_progress: Some(&on_progress),
        };
        let result = adapter
            .generate("m", "hi", &GenerateOptions::default(), &call)
            .await
            .unwrap();
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
        assert_eq!(result.text, "hi there");
        assert_eq!(result.tokens_sent, Some(5));
        assert_eq!(result.tokens_received, Some(2));
        assert_eq!(result.finish_reason, Some("end_turn".to_string()));
        // `message_start`'s own zero-char delta (#674's own rule: it
        // carries only the input-token count, never counted as a real
        // delta by the live-progress throttle) still reaches
        // `on_progress` itself, exactly as Python's own
        // `_AnthropicStream.on_line` does.
        assert_eq!(*seen.borrow(), vec![0, 3, 5]);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // single-threaded tokio test runtime; see env_guard's own doc comment
    async fn generate_streaming_fails_without_a_message_stop_event() {
        let _guard = env_guard().lock().unwrap();
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "sk-test");
        }
        let addr = spawn_sse_server(
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hi\"}}\n",
        );
        let adapter = AnthropicAdapter {
            name: "anthropic".to_string(),
            base_url: Value::Str(format!("http://{addr}")),
            default_model: "claude-sonnet-5".to_string(),
            max_tokens: 4096,
            streams: true,
        };
        let path = electricity_bytecode::EffectPath::root();
        let token = electricity_value::CancellationToken::new();
        let call = CallContext {
            path: &path,
            timeout_seconds: 5,
            token: &token,
            on_progress: None,
        };
        let err = adapter
            .generate("m", "hi", &GenerateOptions::default(), &call)
            .await
            .unwrap_err();
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
        assert!(err.retry.retryable);
        assert!(err.message.contains("curl exit 18"));
    }
}
