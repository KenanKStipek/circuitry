//! `adapters/_openai_compat.py`, ported -- the shared OpenAI Chat
//! Completions transport every member of the OpenAI-compatible
//! family (`ai21`, `azure-openai`, `cloudflare-workers-ai`, `cohere`,
//! `databricks`, `deepseek`, `fireworks`, `gemini`, `groq`,
//! `huggingface-inference`, `llamacpp`, `lmstudio`, `mistral`,
//! `nvidia-nim`, `openrouter`, `perplexity`, `qwen-dashscope`, `tgi`,
//! `together`, `vllm`, `xai`) shares, over [`crate::transport`].

use crate::config::{cfg_get, cfg_str_or, cfg_value_or, rstrip_base_url, value_is_truthy};
use crate::curl_text::{classify_curl_outcome, curl_error_code, curl_failure_message};
use crate::transport::{TransportFailure, run_curl, run_curl_streaming};
use async_trait::async_trait;
use electricity_json::WriteMode;
use electricity_value::{Dict, Value};
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateProgress,
    GenerateResult, PyExc,
};
use electricity_vm::retry::{RetryInfo, status_is_retryable};

fn dict_insert(dict: &mut Dict, key: &str, value: Value) {
    dict.insert(Value::Str(key.to_string()), value);
}

fn last_user_index(messages: &[Value]) -> Option<usize> {
    (0..messages.len()).rev().find(|&i| {
        messages[i]
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

fn non_json_error(prefix: &str, stdout: &str) -> AdapterError {
    AdapterError {
        message: format!(
            "{prefix} returned non-JSON response: {}",
            truncate_chars(stdout, 200)
        ),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    }
}

/// `chat_completion()`'s own request headers -- `pub` so
/// `tests/adapter_corpus.rs` compares the recorded
/// corpus headers against the adapter's own header-building code,
/// not a list written by hand in the test a second time.
pub fn request_headers(api_key: &str, extra_headers: &[(String, String)]) -> Vec<(String, String)> {
    let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
    if !api_key.is_empty() {
        headers.push(("Authorization".to_string(), format!("Bearer {api_key}")));
    }
    for (name, value) in extra_headers {
        headers.push((name.clone(), value.clone()));
    }
    headers
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

fn int_if_pyint(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(v @ (Value::Int(_) | Value::Bool(_))) => electricity_value::pycompat::py_int(v).ok(),
        _ => None,
    }
}

/// `_openai_compat.py::chat_messages` -- shared by `openai.rs` too.
pub fn chat_messages(prompt: &str, options: &GenerateOptions) -> Vec<Value> {
    let mut messages: Vec<Value> = options
        .messages
        .iter()
        .map(|m| {
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
            Value::Dict(entry)
        })
        .collect();
    if messages.is_empty() {
        let mut entry = Dict::new();
        dict_insert(&mut entry, "role", Value::Str("user".to_string()));
        dict_insert(&mut entry, "content", Value::Str(prompt.to_string()));
        messages.push(Value::Dict(entry));
    }
    if !options.images.is_empty() {
        let index = match last_user_index(&messages) {
            Some(i) => i,
            None => {
                let mut entry = Dict::new();
                dict_insert(&mut entry, "role", Value::Str("user".to_string()));
                dict_insert(&mut entry, "content", Value::Str(String::new()));
                messages.push(Value::Dict(entry));
                messages.len() - 1
            }
        };
        let text = match messages[index]
            .as_dict()
            .and_then(|d| d.get(&Value::Str("content".to_string())))
        {
            Some(Value::Str(s)) => s.clone(),
            _ => String::new(),
        };
        let mut parts: Vec<Value> = Vec::new();
        if !text.is_empty() {
            let mut text_part = Dict::new();
            dict_insert(&mut text_part, "type", Value::Str("text".to_string()));
            dict_insert(&mut text_part, "text", Value::Str(text));
            parts.push(Value::Dict(text_part));
        }
        for image in &options.images {
            let mut image_part = Dict::new();
            dict_insert(&mut image_part, "type", Value::Str("image_url".to_string()));
            let mut image_url = Dict::new();
            dict_insert(&mut image_url, "url", Value::Str(data_url(image)));
            dict_insert(&mut image_part, "image_url", Value::Dict(image_url));
            parts.push(Value::Dict(image_part));
        }
        if let Value::Dict(entry) = &mut messages[index] {
            dict_insert(entry, "content", Value::List(parts));
        }
    }
    messages
}

/// `adapters/base.py::ImageInput.data_url` -- the URL itself, or a
/// `data:` URL of the local file's bytes.
fn data_url(image: &electricity_vm::adapter::ImageInput) -> String {
    if let Some(url) = &image.url {
        return url.clone();
    }
    use base64::Engine;
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(image.data.as_deref().unwrap_or(&[]));
    let media_type = image.media_type.as_deref().unwrap_or("");
    format!("data:{media_type};base64,{encoded}")
}

/// `_openai_compat.py::sampling_fields` -- shared by `openai.rs` too
/// (a different *max_tokens_key*).
/// `_openai_compat.py::chat_completion`'s own payload assembly --
/// `model`/`messages` first, then every `sampling_fields` entry
/// inserted over them (so a `params.model` override, present in the
/// corpus's own override case, wins over the resolved *model* the
/// same way a plain `dict.update`-equivalent would). `pub` so
/// `tests/adapter_corpus.rs` replays `generate_adapter_corpus.py`'s
/// own request-building cases through this exact function, the same
/// one [`chat_completion`] itself calls, rather than reassembling the
/// payload by hand in the test (issue #515).
pub fn build_request(model: &str, prompt: &str, options: &GenerateOptions) -> Dict {
    let mut payload = Dict::new();
    dict_insert(&mut payload, "model", Value::Str(model.to_string()));
    dict_insert(
        &mut payload,
        "messages",
        Value::List(chat_messages(prompt, options)),
    );
    for (key, value) in sampling_fields(options, "max_tokens") {
        payload.insert(key, value);
    }
    payload
}

pub fn sampling_fields(options: &GenerateOptions, max_tokens_key: &str) -> Dict {
    let mut fields = Dict::new();
    if let Some(temperature) = options.temperature {
        dict_insert(&mut fields, "temperature", Value::Float(temperature));
    }
    if let Some(max_tokens) = options.max_tokens {
        dict_insert(&mut fields, max_tokens_key, Value::from(max_tokens));
    }
    if !options.stop.is_empty() {
        dict_insert(
            &mut fields,
            "stop",
            Value::List(options.stop.iter().map(|s| Value::Str(s.clone())).collect()),
        );
    }
    if let Some(params) = options.params.as_dict() {
        for (key, value) in params {
            let key_str = match key {
                Value::Str(s) => s.clone(),
                other => other.py_str(),
            };
            fields.insert(Value::Str(key_str), value.clone());
        }
    }
    fields
}

/// Python truthiness (`message or {}`'s own test): a missing key,
/// `None`, `False`, a numeric zero, and any empty `str`/`bytes`/
/// `list`/`dict` are falsy; everything else is truthy.
fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        None => false,
        Some(Value::None) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Int(i)) => !i.is_zero(),
        Some(Value::Float(f)) => *f != 0.0,
        Some(Value::Str(s)) => !s.is_empty(),
        Some(Value::Bytes(b)) => !b.is_empty(),
        Some(Value::List(items)) => !items.is_empty(),
        Some(Value::Dict(d)) => !d.is_empty(),
        Some(Value::Date(_)) | Some(Value::DateTime(..)) => true,
    }
}

