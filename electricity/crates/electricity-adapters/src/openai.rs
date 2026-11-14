//! `adapters/openai.py::OpenAIAdapter`, ported -- its own file (not
//! folded into [`crate::openai_compat`]): `openai` is Circuitry's own
//! default/reference provider, with its own response-format handling
//! the generic OpenAI-compatible shape doesn't cover (the `seed`
//! placement, `max_completion_tokens`, and a looser `usage` coercion).

use crate::config::{
    cfg_str_or, cfg_value_or, resolve_required_api_key_env, rstrip_base_url, stream_flag,
};
use crate::curl_text::{
    classify_curl_outcome_with_error_code, curl_error_code, curl_failure_message,
};
use crate::openai_compat::{
    chat_messages, parse_chat_response, parse_sse_stream, sampling_fields, stream_error_retry_info,
    stream_on_line_progress, usage_tokens,
};
use crate::transport::{TransportFailure, run_curl, run_curl_streaming};
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
/// #515 -- `openai` had no request-building
/// case at all).
pub fn build_request(model: &str, prompt: &str, options: &GenerateOptions, stream: bool) -> Dict {
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
    if stream {
        payload.insert(Value::Str("stream".to_string()), Value::Bool(true));
        let mut stream_options = Dict::new();
        stream_options.insert(Value::Str("include_usage".to_string()), Value::Bool(true));
        payload.insert(
            Value::Str("stream_options".to_string()),
            Value::Dict(stream_options),
        );
    }
    payload
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
        ("Authorization".to_string(), format!("Bearer {api_key}")),
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

/// `usage_tokens`'s own `cached_tokens` coercion: an actual int or bool
/// only, never a float or numeric string, unlike [`coerce_token_count`]
/// above.
fn int_if_pyint(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(v @ (Value::Int(_) | Value::Bool(_))) => electricity_value::pycompat::py_int(v).ok(),
        _ => None,
    }
}

