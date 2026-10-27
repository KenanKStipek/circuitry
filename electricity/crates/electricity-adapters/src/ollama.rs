//! `adapters/ollama.py::OllamaAdapter`, ported -- native Ollama,
//! `/api/generate`/`/api/chat`, over [`crate::transport`]. Issue
//! #503's top priority: lands before the OpenAI-compatible family,
//! which Ollama is *not* routed through (ruling 1: this adapter talks
//! Ollama's own wire format, never the `/v1` compatibility shim).

use crate::config::{cfg_value_or, rstrip_base_url};
use crate::curl_text::{
    classify_curl_outcome_with_error_code, curl_failure_message, parse_error_body, redact_host_url,
};
use crate::transport::{CurlOutcome, TransportFailure, run_curl};
use async_trait::async_trait;
use electricity_json::WriteMode;
use electricity_value::{Dict, Value};
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult, PyExc,
};
use electricity_vm::retry::RetryInfo;
use std::time::Duration;

/// `adapters/ollama.py::_REQUEST_FIELDS` -- a `params` key in this set
/// is a top-level request field, every other key is a model option
/// (goes under `"options"`).
const REQUEST_FIELDS: [&str; 3] = ["format", "keep_alive", "think"];

/// `adapters/base.py::DETERMINISTIC_SEED`.
const DETERMINISTIC_SEED: i64 = 0;

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