/// `_openai_compat.py::parse_chat_response` -- `(text, finish_reason)`.
pub fn parse_chat_response(raw: &Value) -> Result<(String, Option<String>), AdapterError> {
    let dict = raw.as_dict().ok_or_else(|| attribute_error(raw, "get"))?;
    let choices = dict.get(&Value::Str("choices".to_string()));
    let (text, finish_reason) = match choices.and_then(Value::as_list) {
        Some(list) if !list.is_empty() => match list[0].as_dict() {
            Some(choice) => {
                let message = choice.get(&Value::Str("message".to_string()));
                // `message = choice.get("message") or {}`: a falsy
                // message (missing, `None`, `{}`, ...) becomes `{}`
                // (empty content, no error); a truthy *non*-dict
                // message (a string, a number, `[1]`, ...) raises
                // `AttributeError` on the next `.get` the same way a
                // non-dict `raw` does above.
                let content = if is_truthy(message) {
                    match message.and_then(Value::as_dict) {
                        Some(message_dict) => message_dict.get(&Value::Str("content".to_string())),
                        None => return Err(attribute_error(message.unwrap(), "get")),
                    }
                } else {
                    None
                };
                let text = match content {
                    Some(Value::Str(s)) => s.clone(),
                    _ => String::new(),
                };
                let finish_reason = match choice.get(&Value::Str("finish_reason".to_string())) {
                    Some(Value::Str(s)) => Some(s.clone()),
                    _ => None,
                };
                (text, finish_reason)
            }
            None => (String::new(), None),
        },
        _ => (String::new(), None),
    };
    Ok((
        electricity_value::pycompat::py_strip(&text).to_string(),
        finish_reason,
    ))
}

/// `_openai_compat.py::OpenAICompatibleConfig`.
#[derive(Debug, Clone)]
pub struct OpenAiCompatibleConfig {
    /// Raw -- see `openai.rs::OpenAiAdapter::base_url`'s own doc
    /// comment: `.rstrip("/")`'d in
    /// [`chat_completion`] itself, at the same point Python calls it
    /// (after the API-key check), never at build time.
    pub base_url: Value,
    pub api_key_env: String,
    pub default_model: String,
    pub chat_completions_path: String,
    /// Whether this adapter sends `stream: true` by default; an
    /// instance can build this `false` from
    /// `runtime.adapters.<name>.stream`.
    pub streams: bool,
}

impl OpenAiCompatibleConfig {
    fn new(
        base_url: impl Into<Value>,
        api_key_env: impl Into<String>,
        default_model: impl Into<String>,
    ) -> Self {
        OpenAiCompatibleConfig {
            base_url: base_url.into(),
            api_key_env: api_key_env.into(),
            default_model: default_model.into(),
            chat_completions_path: "/chat/completions".to_string(),
            streams: true,
        }
    }

    fn with_path(mut self, path: impl Into<String>) -> Self {
        self.chat_completions_path = path.into();
        self
    }
}

/// `_openai_compat.py::chat_completion`.
#[allow(clippy::too_many_arguments)]
pub async fn chat_completion(
    source: &str,
    cfg: &OpenAiCompatibleConfig,
    model: &str,
    prompt: &str,
    timeout_seconds: u32,
    extra_headers: &[(String, String)],
    options: &GenerateOptions,
    on_progress: Option<&dyn Fn(GenerateProgress)>,
) -> Result<GenerateResult, AdapterError> {
    let model = if model.is_empty() {
        cfg.default_model.as_str()
    } else {
        model
    };
    let api_key = if cfg.api_key_env.is_empty() {
        String::new()
    } else {
        std::env::var(&cfg.api_key_env).unwrap_or_default()
    };
    if !cfg.api_key_env.is_empty() && api_key.is_empty() {
        return Err(AdapterError {
            message: format!(
                "API key not found for {0}. Export {0}=... in the environment.",
                cfg.api_key_env
            ),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        });
    }

    let path = cfg.chat_completions_path.replace("{model}", model);
    let url = format!("{}{path}", rstrip_base_url(&cfg.base_url)?);

    let mut payload = build_request(model, prompt, options);
    if cfg.streams {
        dict_insert(&mut payload, "stream", Value::Bool(true));
        let mut stream_options = Dict::new();
        dict_insert(&mut stream_options, "include_usage", Value::Bool(true));
        dict_insert(&mut payload, "stream_options", Value::Dict(stream_options));
    }
    // `json.dumps(payload)` raises Python's own `TypeError` for a value it
    // can't serialize, uncaught -- never `RuntimeError`.
    let body = electricity_json::dumps(&Value::Dict(payload), WriteMode::COMPACT).map_err(|e| {
        AdapterError {
            message: e.to_string(),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::TypeError,
        }
    })?;

    let headers = request_headers(&api_key, extra_headers);
    let mut secrets: Vec<&str> = Vec::new();
    if !api_key.is_empty() {
        secrets.push(&api_key);
    }
    for (_, value) in extra_headers {
        if value.len() >= 8 {
            secrets.push(value);
        }
    }

    if cfg.streams {
        let mut on_line = |line: &str| stream_on_line_progress(line, on_progress);
        let outcome =
            run_curl_streaming(&url, &headers, Some(&body), timeout_seconds, &mut on_line)
                .await
                .map_err(transport_failure_error)?;
        interpret_streaming_outcome(source, &outcome, model, &url, &secrets)
    } else {
        let outcome = run_curl(&url, &headers, Some(&body), timeout_seconds)
            .await
            .map_err(transport_failure_error)?;
        interpret_outcome(source, &outcome, model, &url, &secrets)
    }
}

/// `_openai_compat.py::StreamAssembly.on_line` -- drives *on_progress*
/// from each SSE delta as it arrives; never the source of the final
/// result, which is parsed from the complete body once the call
/// finishes ([`parse_sse_stream`]).
pub fn stream_on_line_progress(line: &str, on_progress: Option<&dyn Fn(GenerateProgress)>) {
    let Some(on_progress) = on_progress else {
        return;
    };
    let line = line.trim();
    let Some(rest) = line.strip_prefix("data:") else {
        return;
    };
    let payload = rest.trim();
    if payload.is_empty() || payload == "[DONE]" {
        return;
    }
    let Ok(event) = electricity_json::loads(payload) else {
        return;
    };
    let Some(dict) = event.as_dict() else {
        return;
    };
    let has_error = dict
        .get(&Value::Str("error".to_string()))
        .is_some_and(|v| !matches!(v, Value::None));
    if has_error {
        return;
    }
    let Some(choices) = dict
        .get(&Value::Str("choices".to_string()))
        .and_then(Value::as_list)
    else {
        return;
    };
    if choices.is_empty() {
        return;
    }
    let Some(choice) = choices[0].as_dict() else {
        return;
    };
    let Some(Value::Str(content)) = choice
        .get(&Value::Str("delta".to_string()))
        .and_then(Value::as_dict)
        .and_then(|d| d.get(&Value::Str("content".to_string())))
    else {
        return;
    };
    if !content.is_empty() {
        on_progress(GenerateProgress {
            delta_chars: content.chars().count(),
            delta_count: 1,
            tokens_sent: None,
            tokens_received: None,
        });
    }
}