/// Shared by the streamed and non-streamed failure paths -- on the
/// streaming path, a non-22 failure leaves whatever OpenAI had
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
        "openai",
        Some(model),
        url,
        outcome.returncode,
        stdout_for_message,
        &outcome.stderr,
        "",
        &[api_key],
    );
    let mut retry = classify_curl_outcome_with_error_code(outcome, &[api_key]);
    retry.error_code = curl_error_code(stdout_for_message, &[api_key]);
    AdapterError {
        message,
        retry,
        py_class: PyExc::AdapterCallError,
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
        return Err(curl_failure_error(outcome, model, url, api_key, false));
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
    let tokens_cached_in = usage_dict
        .and_then(|d| d.get(&Value::Str("prompt_tokens_details".to_string())))
        .and_then(Value::as_dict)
        .and_then(|d| int_if_pyint(d.get(&Value::Str("cached_tokens".to_string()))));

    Ok(GenerateResult {
        text,
        raw,
        tokens_sent,
        tokens_received,
        tokens_cached_in,
        tokens_cache_write: None,
        finish_reason,
        warnings: Vec::new(),
        cost_usd: None,
    })
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

    let parsed = parse_sse_stream(&outcome.stdout);
    if !parsed.saw_data_line {
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
        let tokens_cached_in = usage_dict
            .and_then(|d| d.get(&Value::Str("prompt_tokens_details".to_string())))
            .and_then(Value::as_dict)
            .and_then(|d| int_if_pyint(d.get(&Value::Str("cached_tokens".to_string()))));
        return Ok(GenerateResult {
            text,
            raw,
            tokens_sent,
            tokens_received,
            tokens_cached_in,
            tokens_cache_write: None,
            finish_reason,
            warnings: Vec::new(),
            cost_usd: None,
        });
    }

    if let Some(error) = parsed.error {
        let mut error_dict = Dict::new();
        error_dict.insert(Value::Str("error".to_string()), error.clone());
        let error_body = electricity_json::dumps(&Value::Dict(error_dict), WriteMode::COMPACT)
            .unwrap_or_default();
        let message = curl_failure_message(
            "openai",
            Some(model),
            url,
            22,
            &error_body,
            "",
            "",
            &[api_key],
        );
        let mut retry = stream_error_retry_info(&error);
        retry.error_code = curl_error_code(&error_body, &[api_key]);
        return Err(AdapterError {
            message,
            retry,
            py_class: PyExc::AdapterCallError,
        });
    }

    if !parsed.saw_done {
        let message = curl_failure_message(
            "openai",
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

    let (tokens_sent, tokens_received, tokens_cached_in) = usage_tokens(parsed.usage.as_ref());
    let mut message_dict = Dict::new();
    message_dict.insert(
        Value::Str("content".to_string()),
        Value::Str(parsed.text.clone()),
    );
    let mut choice = Dict::new();
    choice.insert(Value::Str("message".to_string()), Value::Dict(message_dict));
    choice.insert(
        Value::Str("finish_reason".to_string()),
        parsed
            .finish_reason
            .clone()
            .map(Value::Str)
            .unwrap_or(Value::None),
    );
    let mut raw = Dict::new();
    raw.insert(
        Value::Str("choices".to_string()),
        Value::List(vec![Value::Dict(choice)]),
    );
    raw.insert(
        Value::Str("usage".to_string()),
        parsed.usage.clone().unwrap_or(Value::None),
    );
    Ok(GenerateResult {
        text: electricity_value::pycompat::py_strip(&parsed.text).to_string(),
        raw: Value::Dict(raw),
        tokens_sent,
        tokens_received,
        tokens_cached_in,
        tokens_cache_write: None,
        finish_reason: parsed.finish_reason,
        warnings: Vec::new(),
        cost_usd: None,
    })
}

/// `adapters/openai.py::OpenAIAdapter`.
pub struct OpenAiAdapter {
    /// `openai`, or an alias's own name (#656).
    pub name: String,
    /// Raw -- `cfg.get("base_url") or default`; `.rstrip("/")`'d in
    /// [`Adapter::generate`] itself, the same point Python calls it
    /// (after the API-key check, never at build time: a
    /// non-string value must fail `generate()`, not preflight).
    pub base_url: Value,
    pub default_model: String,
    pub streams: bool,
    pub api_key_env: String,
}

#[async_trait(?Send)]
impl Adapter for OpenAiAdapter {
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
        let api_key = std::env::var(&self.api_key_env).unwrap_or_default();
        if api_key.is_empty() {
            return Err(AdapterError {
                message: format!(
                    "OpenAI API key not found. Set {} in the environment (recommended: \
                     ~/.config/circuitry/.env). Run `cof doctor` to verify.",
                    self.api_key_env
                ),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::RuntimeError,
            });
        }

        let url = format!("{}/chat/completions", rstrip_base_url(&self.base_url)?);
        let payload = build_request(model, prompt, options, self.streams);
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
            let on_progress = call.on_progress;
            let mut on_line = |line: &str| stream_on_line_progress(line, on_progress);
            let outcome = run_curl_streaming(
                &url,
                &headers,
                Some(&body),
                call.timeout_seconds,
                &mut on_line,
            )
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
        if std::env::var(&self.api_key_env)
            .unwrap_or_default()
            .is_empty()
        {
            missing.push(format!("env:{}", self.api_key_env));
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
pub fn build_openai(name: &str, cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(Box::new(OpenAiAdapter {
        name: name.to_string(),
        base_url: cfg_value_or(cfg, "base_url", "https://api.openai.com/v1"),
        default_model: cfg_str_or(cfg, "default_model", "gpt-4o-mini"),
        streams: stream_flag(cfg, name)?,
        api_key_env: resolve_required_api_key_env(cfg, "OPENAI_API_KEY", name)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_vm::adapter::GenerateProgress;
    use std::sync::{Mutex, OnceLock};

    /// `OPENAI_API_KEY` is process environment, shared by every test in
    /// this module; `cargo test` runs them concurrently by default, so
    /// two tests setting and then removing it would race without this
    /// (`watsonx.rs`'s own `env_guard`, same reason).
    fn env_guard() -> &'static Mutex<()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        GUARD.get_or_init(|| Mutex::new(()))
    }

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
        let adapter = build_openai("openai", &Value::None).unwrap();
        assert_eq!(adapter.name(), "openai");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // single-threaded tokio test runtime; see env_guard's own doc comment
    async fn generate_fails_like_python_for_a_non_string_base_url_after_the_api_key_check() {
        // The failure happens inside `generate()`,
        // at the same point Python's own `self.base_url.rstrip("/")`
        // runs (after the API-key check, which must still run first).
        let _guard = env_guard().lock().unwrap();
        let mut cfg = Dict::new();
        cfg.insert(Value::Str("base_url".to_string()), Value::from(5i64));
        let adapter = build_openai("openai", &Value::Dict(cfg)).unwrap();
        unsafe {
            std::env::set_var("OPENAI_API_KEY", "sk-test");
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
            .generate("gpt-4o-mini", "hi", &GenerateOptions::default(), &call)
            .await
            .unwrap_err();
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
        }
        assert_eq!(err.message, "'int' object has no attribute 'rstrip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // single-threaded tokio test runtime; see env_guard's own doc comment
    async fn generate_checks_the_api_key_before_touching_base_url() {
        let _guard = env_guard().lock().unwrap();
        let mut cfg = Dict::new();
        cfg.insert(Value::Str("base_url".to_string()), Value::from(5i64));
        let adapter = build_openai("openai", &Value::Dict(cfg)).unwrap();
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
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
            .generate("gpt-4o-mini", "hi", &GenerateOptions::default(), &call)
            .await
            .unwrap_err();
        assert!(err.message.contains("OpenAI API key not found"));
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

    #[test]
    fn build_request_adds_stream_fields_only_when_streaming() {
        let payload = build_request("m", "hi", &GenerateOptions::default(), true);
        assert_eq!(
            payload.get(&Value::Str("stream".to_string())),
            Some(&Value::Bool(true))
        );
        assert!(payload.contains_key(&Value::Str("stream_options".to_string())));
        let payload = build_request("m", "hi", &GenerateOptions::default(), false);
        assert!(!payload.contains_key(&Value::Str("stream".to_string())));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // single-threaded tokio test runtime; see env_guard's own doc comment
    async fn generate_streams_and_matches_the_non_streamed_shape() {
        let _guard = env_guard().lock().unwrap();
        unsafe {
            std::env::set_var("OPENAI_API_KEY", "sk-test");
        }
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi \"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"there\"},\"finish_reason\":\"stop\"}],",
            "\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n",
            "data: [DONE]\n",
        );
        let addr = spawn_sse_server(body);
        let adapter = OpenAiAdapter {
            name: "openai".to_string(),
            base_url: Value::Str(format!("http://{addr}")),
            default_model: "gpt-4o-mini".to_string(),
            streams: true,
            api_key_env: "OPENAI_API_KEY".to_string(),
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
            .generate("gpt-4o-mini", "hi", &GenerateOptions::default(), &call)
            .await
            .unwrap();
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
        }
        assert_eq!(result.text, "hi there");
        assert_eq!(result.tokens_sent, Some(3));
        assert_eq!(result.tokens_received, Some(2));
        assert_eq!(result.finish_reason, Some("stop".to_string()));
        assert_eq!(*seen.borrow(), vec![3, 5]);
    }

    #[test]
    fn interpret_streaming_outcome_carries_the_provider_error_code_from_a_mid_stream_error() {
        let outcome = crate::transport::CurlOutcome {
            returncode: 0,
            stdout: concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n",
                "data: {\"error\":{\"type\":\"BadRequestError\",\"code\":400}}\n",
            )
            .to_string(),
            stderr: String::new(),
            status: None,
            retry_after: None,
        };
        let err = interpret_streaming_outcome(&outcome, "m", "http://x", "sk-test").unwrap_err();
        assert_eq!(err.retry.error_code, Some("BadRequestError".to_string()));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn generate_streaming_fails_on_a_mid_stream_error_payload() {
        let _guard = env_guard().lock().unwrap();
        unsafe {
            std::env::set_var("OPENAI_API_KEY", "sk-test");
        }
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n",
            "data: {\"error\":{\"type\":\"BadRequestError\",\"code\":400}}\n",
        );
        let addr = spawn_sse_server(body);
        let adapter = OpenAiAdapter {
            name: "openai".to_string(),
            base_url: Value::Str(format!("http://{addr}")),
            default_model: "gpt-4o-mini".to_string(),
            streams: true,
            api_key_env: "OPENAI_API_KEY".to_string(),
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
            .generate("gpt-4o-mini", "hi", &GenerateOptions::default(), &call)
            .await
            .unwrap_err();
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
        }
        assert_eq!(err.retry.error_code, Some("BadRequestError".to_string()));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn generate_streaming_fails_on_disconnect_after_two_deltas() {
        let _guard = env_guard().lock().unwrap();
        unsafe {
            std::env::set_var("OPENAI_API_KEY", "sk-test");
        }
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi \"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"there\"}}]}\n",
        );
        let addr = spawn_truncated_sse_server(body);
        let adapter = OpenAiAdapter {
            name: "openai".to_string(),
            base_url: Value::Str(format!("http://{addr}")),
            default_model: "gpt-4o-mini".to_string(),
            streams: true,
            api_key_env: "OPENAI_API_KEY".to_string(),
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
            .generate("gpt-4o-mini", "hi", &GenerateOptions::default(), &call)
            .await
            .unwrap_err();
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
        }
        assert!(err.retry.retryable);
    }
}