/// `adapters/ollama.py::_image_base64` -- a local image's bytes,
/// base64; a URL image raises before any request.
fn image_base64(image: &electricity_vm::adapter::ImageInput) -> Result<String, AdapterError> {
    match &image.data {
        Some(bytes) => Ok(base64_encode(bytes)),
        None => Err(AdapterError {
            message: format!(
                "ollama cannot fetch an image URL ({}); download it and give the asset a \
                 local path.",
                image.url.as_deref().unwrap_or("")
            ),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        }),
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `generate()`'s own request headers -- `pub` so
/// `tests/adapter_corpus.rs` compares the recorded corpus headers
/// against the adapter's own header-building code, not a list written
/// by hand in the test a second time.
pub fn request_headers() -> Vec<(String, String)> {
    vec![("Content-Type".to_string(), "application/json".to_string())]
}

/// `adapters/ollama.py::_request` -- `(endpoint, payload)`. `pub`
/// (not just `pub(crate)`) so `tests/adapter_corpus.rs` can replay
/// `generate_adapter_corpus.py`'s own request-building cases directly
/// against this pure half, the same way `generate()` itself calls it.
pub fn build_request(
    model: &str,
    prompt: &str,
    options: &GenerateOptions,
) -> Result<(&'static str, Dict), AdapterError> {
    let mut payload = Dict::new();
    dict_insert(&mut payload, "model", Value::Str(model.to_string()));
    dict_insert(&mut payload, "stream", Value::Bool(false));

    let mut images_b64: Vec<Value> = Vec::new();
    for image in &options.images {
        images_b64.push(Value::Str(image_base64(image)?));
    }

    let endpoint = if !options.messages.is_empty() {
        let mut messages: Vec<Value> = options
            .messages
            .iter()
            .map(|m| {
                let mut entry = Dict::new();
                dict_insert(&mut entry, "role", Value::Str(m.role.clone()));
                dict_insert(&mut entry, "content", Value::Str(m.content.clone()));
                Value::Dict(entry)
            })
            .collect();
        if !images_b64.is_empty() {
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
            if let Value::Dict(entry) = &mut messages[index] {
                dict_insert(entry, "images", Value::List(images_b64.clone()));
            }
        }
        dict_insert(&mut payload, "messages", Value::List(messages));
        "/api/chat"
    } else {
        dict_insert(&mut payload, "prompt", Value::Str(prompt.to_string()));
        if !images_b64.is_empty() {
            dict_insert(&mut payload, "images", Value::List(images_b64));
        }
        "/api/generate"
    };

    let mut model_options = Dict::new();
    if let Some(temperature) = options.temperature {
        dict_insert(&mut model_options, "temperature", Value::Float(temperature));
    }
    if let Some(max_tokens) = options.max_tokens {
        dict_insert(&mut model_options, "num_predict", Value::from(max_tokens));
    }
    if !options.stop.is_empty() {
        dict_insert(
            &mut model_options,
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
            if REQUEST_FIELDS.contains(&key_str.as_str()) {
                dict_insert(&mut payload, &key_str, value.clone());
            } else {
                dict_insert(&mut model_options, &key_str, value.clone());
            }
        }
    }
    if options.deterministic {
        model_options
            .entry(Value::Str("seed".to_string()))
            .or_insert(Value::from(DETERMINISTIC_SEED));
    }
    if !model_options.is_empty() {
        dict_insert(&mut payload, "options", Value::Dict(model_options));
    }
    Ok((endpoint, payload))
}

/// `adapters/ollama.py::_curl_json`'s own hint selection
/// (`:112-130`).
fn ollama_hint(
    returncode: i32,
    timeout_seconds: u32,
    model: &str,
    stdout: &str,
    base_url: &str,
) -> String {
    match returncode {
        7 => format!(
            "Ollama at {base_url} is not reachable. Start it (`ollama serve`), or set \
             `runtime.adapters.ollama.base_url` in your config. Run `cof doctor` to verify \
             connectivity."
        ),
        28 => format!(
            "The model didn't finish within {timeout_seconds}s. Raise \
             `runtime.adapters.ollama.timeout_seconds` in your config (a prompt's \
             `timeout_ms` can only shorten it), or use a smaller/faster model."
        ),
        22 => {
            let body_message = parse_error_body(stdout).unwrap_or_default();
            if !model.is_empty() && body_message.to_lowercase().contains("not found") {
                format!("Try `ollama pull {model}` first.")
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((byte_index, _)) => &text[..byte_index],
        None => text,
    }
}

fn non_json_error(stdout: &str) -> AdapterError {
    AdapterError {
        message: format!(
            "curl returned non-JSON response: {}",
            truncate_chars(stdout, 200)
        ),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
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

/// The pure half of [`Adapter::generate`] -- everything after
/// [`run_curl`] returns, replayed by `generate_adapter_corpus.py`'s
/// own scripted `(returncode, stdout, stderr)` triples
/// (`tests/adapter_corpus.rs`).
pub fn interpret_outcome(
    outcome: &CurlOutcome,
    model: &str,
    timeout_seconds: u32,
    base_url: &str,
    url: &str,
) -> Result<GenerateResult, AdapterError> {
    if outcome.returncode != 0 {
        let hint = ollama_hint(
            outcome.returncode,
            timeout_seconds,
            model,
            &outcome.stdout,
            base_url,
        );
        let message = curl_failure_message(
            "ollama",
            (!model.is_empty()).then_some(model),
            url,
            outcome.returncode,
            &outcome.stdout,
            &outcome.stderr,
            &hint,
            &[],
        );
        return Err(AdapterError {
            message,
            retry: classify_curl_outcome_with_error_code(outcome, &[]),
            py_class: PyExc::AdapterCallError,
        });
    }

    let raw =
        electricity_json::loads(&outcome.stdout).map_err(|_| non_json_error(&outcome.stdout))?;
    let dict = raw.as_dict().ok_or_else(|| attribute_error(&raw, "get"))?;

    let message_field = dict.get(&Value::Str("message".to_string()));
    let text_value = match message_field {
        Some(Value::Dict(message_dict)) => message_dict
            .get(&Value::Str("content".to_string()))
            .cloned(),
        _ => dict.get(&Value::Str("response".to_string())).cloned(),
    };
    // `(text or "").strip()`: a falsy value (including `None`/absent)
    // becomes `""`; a truthy non-`str` raises CPython's own
    // `AttributeError` for `.strip()`.
    let text = match &text_value {
        None => String::new(),
        Some(value) if !crate::config::value_is_truthy(value) => String::new(),
        Some(Value::Str(s)) => electricity_value::pycompat::py_strip(s).to_string(),
        Some(other) => {
            return Err(AdapterError {
                message: format!("'{}' object has no attribute 'strip'", other.type_name()),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::AttributeError,
            });
        }
    };

    let int_if_pyint = |value: Option<&Value>| -> Option<i64> {
        match value {
            Some(v @ (Value::Int(_) | Value::Bool(_))) => {
                electricity_value::pycompat::py_int(v).ok()
            }
            _ => None,
        }
    };
    let tokens_sent = int_if_pyint(dict.get(&Value::Str("prompt_eval_count".to_string())));
    let tokens_received = int_if_pyint(dict.get(&Value::Str("eval_count".to_string())));
    let finish_reason = match dict.get(&Value::Str("done_reason".to_string())) {
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

/// `adapters/ollama.py::OllamaAdapter`.
pub struct OllamaAdapter {
    /// Raw -- see `openai.rs::OpenAiAdapter::base_url`'s own doc
    /// comment. Unlike the curl-based adapters,
    /// `generate()` has no earlier check to run first: Python's own
    /// `self.base_url.rstrip("/")` is the very first thing `generate()`
    /// touches, so a bad value fails immediately there too.
    pub base_url: Value,
}

#[async_trait(?Send)]
impl Adapter for OllamaAdapter {
    fn name(&self) -> &str {
        "ollama"
    }

    async fn generate(
        &self,
        model: &str,
        prompt: &str,
        options: &GenerateOptions,
        call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        let (endpoint, payload) = build_request(model, prompt, options)?;
        // `self.base_url.rstrip("/")` is a plain `Value::Str` by the
        // time this is ever reached for real work below: this closure
        // only runs if [`rstrip_base_url`] already raised.
        let raw_base_url = match &self.base_url {
            Value::Str(s) => s.clone(),
            _ => String::new(),
        };
        let base = rstrip_base_url(&self.base_url)?;
        let url = format!("{base}{endpoint}");
        let headers = request_headers();
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
        let outcome = run_curl(&url, &headers, Some(&body), call.timeout_seconds)
            .await
            .map_err(transport_failure_error)?;
        interpret_outcome(&outcome, model, call.timeout_seconds, &raw_base_url, &url)
    }

    async fn check(&self) -> CheckResult {
        // `redact_host_url`'s own Python counterpart (`curl_support.py`,
        // issue #515) coerces with `str()` first --
        // a non-string `base_url` still produces a `host:` requirement,
        // not a crash, since this is already the branch reporting a
        // problem.
        let redacted = format!("host:{}", redact_host_url(&self.base_url.py_str()));
        let base = match rstrip_base_url(&self.base_url) {
            Ok(base) => base,
            // Unlike `cyberdiner.rs::check`, Python's own `self.base_url.
            // rstrip("/")` here sits *inside* the `try/except Exception`
            // around the whole curl probe (`ollama.py::check`), so the
            // `AttributeError` is swallowed into an ordinary `host:`
            // entry, never `call_check`'s own "check() raised ..." text
            // (confirmed by running `OllamaAdapter(base_url=5).check()`
            // directly, post-F8: `CheckResult(ok=False, missing=['host:5'])`).
            Err(_) => {
                return CheckResult {
                    ok: false,
                    missing: vec![redacted],
                    message: None,
                };
            }
        };
        let url = format!("{base}/api/tags");
        let reachable = crate::transport::head_probe(&url, Duration::from_secs(2)).await;
        if reachable {
            CheckResult {
                ok: true,
                missing: Vec::new(),
                message: None,
            }
        } else {
            CheckResult {
                ok: false,
                missing: vec![redacted],
                message: None,
            }
        }
    }

    fn accepts_images(&self) -> bool {
        true
    }
}

/// `adapters/factory.py::_build_ollama`.
pub fn build_ollama(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(Box::new(OllamaAdapter {
        base_url: cfg_value_or(cfg, "base_url", "http://localhost:11434"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_bytecode::EffectPath;
    use electricity_value::CancellationToken;
    use electricity_vm::adapter::{ChatMessage, ImageInput};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn call_ctx<'a>(path: &'a EffectPath, token: &'a CancellationToken) -> CallContext<'a> {
        CallContext {
            path,
            timeout_seconds: 5,
            token,
        }
    }

    #[test]
    fn build_request_uses_generate_for_a_template_prompt() {
        let (endpoint, payload) = build_request("m", "hello", &GenerateOptions::default()).unwrap();
        assert_eq!(endpoint, "/api/generate");
        assert_eq!(
            payload.get(&Value::Str("prompt".to_string())),
            Some(&Value::Str("hello".to_string()))
        );
        assert!(!payload.contains_key(&Value::Str("options".to_string())));
    }

    #[test]
    fn build_request_uses_chat_for_messages_with_roles_verbatim() {
        let options = GenerateOptions {
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: "sys".to_string(),
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: "tool-out".to_string(),
                },
            ],
            ..GenerateOptions::default()
        };
        let (endpoint, payload) = build_request("m", "hello", &options).unwrap();
        assert_eq!(endpoint, "/api/chat");
        let messages = payload
            .get(&Value::Str("messages".to_string()))
            .unwrap()
            .as_list()
            .unwrap();
        let roles: Vec<&str> = messages
            .iter()
            .map(|m| {
                m.as_dict()
                    .unwrap()
                    .get(&Value::Str("role".to_string()))
                    .unwrap()
                    .as_str()
                    .unwrap()
            })
            .collect();
        assert_eq!(roles, vec!["system", "tool"]);
    }

    #[test]
    fn build_request_attaches_images_to_the_last_user_turn() {
        let options = GenerateOptions {
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "hi".to_string(),
            }],
            images: vec![ImageInput {
                data: Some(vec![1, 2, 3]),
                media_type: Some("image/png".to_string()),
                url: None,
            }],
            ..GenerateOptions::default()
        };
        let (_, payload) = build_request("m", "hi", &options).unwrap();
        let messages = payload
            .get(&Value::Str("messages".to_string()))
            .unwrap()
            .as_list()
            .unwrap();
        let entry = messages[0].as_dict().unwrap();
        assert!(entry.contains_key(&Value::Str("images".to_string())));
    }

    #[test]
    fn build_request_appends_an_empty_user_turn_for_a_system_only_image_conversation() {
        let options = GenerateOptions {
            messages: vec![ChatMessage {
                role: "system".to_string(),
                content: "sys".to_string(),
            }],
            images: vec![ImageInput {
                data: Some(vec![1]),
                media_type: None,
                url: None,
            }],
            ..GenerateOptions::default()
        };
        let (_, payload) = build_request("m", "hi", &options).unwrap();
        let messages = payload
            .get(&Value::Str("messages".to_string()))
            .unwrap()
            .as_list()
            .unwrap();
        assert_eq!(messages.len(), 2);
        let last = messages[1].as_dict().unwrap();
        assert_eq!(
            last.get(&Value::Str("role".to_string())),
            Some(&Value::Str("user".to_string()))
        );
        assert!(last.contains_key(&Value::Str("images".to_string())));
    }

    #[test]
    fn build_request_refuses_a_url_image_before_any_request() {
        let options = GenerateOptions {
            images: vec![ImageInput {
                data: None,
                media_type: None,
                url: Some("https://example.com/a.png".to_string()),
            }],
            ..GenerateOptions::default()
        };
        let err = build_request("m", "hi", &options).unwrap_err();
        assert_eq!(
            err.message,
            "ollama cannot fetch an image URL (https://example.com/a.png); download it and \
             give the asset a local path."
        );
        assert!(!err.retry.retryable);
    }

    #[test]
    fn build_request_orders_model_options_fields_and_seed_last() {
        let mut params = Dict::new();
        dict_insert(&mut params, "num_ctx", Value::from(4096i64));
        dict_insert(&mut params, "format", Value::Str("json".to_string()));
        let options = GenerateOptions {
            temperature: Some(0.2),
            params: Value::Dict(params),
            deterministic: true,
            ..GenerateOptions::default()
        };
        let (_, payload) = build_request("m", "hi", &options).unwrap();
        assert_eq!(
            payload.get(&Value::Str("format".to_string())),
            Some(&Value::Str("json".to_string()))
        );
        let model_options = payload
            .get(&Value::Str("options".to_string()))
            .unwrap()
            .as_dict()
            .unwrap();
        let keys: Vec<String> = model_options.keys().map(|k| k.py_str()).collect();
        assert_eq!(keys, vec!["temperature", "num_ctx", "seed"]);
        assert_eq!(
            model_options.get(&Value::Str("seed".to_string())),
            Some(&Value::from(0i64))
        );
    }

    #[test]
    fn build_request_lets_a_params_seed_win_over_the_deterministic_default() {
        let mut params = Dict::new();
        dict_insert(&mut params, "seed", Value::from(7i64));
        let options = GenerateOptions {
            params: Value::Dict(params),
            deterministic: true,
            ..GenerateOptions::default()
        };
        let (_, payload) = build_request("m", "hi", &options).unwrap();
        let model_options = payload
            .get(&Value::Str("options".to_string()))
            .unwrap()
            .as_dict()
            .unwrap();
        assert_eq!(
            model_options.get(&Value::Str("seed".to_string())),
            Some(&Value::from(7i64))
        );
    }

    #[test]
    fn build_ollama_defaults_the_base_url() {
        let adapter = build_ollama(&Value::None).unwrap();
        assert_eq!(adapter.name(), "ollama");
    }

    #[tokio::test]
    async fn generate_fails_like_python_for_a_non_string_base_url() {
        // `runtime.adapters.ollama.base_url: 5` must
        // not silently fall back to the default URL -- Python keeps the
        // int and then raises `AttributeError: 'int' object has no
        // attribute 'rstrip'` the moment `self.base_url.rstrip("/")`
        // runs, inside `generate()` (ollama has no API key to check
        // first) -- never at build time.
        let adapter = OllamaAdapter {
            base_url: Value::from(5i64),
        };
        let token = CancellationToken::new();
        let path = EffectPath::root();
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
    async fn check_reports_a_non_string_base_url_as_an_ordinary_missing_host() {
        // Confirmed by running `OllamaAdapter(base_url=5).check()`
        // directly post-F8 (`curl_support.redact_host_url`'s own `str()`
        // coercion): Python's own `check()` wraps the probe in a broad
        // `except Exception`, so the `AttributeError` from `self.
        // base_url.rstrip("/")` is swallowed into an ordinary `host:`
        // entry -- `CheckResult(ok=False, missing=['host:5'])`, never
        // `call_check`'s own "check() raised ..." text (unlike
        // `cyberdiner`'s own `check()`, which has no such guard).
        let adapter = OllamaAdapter {
            base_url: Value::from(5i64),
        };
        let result = adapter.check().await;
        assert!(!result.ok);
        assert_eq!(result.missing, vec!["host:5".to_string()]);
        assert_eq!(result.message, None);
    }

    fn spawn_ollama_server_body(body: &'static str) -> std::net::SocketAddr {
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

    fn spawn_ollama_server_status(
        status_line: &'static str,
        body: &'static str,
    ) -> std::net::SocketAddr {
        let response = format!(
            "{status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

    #[tokio::test]
    async fn generate_parses_a_successful_generate_response() {
        let addr = spawn_ollama_server_body(
            "{\"response\": \" hi \", \"prompt_eval_count\": 3, \"eval_count\": 5, \"done_reason\": \"stop\"}",
        );
        let adapter = OllamaAdapter {
            base_url: Value::Str(format!("http://{addr}")),
        };
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let result = adapter
            .generate(
                "m",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap();
        assert_eq!(result.text, "hi");
        assert_eq!(result.tokens_sent, Some(3));
        assert_eq!(result.tokens_received, Some(5));
        assert_eq!(result.finish_reason, Some("stop".to_string()));
    }

    #[tokio::test]
    async fn generate_surfaces_the_model_not_found_hint_on_a_404() {
        let addr = spawn_ollama_server_status(
            "HTTP/1.1 404 Not Found",
            "{\"error\": \"model \\\"m\\\" not found, pull it\"}",
        );
        let adapter = OllamaAdapter {
            base_url: Value::Str(format!("http://{addr}")),
        };
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let err = adapter
            .generate(
                "m",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("Try `ollama pull m` first."));
        assert!(!err.retry.retryable);
        assert_eq!(err.py_class, PyExc::AdapterCallError);
    }

    #[tokio::test]
    async fn generate_raises_runtime_error_for_a_non_json_success_body() {
        let addr = spawn_ollama_server_body("<html>");
        let adapter = OllamaAdapter {
            base_url: Value::Str(format!("http://{addr}")),
        };
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let err = adapter
            .generate(
                "m",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert_eq!(err.message, "curl returned non-JSON response: <html>");
        assert_eq!(err.py_class, PyExc::RuntimeError);
    }

    #[tokio::test]
    async fn generate_raises_type_error_for_an_unserializable_params_value() {
        // Issue #515: `json.dumps(payload)` raises Python's own
        // `TypeError` for a value it can't serialize (bytes, a date, ...),
        // uncaught -- never `RuntimeError`.
        let mut params = Dict::new();
        dict_insert(&mut params, "weird", Value::Bytes(vec![1, 2, 3]));
        let options = GenerateOptions {
            params: Value::Dict(params),
            ..GenerateOptions::default()
        };
        let adapter = OllamaAdapter {
            base_url: Value::Str("http://127.0.0.1:1".to_string()),
        };
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let err = adapter
            .generate("m", "hi", &options, &call_ctx(&path, &token))
            .await
            .unwrap_err();
        // Message taken from running `json.dumps({"weird":
        // bytes([1, 2, 3])})` directly.
        assert_eq!(err.message, "Object of type bytes is not JSON serializable");
        assert_eq!(err.py_class, PyExc::TypeError);
    }

    #[tokio::test]
    async fn check_reports_missing_host_for_a_closed_port() {
        let addr = crate::test_support::closed_port();
        let adapter = OllamaAdapter {
            base_url: Value::Str(format!("http://{addr}")),
        };
        let result = adapter.check().await;
        assert!(!result.ok);
        assert_eq!(result.missing, vec![format!("host:http://{addr}")]);
    }

    #[tokio::test]
    async fn check_masks_userinfo_in_the_missing_host_text() {
        let addr = crate::test_support::closed_port();
        let adapter = OllamaAdapter {
            base_url: Value::Str(format!("http://user:secret@{addr}")),
        };
        let result = adapter.check().await;
        assert!(!result.ok);
        assert_eq!(result.missing, vec![format!("host:http://{addr}")]);
        assert!(!result.missing.iter().any(|m| m.contains("secret")));
    }

    #[tokio::test]
    async fn check_is_reachable_for_any_http_response_including_404() {
        let addr = spawn_ollama_server_status("HTTP/1.1 404 Not Found", "");
        let adapter = OllamaAdapter {
            base_url: Value::Str(format!("http://{addr}")),
        };
        let result = adapter.check().await;
        assert!(result.ok);
        assert!(result.missing.is_empty());
    }
}