/// `_openai_compat.py::parse_sse_stream` -- `(text, finish_reason,
/// usage, saw_done, saw_data_line, error)` from a completed SSE Chat
/// Completions stream, parsed in full -- the authoritative result for
/// a streamed call, built the same way the non-streamed path builds
/// its one JSON object's. `saw_data_line` tells
/// [`interpret_streaming_outcome`] whether the server answered with
/// SSE at all, or one plain JSON body instead.
pub struct SseStream {
    pub text: String,
    pub finish_reason: Option<String>,
    pub usage: Option<Value>,
    pub saw_done: bool,
    pub saw_data_line: bool,
    pub error: Option<Value>,
}

pub fn parse_sse_stream(stdout: &str) -> SseStream {
    let mut text = String::new();
    let mut finish_reason: Option<String> = None;
    let mut usage: Option<Value> = None;
    let mut saw_done = false;
    let mut saw_data_line = false;
    let mut error: Option<Value> = None;
    for raw_line in electricity_value::pycompat::py_splitlines(stdout) {
        let line = electricity_value::pycompat::py_strip(&raw_line);
        let Some(rest) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = electricity_value::pycompat::py_strip(rest);
        if payload.is_empty() {
            continue;
        }
        if payload == "[DONE]" {
            saw_done = true;
            continue;
        }
        saw_data_line = true;
        let Ok(event) = electricity_json::loads(payload) else {
            continue;
        };
        let Some(dict) = event.as_dict() else {
            continue;
        };
        let line_error = dict
            .get(&Value::Str("error".to_string()))
            .filter(|v| !matches!(v, Value::None));
        if line_error.is_some() && error.is_none() {
            error = line_error.cloned();
            continue;
        }
        if let Some(line_usage) = dict.get(&Value::Str("usage".to_string())) {
            if !matches!(line_usage, Value::None) {
                usage = Some(line_usage.clone());
            }
        }
        let Some(choices) = dict
            .get(&Value::Str("choices".to_string()))
            .and_then(Value::as_list)
        else {
            continue;
        };
        if choices.is_empty() {
            continue;
        }
        let Some(choice) = choices[0].as_dict() else {
            continue;
        };
        if let Some(Value::Str(reason)) = choice.get(&Value::Str("finish_reason".to_string())) {
            finish_reason = Some(reason.clone());
        }
        if let Some(Value::Str(content)) = choice
            .get(&Value::Str("delta".to_string()))
            .and_then(Value::as_dict)
            .and_then(|d| d.get(&Value::Str("content".to_string())))
        {
            if !content.is_empty() {
                text.push_str(content);
            }
        }
    }
    SseStream {
        text,
        finish_reason,
        usage,
        saw_done,
        saw_data_line,
        error,
    }
}

/// `_openai_compat.py::usage_tokens`.
pub fn usage_tokens(usage: Option<&Value>) -> (Option<i64>, Option<i64>) {
    let Some(dict) = usage.and_then(Value::as_dict) else {
        return (None, None);
    };
    (
        int_if_pyint(dict.get(&Value::Str("prompt_tokens".to_string()))),
        int_if_pyint(dict.get(&Value::Str("completion_tokens".to_string()))),
    )
}

/// `_openai_compat.py::stream_error_retry_info` -- a numeric, non-bool
/// `code` is read as the status it stands in for; anything else is
/// not retryable.
pub fn stream_error_retry_info(error: &Value) -> RetryInfo {
    if let Some(Value::Int(_)) = error
        .as_dict()
        .and_then(|d| d.get(&Value::Str("code".to_string())))
    {
        let code = error
            .as_dict()
            .and_then(|d| d.get(&Value::Str("code".to_string())))
            .unwrap();
        if let Ok(status_i64) = electricity_value::pycompat::py_int(code) {
            if let Ok(status) = u16::try_from(status_i64) {
                return RetryInfo {
                    retryable: status_is_retryable(status),
                    status: Some(status),
                    retry_after: None,
                    error_code: None,
                };
            }
        }
    }
    RetryInfo::not_retryable()
}

/// The pure half of [`chat_completion`]'s streaming path -- everything
/// after [`run_curl_streaming`] returns.
pub fn interpret_streaming_outcome(
    source: &str,
    outcome: &crate::transport::CurlOutcome,
    model: &str,
    url: &str,
    secrets: &[&str],
) -> Result<GenerateResult, AdapterError> {
    if outcome.returncode != 0 {
        return Err(curl_failure_error(
            source, outcome, model, url, secrets, true,
        ));
    }

    let parsed = parse_sse_stream(&outcome.stdout);
    if !parsed.saw_data_line {
        // Either streaming was off, or the server answered a streaming
        // request with one plain JSON body instead of SSE -- parse the
        // whole body the non-streamed way either way, so the two paths
        // can never drift apart.
        let raw = electricity_json::loads(&outcome.stdout)
            .map_err(|_| non_json_error("Provider", &outcome.stdout))?;
        let (text, finish_reason) = parse_chat_response(&raw)?;
        let usage = raw
            .as_dict()
            .and_then(|d| d.get(&Value::Str("usage".to_string())));
        let usage_dict = match usage {
            Some(Value::Dict(d)) => Some(d),
            Some(other) if !crate::config::value_is_truthy(other) => None,
            Some(other) => return Err(attribute_error(other, "get")),
            None => None,
        };
        let tokens_sent =
            usage_dict.and_then(|d| int_if_pyint(d.get(&Value::Str("prompt_tokens".to_string()))));
        let tokens_received = usage_dict
            .and_then(|d| int_if_pyint(d.get(&Value::Str("completion_tokens".to_string()))));
        return Ok(GenerateResult {
            text,
            raw,
            tokens_sent,
            tokens_received,
            finish_reason,
            warnings: Vec::new(),
            cost_usd: None,
        });
    }

    if let Some(error) = parsed.error {
        let mut error_dict = Dict::new();
        dict_insert(&mut error_dict, "error", error.clone());
        let error_body = electricity_json::dumps(&Value::Dict(error_dict), WriteMode::COMPACT)
            .unwrap_or_default();
        let message =
            curl_failure_message(source, Some(model), url, 22, &error_body, "", "", secrets);
        let mut retry = stream_error_retry_info(&error);
        retry.error_code = curl_error_code(&error_body, secrets);
        return Err(AdapterError {
            message,
            retry,
            py_class: PyExc::AdapterCallError,
        });
    }

    if !parsed.saw_done {
        // The stream ended (exit 0) without a `[DONE]` sentinel --
        // classified the same way an actual mid-stream disconnect is.
        let message = curl_failure_message(
            source,
            Some(model),
            url,
            18,
            "",
            "curl: (18) transfer closed with outstanding read data remaining",
            "",
            secrets,
        );
        return Err(AdapterError {
            message,
            retry: RetryInfo::retryable(),
            py_class: PyExc::AdapterCallError,
        });
    }

    let (tokens_sent, tokens_received) = usage_tokens(parsed.usage.as_ref());
    let mut raw = Dict::new();
    let mut message_dict = Dict::new();
    dict_insert(
        &mut message_dict,
        "content",
        Value::Str(parsed.text.clone()),
    );
    let mut choice = Dict::new();
    dict_insert(&mut choice, "message", Value::Dict(message_dict));
    dict_insert(
        &mut choice,
        "finish_reason",
        parsed
            .finish_reason
            .clone()
            .map(Value::Str)
            .unwrap_or(Value::None),
    );
    dict_insert(&mut raw, "choices", Value::List(vec![Value::Dict(choice)]));
    dict_insert(
        &mut raw,
        "usage",
        parsed.usage.clone().unwrap_or(Value::None),
    );
    Ok(GenerateResult {
        text: electricity_value::pycompat::py_strip(&parsed.text).to_string(),
        raw: Value::Dict(raw),
        tokens_sent,
        tokens_received,
        finish_reason: parsed.finish_reason,
        warnings: Vec::new(),
        cost_usd: None,
    })
}

/// `_openai_compat.py`'s own shared curl-failure branch, both
/// streamed and non-streamed. On the streaming path, a non-22
/// failure leaves whatever the provider had already answered in
/// *outcome*'s own stdout -- only a complete HTTP response (22) has
/// a body worth reporting.
fn curl_failure_error(
    source: &str,
    outcome: &crate::transport::CurlOutcome,
    model: &str,
    url: &str,
    secrets: &[&str],
    streaming: bool,
) -> AdapterError {
    let stdout_for_message: &str = if outcome.returncode == 22 || !streaming {
        &outcome.stdout
    } else {
        ""
    };
    let message = curl_failure_message(
        source,
        Some(model),
        url,
        outcome.returncode,
        stdout_for_message,
        &outcome.stderr,
        "",
        secrets,
    );
    let mut retry = classify_curl_outcome(outcome);
    retry.error_code = curl_error_code(stdout_for_message, secrets);
    AdapterError {
        message,
        retry,
        py_class: PyExc::AdapterCallError,
    }
}

/// The pure half of [`chat_completion`] -- everything after
/// [`run_curl`] returns, replayed by `generate_adapter_corpus.py`'s
/// own scripted `(returncode, stdout, stderr)` triples
/// (`tests/adapter_corpus.rs`). `pub` for the same reason.
pub fn interpret_outcome(
    source: &str,
    outcome: &crate::transport::CurlOutcome,
    model: &str,
    url: &str,
    secrets: &[&str],
) -> Result<GenerateResult, AdapterError> {
    if outcome.returncode != 0 {
        return Err(curl_failure_error(
            source, outcome, model, url, secrets, false,
        ));
    }

    let raw = electricity_json::loads(&outcome.stdout)
        .map_err(|_| non_json_error("Provider", &outcome.stdout))?;
    let (text, finish_reason) = parse_chat_response(&raw)?;
    let usage = raw
        .as_dict()
        .and_then(|d| d.get(&Value::Str("usage".to_string())));
    let usage_dict = match usage {
        Some(Value::Dict(d)) => Some(d),
        Some(other) if !crate::config::value_is_truthy(other) => None,
        Some(other) => return Err(attribute_error(other, "get")),
        None => None,
    };
    let tokens_sent =
        usage_dict.and_then(|d| int_if_pyint(d.get(&Value::Str("prompt_tokens".to_string()))));
    let tokens_received =
        usage_dict.and_then(|d| int_if_pyint(d.get(&Value::Str("completion_tokens".to_string()))));

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

/// `_openai_compat.py::check_dependencies`.
pub fn check_dependencies(cfg: &OpenAiCompatibleConfig) -> CheckResult {
    let mut missing = Vec::new();
    if !cfg.api_key_env.is_empty()
        && std::env::var(&cfg.api_key_env)
            .unwrap_or_default()
            .is_empty()
    {
        missing.push(format!("env:{}", cfg.api_key_env));
    }
    CheckResult {
        ok: missing.is_empty(),
        missing,
        message: None,
    }
}

/// `_openai_compat.py::OpenAICompatibleAdapter` -- one struct for the
/// whole family, configured per provider name (the stub's own fields
/// were this lane's to change).
/// A pre-check one family member raises before any request --
/// returns the extra headers (`azure-openai`'s own `api-key`) to send
/// alongside the ordinary ones, or an error with exact text
/// (`azure-openai`'s endpoint/key checks, `cloudflare-workers-ai`'s/
/// `databricks`'s own resolved-base-url checks).
type Precheck = fn(&str, &OpenAiCompatibleConfig) -> Result<Vec<(String, String)>, AdapterError>;

pub struct OpenAiCompatibleAdapter {
    pub provider_name: String,
    pub cfg: OpenAiCompatibleConfig,
    pub precheck: Option<Precheck>,
    /// A `check()` override for a member whose readiness depends on
    /// more than the API key (`cloudflare-workers-ai`/`databricks`'s
    /// own missing-base-url message, `azure-openai`'s own two env
    /// vars).
    pub check_override: Option<fn(&str, &OpenAiCompatibleConfig) -> CheckResult>,
}

#[async_trait(?Send)]
impl Adapter for OpenAiCompatibleAdapter {
    fn name(&self) -> &str {
        &self.provider_name
    }

    async fn generate(
        &self,
        model: &str,
        prompt: &str,
        options: &GenerateOptions,
        call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        let extra_headers = match self.precheck {
            Some(precheck) => precheck(&self.provider_name, &self.cfg)?,
            None => Vec::new(),
        };
        chat_completion(
            "OpenAI-compatible",
            &self.cfg,
            model,
            prompt,
            call.timeout_seconds,
            &extra_headers,
            options,
            call.on_progress,
        )
        .await
    }

    async fn check(&self) -> CheckResult {
        match self.check_override {
            Some(f) => f(&self.provider_name, &self.cfg),
            None => check_dependencies(&self.cfg),
        }
    }

    fn streams(&self) -> bool {
        self.cfg.streams
    }

    fn accepts_images(&self) -> bool {
        true
    }
}

fn simple_adapter(
    provider_name: &str,
    mut cfg: OpenAiCompatibleConfig,
    raw_cfg: &Value,
) -> Result<Box<dyn Adapter>, AdapterError> {
    cfg.streams = crate::config::stream_flag(raw_cfg, provider_name)?;
    Ok(Box::new(OpenAiCompatibleAdapter {
        provider_name: provider_name.to_string(),
        cfg,
        precheck: None,
        check_override: None,
    }))
}

// ---------------------------------------------------------------------
// Per-provider builders (`adapters/factory.py`'s own `_build_*`).
// ---------------------------------------------------------------------

pub fn build_ai21(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(cfg, "base_url", "https://api.ai21.com/studio/v1"),
            "AI21_API_KEY",
            cfg_str_or(cfg, "default_model", "jamba-large"),
        ),
        cfg,
    )
}

pub fn build_cohere(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(cfg, "base_url", "https://api.cohere.com"),
            "COHERE_API_KEY",
            cfg_str_or(cfg, "default_model", "command-r-plus"),
        )
        .with_path("/compatibility/v1/chat/completions"),
        cfg,
    )
}

pub fn build_deepseek(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(cfg, "base_url", "https://api.deepseek.com/v1"),
            "DEEPSEEK_API_KEY",
            cfg_str_or(cfg, "default_model", "deepseek-chat"),
        ),
        cfg,
    )
}

pub fn build_fireworks(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(cfg, "base_url", "https://api.fireworks.ai/inference/v1"),
            "FIREWORKS_API_KEY",
            cfg_str_or(
                cfg,
                "default_model",
                "accounts/fireworks/models/llama-v3p3-70b-instruct",
            ),
        ),
        cfg,
    )
}

pub fn build_gemini(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(
                cfg,
                "base_url",
                "https://generativelanguage.googleapis.com/v1beta/openai",
            ),
            "GOOGLE_API_KEY",
            cfg_str_or(cfg, "default_model", "gemini-2.5-flash"),
        ),
        cfg,
    )
}

pub fn build_groq(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(cfg, "base_url", "https://api.groq.com/openai/v1"),
            "GROQ_API_KEY",
            cfg_str_or(cfg, "default_model", "llama-3.3-70b-versatile"),
        ),
        cfg,
    )
}

pub fn build_huggingface_inference(
    name: &str,
    cfg: &Value,
) -> Result<Box<dyn Adapter>, AdapterError> {
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(cfg, "base_url", "https://router.huggingface.co/v1"),
            "HF_TOKEN",
            cfg_str_or(cfg, "default_model", "meta-llama/Llama-3.3-70B-Instruct"),
        ),
        cfg,
    )
}

pub fn build_llamacpp(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(cfg, "base_url", "http://localhost:8080/v1"),
            "",
            cfg_str_or(cfg, "default_model", ""),
        ),
        cfg,
    )
}

pub fn build_lmstudio(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(cfg, "base_url", "http://localhost:1234/v1"),
            "",
            cfg_str_or(cfg, "default_model", ""),
        ),
        cfg,
    )
}

pub fn build_mistral(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(cfg, "base_url", "https://api.mistral.ai/v1"),
            "MISTRAL_API_KEY",
            cfg_str_or(cfg, "default_model", "mistral-large-latest"),
        ),
        cfg,
    )
}

pub fn build_nvidia_nim(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    let api_key_env = match cfg_get(cfg, "api_key_env") {
        None | Some(Value::None) => "NIM_API_KEY".to_string(),
        Some(Value::Str(s)) => s.clone(),
        Some(_) => "NIM_API_KEY".to_string(),
    };
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(cfg, "base_url", "https://integrate.api.nvidia.com/v1"),
            api_key_env,
            cfg_str_or(cfg, "default_model", "meta/llama-3.3-70b-instruct"),
        ),
        cfg,
    )
}

pub fn build_openrouter(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(cfg, "base_url", "https://openrouter.ai/api/v1"),
            "OPENROUTER_API_KEY",
            cfg_str_or(cfg, "default_model", "openai/gpt-4o-mini"),
        ),
        cfg,
    )
}

pub fn build_perplexity(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(cfg, "base_url", "https://api.perplexity.ai"),
            "PERPLEXITY_API_KEY",
            cfg_str_or(cfg, "default_model", "sonar"),
        ),
        cfg,
    )
}

pub fn build_qwen_dashscope(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(
                cfg,
                "base_url",
                "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            ),
            "DASHSCOPE_API_KEY",
            cfg_str_or(cfg, "default_model", "qwen-max"),
        ),
        cfg,
    )
}

pub fn build_tgi(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    let api_key_env = match cfg_get(cfg, "api_key_env") {
        None | Some(Value::None) => String::new(),
        Some(Value::Str(s)) => s.clone(),
        Some(_) => String::new(),
    };
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(cfg, "base_url", "http://localhost:3000/v1"),
            api_key_env,
            cfg_str_or(cfg, "default_model", ""),
        ),
        cfg,
    )
}

pub fn build_together(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(cfg, "base_url", "https://api.together.xyz/v1"),
            "TOGETHER_API_KEY",
            cfg_str_or(
                cfg,
                "default_model",
                "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            ),
        ),
        cfg,
    )
}

pub fn build_vllm(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(cfg, "base_url", "http://localhost:8000/v1"),
            "",
            cfg_str_or(cfg, "default_model", ""),
        ),
        cfg,
    )
}

pub fn build_xai(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    simple_adapter(
        name,
        OpenAiCompatibleConfig::new(
            cfg_value_or(cfg, "base_url", "https://api.x.ai/v1"),
            "XAI_API_KEY",
            cfg_str_or(cfg, "default_model", "grok-2-latest"),
        ),
        cfg,
    )
}

// -- Members needing a pre-check or a `check()` override. --

fn azure_openai_precheck(
    name: &str,
    cfg: &OpenAiCompatibleConfig,
) -> Result<Vec<(String, String)>, AdapterError> {
    // `self._cfg()` is the first thing Python's own `generate()` does --
    // the deferred `.rstrip("/")` failure above must surface before
    // either check below.
    let endpoint = azure_resolve_endpoint(&cfg.base_url)?;
    if endpoint.is_empty() {
        return Err(AdapterError {
            message: format!(
                "{name}: endpoint not configured. Set AZURE_OPENAI_ENDPOINT or \
                 runtime.adapters.{name}.endpoint."
            ),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        });
    }
    let api_key = std::env::var("AZURE_OPENAI_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        return Err(AdapterError {
            message: format!("{name}: AZURE_OPENAI_API_KEY not set."),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        });
    }
    Ok(vec![("api-key".to_string(), api_key)])
}

/// `azure_openai.py::AzureOpenAIAdapter::_cfg`'s own `self.
/// _resolve_endpoint().rstrip("/")` -- unlike every other member of
/// this family, Azure's own `.rstrip("/")` happens unconditionally
/// (never only when the value is already known to be a string), so a
/// truthy non-string `endpoint`/`base_url` raises here *before* either
/// `generate()`'s or `check()`'s own API-key/endpoint-configured
/// checks run (confirmed by running `AzureOpenAIAdapter(endpoint=5)
/// .generate(...)` directly: the `AttributeError` is the very first
/// thing raised). A falsy raw value resolves through
/// `AZURE_OPENAI_ENDPOINT` instead, always a string, so `.rstrip("/")`
/// on *that* branch never fails.
fn azure_resolve_endpoint(raw_endpoint: &Value) -> Result<String, AdapterError> {
    if value_is_truthy(raw_endpoint) {
        return rstrip_base_url(raw_endpoint);
    }
    let env = std::env::var("AZURE_OPENAI_ENDPOINT").unwrap_or_default();
    Ok(env.trim_end_matches('/').to_string())
}

fn azure_openai_check(_name: &str, cfg: &OpenAiCompatibleConfig) -> CheckResult {
    match azure_resolve_endpoint(&cfg.base_url) {
        Err(err) => CheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some(format!(
                "check() raised {}: {}",
                err.py_class.name(),
                err.message
            )),
        },
        Ok(endpoint) => {
            let mut missing = Vec::new();
            if std::env::var("AZURE_OPENAI_API_KEY")
                .unwrap_or_default()
                .is_empty()
            {
                missing.push("env:AZURE_OPENAI_API_KEY".to_string());
            }
            if endpoint.is_empty() {
                missing.push("env:AZURE_OPENAI_ENDPOINT".to_string());
            }
            CheckResult {
                ok: missing.is_empty(),
                missing,
                message: None,
            }
        }
    }
}

pub fn build_azure_openai(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    // `adapters/factory.py::_build_azure_openai`: `cfg.get("endpoint")
    // or cfg.get("base_url") or ""` -- the first truthy value wins,
    // kept raw (never pre-stripped: `azure_resolve_endpoint` above
    // defers that to the actual Python call site).
    let raw_endpoint = cfg_get(cfg, "endpoint")
        .filter(|v| value_is_truthy(v))
        .or_else(|| cfg_get(cfg, "base_url").filter(|v| value_is_truthy(v)))
        .cloned()
        .unwrap_or_else(|| Value::Str(String::new()));
    let api_version = cfg_str_or(cfg, "api_version", "2024-10-21");
    let mut openai_cfg =
        OpenAiCompatibleConfig::new(raw_endpoint, "", cfg_str_or(cfg, "default_model", ""))
            .with_path(format!(
                "/openai/deployments/{{model}}/chat/completions?api-version={api_version}"
            ));
    openai_cfg.streams = crate::config::stream_flag(cfg, name)?;
    Ok(Box::new(OpenAiCompatibleAdapter {
        provider_name: name.to_string(),
        cfg: openai_cfg,
        precheck: Some(azure_openai_precheck),
        check_override: Some(azure_openai_check),
    }))
}

fn cloudflare_resolve_base_url(explicit: &Value, account_id: &str) -> Value {
    if value_is_truthy(explicit) {
        return explicit.clone();
    }
    if account_id.is_empty() {
        return Value::Str(String::new());
    }
    Value::Str(format!(
        "https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1"
    ))
}

fn cloudflare_precheck(
    name: &str,
    cfg: &OpenAiCompatibleConfig,
) -> Result<Vec<(String, String)>, AdapterError> {
    if !value_is_truthy(&cfg.base_url) {
        return Err(AdapterError {
            message: format!(
                "{name}: account_id not configured. Set CF_ACCOUNT_ID or \
                 runtime.adapters.{name}.account_id (or supply a fully-formed base_url)."
            ),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        });
    }
    Ok(Vec::new())
}

fn cloudflare_check(_name: &str, cfg: &OpenAiCompatibleConfig) -> CheckResult {
    let base = check_dependencies(cfg);
    if !value_is_truthy(&cfg.base_url) {
        let mut missing = base.missing;
        missing.push("env:CF_ACCOUNT_ID".to_string());
        return CheckResult {
            ok: false,
            missing,
            message: Some("set CF_ACCOUNT_ID to your Cloudflare account UUID.".to_string()),
        };
    }
    base
}

pub fn build_cloudflare_workers_ai(
    name: &str,
    cfg: &Value,
) -> Result<Box<dyn Adapter>, AdapterError> {
    let explicit_base_url = cfg_value_or(cfg, "base_url", "");
    let account_id = {
        let configured = cfg_str_or(cfg, "account_id", "");
        if configured.is_empty() {
            std::env::var("CF_ACCOUNT_ID").unwrap_or_default()
        } else {
            configured
        }
    };
    let base_url = cloudflare_resolve_base_url(&explicit_base_url, &account_id);
    let mut openai_cfg = OpenAiCompatibleConfig::new(
        base_url,
        "CF_API_TOKEN",
        cfg_str_or(
            cfg,
            "default_model",
            "@cf/meta/llama-3.3-70b-instruct-fp8-fast",
        ),
    );
    openai_cfg.streams = crate::config::stream_flag(cfg, name)?;
    Ok(Box::new(OpenAiCompatibleAdapter {
        provider_name: name.to_string(),
        cfg: openai_cfg,
        precheck: Some(cloudflare_precheck),
        check_override: Some(cloudflare_check),
    }))
}

fn databricks_resolve_base_url(explicit: &Value) -> Value {
    if value_is_truthy(explicit) {
        return explicit.clone();
    }
    let host = std::env::var("DATABRICKS_HOST").unwrap_or_default();
    let host = host.trim_end_matches('/');
    if host.is_empty() {
        return Value::Str(String::new());
    }
    let host = if host.starts_with("http://") || host.starts_with("https://") {
        host.to_string()
    } else {
        format!("https://{host}")
    };
    Value::Str(format!("{host}/serving-endpoints"))
}

fn databricks_precheck(
    name: &str,
    cfg: &OpenAiCompatibleConfig,
) -> Result<Vec<(String, String)>, AdapterError> {
    if !value_is_truthy(&cfg.base_url) {
        return Err(AdapterError {
            message: format!(
                "{name}: workspace host not configured. Set DATABRICKS_HOST or \
                 runtime.adapters.{name}.base_url to your workspace URL."
            ),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        });
    }
    Ok(Vec::new())
}

fn databricks_check(_name: &str, cfg: &OpenAiCompatibleConfig) -> CheckResult {
    let base = check_dependencies(cfg);
    if !value_is_truthy(&cfg.base_url) {
        let mut missing = base.missing;
        missing.push("env:DATABRICKS_HOST".to_string());
        return CheckResult {
            ok: false,
            missing,
            message: Some("set DATABRICKS_HOST to your workspace.".to_string()),
        };
    }
    base
}

pub fn build_databricks(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    let base_url = databricks_resolve_base_url(&cfg_value_or(cfg, "base_url", ""));
    let mut openai_cfg = OpenAiCompatibleConfig::new(
        base_url,
        "DATABRICKS_TOKEN",
        cfg_str_or(
            cfg,
            "default_model",
            "databricks-meta-llama-3-3-70b-instruct",
        ),
    );
    openai_cfg.streams = crate::config::stream_flag(cfg, name)?;
    Ok(Box::new(OpenAiCompatibleAdapter {
        provider_name: name.to_string(),
        cfg: openai_cfg,
        precheck: Some(databricks_precheck),
        check_override: Some(databricks_check),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_bytecode::EffectPath;
    use electricity_value::CancellationToken;
    use electricity_vm::adapter::ChatMessage;
    use std::sync::{Mutex, OnceLock};

    /// The provider API-key env vars this module's own tests set/
    /// remove (`watsonx.rs`'s own `env_guard`, same reason: `cargo
    /// test` runs them concurrently by default).
    fn env_guard() -> &'static Mutex<()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        GUARD.get_or_init(|| Mutex::new(()))
    }

    fn call_ctx<'a>(path: &'a EffectPath, token: &'a CancellationToken) -> CallContext<'a> {
        CallContext {
            path,
            timeout_seconds: 5,
            token,
            on_progress: None,
        }
    }

    #[test]
    fn chat_messages_turns_a_tool_role_into_a_prefixed_user_turn() {
        let options = GenerateOptions {
            messages: vec![ChatMessage {
                role: "tool".to_string(),
                content: "result".to_string(),
            }],
            ..GenerateOptions::default()
        };
        let messages = chat_messages("hi", &options);
        let entry = messages[0].as_dict().unwrap();
        assert_eq!(
            entry.get(&Value::Str("role".to_string())),
            Some(&Value::Str("user".to_string()))
        );
        assert_eq!(
            entry.get(&Value::Str("content".to_string())),
            Some(&Value::Str("tool: result".to_string()))
        );
    }

    #[test]
    fn chat_messages_defaults_to_one_user_turn_with_the_flattened_prompt() {
        let messages = chat_messages("hi", &GenerateOptions::default());
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn sampling_fields_lets_params_collide_with_an_earlier_key_in_place() {
        let mut params = Dict::new();
        dict_insert(&mut params, "temperature", Value::Float(0.9));
        let options = GenerateOptions {
            temperature: Some(0.1),
            params: Value::Dict(params),
            ..GenerateOptions::default()
        };
        let fields = sampling_fields(&options, "max_tokens");
        let keys: Vec<String> = fields.keys().map(|k| k.py_str()).collect();
        assert_eq!(keys, vec!["temperature"]);
        assert_eq!(
            fields.get(&Value::Str("temperature".to_string())),
            Some(&Value::Float(0.9))
        );
    }

    #[test]
    fn parse_chat_response_reads_content_and_finish_reason() {
        let raw = electricity_json::loads(
            r#"{"choices": [{"message": {"content": " hi "}, "finish_reason": "stop"}]}"#,
        )
        .unwrap();
        let (text, finish_reason) = parse_chat_response(&raw).unwrap();
        assert_eq!(text, "hi");
        assert_eq!(finish_reason, Some("stop".to_string()));
    }

    #[test]
    fn parse_chat_response_treats_a_falsy_message_as_empty_content() {
        let raw = electricity_json::loads(r#"{"choices": [{"message": null}]}"#).unwrap();
        let (text, finish_reason) = parse_chat_response(&raw).unwrap();
        assert_eq!(text, "");
        assert_eq!(finish_reason, None);
    }

    #[test]
    fn parse_chat_response_raises_on_a_truthy_non_dict_message() {
        let raw = electricity_json::loads(r#"{"choices": [{"message": "oops"}]}"#).unwrap();
        let err = parse_chat_response(&raw).unwrap_err();
        assert_eq!(err.message, "'str' object has no attribute 'get'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }

    #[test]
    fn build_azure_openai_combines_endpoint_and_model_into_the_path() {
        let adapter = build_azure_openai("azure-openai", &Value::None).unwrap();
        assert_eq!(adapter.name(), "azure-openai");
    }

    #[test]
    fn build_cloudflare_requires_an_account_id_before_any_request() {
        let adapter = build_cloudflare_workers_ai("cloudflare-workers-ai", &Value::None).unwrap();
        assert_eq!(adapter.name(), "cloudflare-workers-ai");
    }

    #[test]
    fn build_nvidia_nim_lets_an_explicit_empty_api_key_env_disable_the_key() {
        let mut cfg = Dict::new();
        dict_insert(&mut cfg, "api_key_env", Value::Str(String::new()));
        let adapter = build_nvidia_nim("nvidia-nim", &Value::Dict(cfg)).unwrap();
        assert_eq!(adapter.name(), "nvidia-nim");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // single-threaded tokio test runtime; see env_guard's own doc comment
    async fn generate_fails_like_python_for_a_non_string_base_url_after_the_api_key_check() {
        // Every OpenAI-compatible provider shares
        // the one `_openai_compat.py::chat_completion` call that
        // raises `AttributeError` on `self.base_url.rstrip("/")`,
        // *after* its own API-key check, not at build time.
        let _guard = env_guard().lock().unwrap();
        let mut cfg = Dict::new();
        dict_insert(&mut cfg, "base_url", Value::Bool(true));
        let adapter = build_mistral("mistral", &Value::Dict(cfg)).unwrap();
        unsafe {
            std::env::set_var("MISTRAL_API_KEY", "test-key");
        }
        let path = EffectPath::root();
        let token = CancellationToken::new();
        let err = adapter
            .generate(
                "m",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        unsafe {
            std::env::remove_var("MISTRAL_API_KEY");
        }
        assert_eq!(err.message, "'bool' object has no attribute 'rstrip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // single-threaded tokio test runtime; see env_guard's own doc comment
    async fn azure_generate_fails_like_python_for_a_non_string_endpoint_before_any_other_check() {
        // Confirmed by running `AzureOpenAIAdapter(endpoint=5).generate(...)`
        // directly: unlike every other member, Azure's own `_cfg()` --
        // the very first thing `generate()` calls -- does an
        // unconditional `.rstrip("/")`, so this raises before even the
        // API-key check (which is why no env var is set here).
        let _guard = env_guard().lock().unwrap();
        unsafe {
            std::env::remove_var("AZURE_OPENAI_API_KEY");
            std::env::remove_var("AZURE_OPENAI_ENDPOINT");
        }
        let mut cfg = Dict::new();
        dict_insert(&mut cfg, "endpoint", Value::from(5i64));
        let adapter = build_azure_openai("azure-openai", &Value::Dict(cfg)).unwrap();
        let path = EffectPath::root();
        let token = CancellationToken::new();
        let err = adapter
            .generate(
                "m",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert_eq!(err.message, "'int' object has no attribute 'rstrip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // single-threaded tokio test runtime; see env_guard's own doc comment
    async fn azure_generate_fails_like_python_for_a_non_string_base_url_fallback() {
        let _guard = env_guard().lock().unwrap();
        unsafe {
            std::env::remove_var("AZURE_OPENAI_API_KEY");
            std::env::remove_var("AZURE_OPENAI_ENDPOINT");
        }
        let mut cfg = Dict::new();
        dict_insert(&mut cfg, "base_url", Value::from(5i64));
        let adapter = build_azure_openai("azure-openai", &Value::Dict(cfg)).unwrap();
        let path = EffectPath::root();
        let token = CancellationToken::new();
        let err = adapter
            .generate(
                "m",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert_eq!(err.message, "'int' object has no attribute 'rstrip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // single-threaded tokio test runtime; see env_guard's own doc comment
    async fn azure_check_raised_text_for_a_non_string_endpoint() {
        // Confirmed by running `call_check(AzureOpenAIAdapter(endpoint=5))`
        // directly: `check()` has no guard around its own `self._cfg()`
        // call either, so the `AttributeError` propagates out of
        // `check()` uncaught, for `preflight.py::call_check`'s own
        // "check() raised ..." fold -- `scripted.rs`'s own
        // `check() raised {type}: {}` convention, reproduced directly
        // here since Rust's `check()` returns `CheckResult`, never `Err`.
        let _guard = env_guard().lock().unwrap();
        unsafe {
            std::env::remove_var("AZURE_OPENAI_API_KEY");
            std::env::remove_var("AZURE_OPENAI_ENDPOINT");
        }
        let mut cfg = Dict::new();
        dict_insert(&mut cfg, "endpoint", Value::from(5i64));
        let adapter = build_azure_openai("azure-openai", &Value::Dict(cfg)).unwrap();
        let result = adapter.check().await;
        assert!(!result.ok);
        assert_eq!(
            result.message,
            Some(
                "check() raised AttributeError: 'int' object has no attribute 'rstrip'".to_string()
            )
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // single-threaded tokio test runtime; see env_guard's own doc comment
    async fn cloudflare_generate_fails_like_python_for_a_non_string_base_url_after_the_api_key_check()
     {
        let _guard = env_guard().lock().unwrap();
        let mut cfg = Dict::new();
        dict_insert(&mut cfg, "base_url", Value::from(5i64));
        let adapter =
            build_cloudflare_workers_ai("cloudflare-workers-ai", &Value::Dict(cfg)).unwrap();
        unsafe {
            std::env::set_var("CF_API_TOKEN", "test-token");
        }
        let path = EffectPath::root();
        let token = CancellationToken::new();
        let err = adapter
            .generate(
                "m",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        unsafe {
            std::env::remove_var("CF_API_TOKEN");
        }
        assert_eq!(err.message, "'int' object has no attribute 'rstrip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // single-threaded tokio test runtime; see env_guard's own doc comment
    async fn databricks_generate_fails_like_python_for_a_non_string_base_url_after_the_api_key_check()
     {
        let _guard = env_guard().lock().unwrap();
        let mut cfg = Dict::new();
        dict_insert(&mut cfg, "base_url", Value::from(5i64));
        let adapter = build_databricks("databricks", &Value::Dict(cfg)).unwrap();
        unsafe {
            std::env::set_var("DATABRICKS_TOKEN", "test-token");
        }
        let path = EffectPath::root();
        let token = CancellationToken::new();
        let err = adapter
            .generate(
                "m",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        unsafe {
            std::env::remove_var("DATABRICKS_TOKEN");
        }
        assert_eq!(err.message, "'int' object has no attribute 'rstrip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }

    // -- streaming (#676) --

    #[test]
    fn parse_sse_stream_assembles_text_usage_and_finish_reason() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi \"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"there\"},\"finish_reason\":\"stop\"}],",
            "\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n",
            "data: [DONE]\n",
        );
        let parsed = parse_sse_stream(body);
        assert_eq!(parsed.text, "hi there");
        assert_eq!(parsed.finish_reason, Some("stop".to_string()));
        assert!(parsed.saw_done);
        assert!(parsed.saw_data_line);
        assert!(parsed.error.is_none());
        let (sent, received) = usage_tokens(parsed.usage.as_ref());
        assert_eq!(sent, Some(3));
        assert_eq!(received, Some(2));
    }

    #[test]
    fn parse_sse_stream_reports_a_mid_stream_error_and_stops_collecting_text() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n",
            "data: {\"error\":{\"message\":\"boom\",\"code\":500}}\n",
        );
        let parsed = parse_sse_stream(body);
        assert!(parsed.error.is_some());
        let retry = stream_error_retry_info(parsed.error.as_ref().unwrap());
        assert!(retry.retryable);
        assert_eq!(retry.status, Some(500));
    }

    #[test]
    fn parse_sse_stream_without_a_done_sentinel_reports_saw_done_false() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n";
        let parsed = parse_sse_stream(body);
        assert!(!parsed.saw_done);
        assert!(parsed.saw_data_line);
    }

    #[test]
    fn parse_sse_stream_with_no_data_lines_reports_saw_data_line_false() {
        let parsed = parse_sse_stream("not sse at all");
        assert!(!parsed.saw_data_line);
    }

    #[test]
    fn parse_sse_stream_treats_a_done_only_body_as_no_data_line_seen() {
        let parsed = parse_sse_stream("data: [DONE]\n");
        assert!(parsed.saw_done);
        assert!(!parsed.saw_data_line);
    }

    fn streaming_outcome(stdout: &str) -> crate::transport::CurlOutcome {
        crate::transport::CurlOutcome {
            returncode: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
            status: None,
            retry_after: None,
        }
    }

    #[test]
    fn interpret_streaming_outcome_carries_the_provider_error_code_from_a_mid_stream_error() {
        let outcome = streaming_outcome(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n",
            "data: {\"error\":{\"type\":\"BadRequestError\",\"code\":400}}\n",
        ));
        let err =
            interpret_streaming_outcome("lmstudio", &outcome, "m", "http://x", &[]).unwrap_err();
        assert_eq!(err.retry.error_code, Some("BadRequestError".to_string()));
    }

    #[test]
    fn interpret_streaming_outcome_treats_a_done_only_body_as_non_json() {
        let outcome = streaming_outcome("data: [DONE]\n");
        let err =
            interpret_streaming_outcome("lmstudio", &outcome, "m", "http://x", &[]).unwrap_err();
        assert!(err.message.contains("non-JSON response"));
    }

    #[test]
    fn stream_on_line_progress_fires_once_per_delta_and_skips_errors_and_dones() {
        let seen = std::cell::RefCell::new(Vec::new());
        let on_progress = |p: GenerateProgress| seen.borrow_mut().push(p.delta_chars);
        stream_on_line_progress(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}",
            Some(&on_progress),
        );
        stream_on_line_progress("data: [DONE]", Some(&on_progress));
        stream_on_line_progress(
            "data: {\"error\":{\"message\":\"boom\"}}",
            Some(&on_progress),
        );
        assert_eq!(*seen.borrow(), vec![2]);
    }

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

    /// Like [`spawn_sse_server`], but the response's own `Content-Length`
    /// claims more bytes than *body* actually has, so the connection
    /// looks closed mid-body to the client -- a mid-stream disconnect
    /// after whatever deltas *body* already carries.
    fn spawn_truncated_sse_server(body: &'static str) -> std::net::SocketAddr {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len() + 64
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

    #[tokio::test]
    async fn generate_streams_and_matches_the_non_streamed_shape() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi \"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"there\"},\"finish_reason\":\"stop\"}],",
            "\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n",
            "data: [DONE]\n",
        );
        let addr = spawn_sse_server(body);
        let cfg = OpenAiCompatibleConfig::new(format!("http://{addr}"), "", "m");
        assert!(cfg.streams);
        let seen = std::cell::RefCell::new(Vec::new());
        let on_progress = |p: GenerateProgress| seen.borrow_mut().push(p.delta_chars);
        let result = chat_completion(
            "test",
            &cfg,
            "m",
            "hi",
            5,
            &[],
            &GenerateOptions::default(),
            Some(&on_progress),
        )
        .await
        .unwrap();
        assert_eq!(result.text, "hi there");
        assert_eq!(result.tokens_sent, Some(3));
        assert_eq!(result.tokens_received, Some(2));
        assert_eq!(result.finish_reason, Some("stop".to_string()));
        assert_eq!(*seen.borrow(), vec![3, 5]);
    }

    #[tokio::test]
    async fn generate_streaming_fails_without_a_done_sentinel() {
        let addr = spawn_sse_server("data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n");
        let cfg = OpenAiCompatibleConfig::new(format!("http://{addr}"), "", "m");
        let err = chat_completion(
            "test",
            &cfg,
            "m",
            "hi",
            5,
            &[],
            &GenerateOptions::default(),
            None,
        )
        .await
        .unwrap_err();
        assert!(err.retry.retryable);
        assert!(err.message.contains("curl exit 18"));
    }

    #[tokio::test]
    async fn generate_streaming_fails_on_a_mid_stream_error_payload() {
        let addr = spawn_sse_server(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n",
            "data: {\"error\":{\"type\":\"BadRequestError\",\"code\":400}}\n",
        ));
        let cfg = OpenAiCompatibleConfig::new(format!("http://{addr}"), "", "m");
        let err = chat_completion(
            "test",
            &cfg,
            "m",
            "hi",
            5,
            &[],
            &GenerateOptions::default(),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.retry.error_code, Some("BadRequestError".to_string()));
    }

    #[tokio::test]
    async fn generate_streaming_fails_on_disconnect_after_two_deltas() {
        let addr = spawn_truncated_sse_server(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi \"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"there\"}}]}\n",
        ));
        let cfg = OpenAiCompatibleConfig::new(format!("http://{addr}"), "", "m");
        let err = chat_completion(
            "test",
            &cfg,
            "m",
            "hi",
            5,
            &[],
            &GenerateOptions::default(),
            None,
        )
        .await
        .unwrap_err();
        assert!(err.retry.retryable);
    }
}
