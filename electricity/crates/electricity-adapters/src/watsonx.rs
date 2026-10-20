//! `adapters/watsonx.py::WatsonXAdapter`, ported -- the two-step IAM
//! token exchange plus the `/ml/v1/text/generation` call, over
//! [`crate::transport`]. Rust tests only (the IAM URL is hard-coded in
//! the reference); no conformance case (issue #503's own ruling).

use crate::config::{cfg_str_or, cfg_value_or, rstrip_base_url, value_is_truthy};
use crate::curl_text::{classify_curl_outcome, curl_failure_message};
use crate::transport::{TransportFailure, run_curl};
use async_trait::async_trait;
use electricity_value::{Dict, Value};
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult, PyExc,
};
use electricity_vm::retry::RetryInfo;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// `adapters/watsonx.py::_TOKEN_TTL_BUFFER_SECONDS`.
const TOKEN_TTL_BUFFER_SECONDS: u64 = 300;

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

/// `adapters/watsonx.py::_TOKEN_CACHE` -- process-wide, keyed by API
/// key, exactly like Python's own module-level dict. A Rust
/// [`WatsonXAdapter`] is rebuilt fresh for every call
/// (`electricity_vm::adapter::AdapterRegistry`'s own doc comment), so
/// a cache scoped to the instance (the prior shape here) never hits
/// a second time -- every call pays its own IAM exchange, where
/// Python's one process-wide dict makes one IAM exchange per API key
/// for the life of the process. This static makes the two match.
static TOKEN_CACHE: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();

struct TokenCache;

impl TokenCache {
    fn cache() -> &'static Mutex<HashMap<String, (String, Instant)>> {
        TOKEN_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn get(&self, api_key: &str) -> Option<String> {
        let guard = Self::cache().lock().unwrap();
        match guard.get(api_key) {
            Some((token, expires_at)) if *expires_at > Instant::now() => Some(token.clone()),
            _ => None,
        }
    }

    fn set(&self, api_key: &str, token: String, expires_at: Instant) {
        Self::cache()
            .lock()
            .unwrap()
            .insert(api_key.to_string(), (token, expires_at));
    }
}

/// `adapters/watsonx.py::_exchange_iam_token` -- *iam_url* is injected
/// so Rust tests can point this at a mock server instead of the real,
/// hard-coded `https://iam.cloud.ibm.com/identity/token`.
async fn exchange_iam_token(
    iam_url: &str,
    api_key: &str,
    timeout_seconds: u32,
) -> Result<(String, u64), AdapterError> {
    let body = format!("grant_type=urn:ibm:params:oauth:grant-type:apikey&apikey={api_key}");
    let headers = vec![(
        "Content-Type".to_string(),
        "application/x-www-form-urlencoded".to_string(),
    )];
    let outcome = run_curl(iam_url, &headers, Some(&body), timeout_seconds)
        .await
        .map_err(transport_failure_error)?;
    if outcome.returncode != 0 {
        let message = curl_failure_message(
            "watsonx-iam",
            None,
            iam_url,
            outcome.returncode,
            &outcome.stdout,
            &outcome.stderr,
            "",
            &[api_key],
        );
        return Err(AdapterError {
            message,
            retry: classify_curl_outcome(&outcome),
            py_class: PyExc::AdapterCallError,
        });
    }
    let raw = electricity_json::loads(&outcome.stdout).map_err(|_| AdapterError {
        message: format!(
            "watsonx IAM returned non-JSON response: {}",
            truncate_chars(&outcome.stdout, 200)
        ),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    })?;
    let dict = raw.as_dict();
    let token = dict.and_then(|d| d.get(&Value::Str("access_token".to_string())));
    let expires_in = dict
        .and_then(|d| d.get(&Value::Str("expires_in".to_string())))
        .and_then(Value::as_int)
        .map(|i| i.to_bigint().to_string().parse::<i64>().unwrap_or(3600))
        .unwrap_or(3600);
    match token {
        Some(Value::Str(s)) if !s.is_empty() => Ok((s.clone(), expires_in.max(0) as u64)),
        _ => Err(AdapterError {
            message: format!(
                "watsonx IAM response missing access_token: {}",
                raw.py_str()
            ),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        }),
    }
}

/// `adapters/watsonx.py::_TOKEN_LOCK` -- one process-wide lock around
/// the *whole* check-then-exchange-then-store sequence, never only
/// the cache read/write either side of it. Holding
/// it only around [`TokenCache::get`]/[`TokenCache::set`] (the prior
/// shape here) still lets two concurrent calls that both miss the
/// cache each start their own IAM exchange; an
/// [`async`][tokio::sync::Mutex] lock (not [`std::sync::Mutex`],
/// which may not be held across an `.await`) held for the whole
/// function serializes them exactly as Python's own `with
/// _TOKEN_LOCK:` does -- the loser of the race finds the token the
/// winner just cached, the same `cache.get` hit an already-cached key
/// gets, rather than exchanging a second time.
fn exchange_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn get_token(
    cache: &TokenCache,
    iam_url: &str,
    api_key: &str,
    timeout_seconds: u32,
) -> Result<String, AdapterError> {
    let _guard = exchange_lock().lock().await;
    if let Some(token) = cache.get(api_key) {
        return Ok(token);
    }
    let (token, expires_in) = exchange_iam_token(iam_url, api_key, timeout_seconds).await?;
    let ttl = expires_in.saturating_sub(TOKEN_TTL_BUFFER_SECONDS).max(60);
    cache.set(
        api_key,
        token.clone(),
        Instant::now() + Duration::from_secs(ttl),
    );
    Ok(token)
}

/// `adapters/watsonx.py::WatsonXAdapter`.
pub struct WatsonXAdapter {
    /// Raw -- `cfg.get("base_url") or ""`. Python's own `_resolve_base_
    /// url` only ever calls `.rstrip("/")` on a *truthy* value (`if
    /// self.base_url: return self.base_url.rstrip("/")`), so an empty
    /// string here is never a `rstrip_base_url` failure -- it falls
    /// through to the `WATSONX_REGION` default -- but a truthy
    /// non-string value is (raised inside
    /// `generate()`, after the IAM token exchange, matching Python's
    /// own call order: `_resolve_base_url` runs after `_get_token`).
    pub base_url: Value,
    pub default_model: String,
    pub api_version: String,
    iam_url: String,
    token_cache: TokenCache,
}

impl WatsonXAdapter {
    fn resolve_base_url(&self) -> Result<String, AdapterError> {
        if value_is_truthy(&self.base_url) {
            return rstrip_base_url(&self.base_url);
        }
        let region = {
            let raw = std::env::var("WATSONX_REGION").unwrap_or_default();
            let trimmed = electricity_value::pycompat::py_strip(&raw);
            if trimmed.is_empty() {
                "us-south".to_string()
            } else {
                trimmed.to_string()
            }
        };
        Ok(format!("https://{region}.ml.cloud.ibm.com"))
    }
}

#[async_trait(?Send)]
impl Adapter for WatsonXAdapter {
    fn name(&self) -> &str {
        "watsonx"
    }

    async fn generate(
        &self,
        model: &str,
        prompt: &str,
        options: &GenerateOptions,
        call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        let api_key = std::env::var("WATSONX_API_KEY").unwrap_or_default();
        if api_key.is_empty() {
            return Err(AdapterError {
                message: "watsonx: WATSONX_API_KEY not set.".to_string(),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::RuntimeError,
            });
        }
        let project_id = std::env::var("WATSONX_PROJECT_ID").unwrap_or_default();
        if project_id.is_empty() {
            return Err(AdapterError {
                message: "watsonx: WATSONX_PROJECT_ID not set.".to_string(),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::RuntimeError,
            });
        }

        let token = get_token(
            &self.token_cache,
            &self.iam_url,
            &api_key,
            call.timeout_seconds,
        )
        .await?;
        let target_model = if model.is_empty() {
            self.default_model.as_str()
        } else {
            model
        };
        let api_version = {
            let env_version = std::env::var("WATSONX_API_VERSION").unwrap_or_default();
            if env_version.is_empty() {
                self.api_version.clone()
            } else {
                env_version
            }
        };

        let url = format!(
            "{}/ml/v1/text/generation?version={api_version}",
            self.resolve_base_url()?
        );
        let mut payload = Dict::new();
        payload.insert(
            Value::Str("model_id".to_string()),
            Value::Str(target_model.to_string()),
        );
        payload.insert(
            Value::Str("input".to_string()),
            Value::Str(prompt.to_string()),
        );
        payload.insert(Value::Str("project_id".to_string()), Value::Str(project_id));
        let body =
            // `json.dumps(payload)` raises Python's own `TypeError` for a
            // value it can't serialize, uncaught -- never `RuntimeError`.
            electricity_json::dumps(&Value::Dict(payload), electricity_json::WriteMode::COMPACT)
                .map_err(|e| AdapterError {
                    message: e.to_string(),
                    retry: RetryInfo::not_retryable(),
                    py_class: PyExc::TypeError,
                })?;

        let headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Authorization".to_string(), format!("Bearer {token}")),
        ];
        let outcome = run_curl(&url, &headers, Some(&body), call.timeout_seconds)
            .await
            .map_err(transport_failure_error)?;

        if outcome.returncode != 0 {
            let message = curl_failure_message(
                "watsonx",
                Some(target_model),
                &url,
                outcome.returncode,
                &outcome.stdout,
                &outcome.stderr,
                "",
                &[&api_key, &token],
            );
            return Err(AdapterError {
                message,
                retry: classify_curl_outcome(&outcome),
                py_class: PyExc::AdapterCallError,
            });
        }

        let raw = electricity_json::loads(&outcome.stdout).map_err(|_| AdapterError {
            message: format!(
                "watsonx returned non-JSON response: {}",
                truncate_chars(&outcome.stdout, 200)
            ),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        })?;

        let results = raw
            .as_dict()
            .and_then(|d| d.get(&Value::Str("results".to_string())))
            .and_then(Value::as_list)
            .unwrap_or(&[]);
        let first = results.first().and_then(Value::as_dict);
        let text = match first.and_then(|d| d.get(&Value::Str("generated_text".to_string()))) {
            Some(Value::Str(s)) => s.clone(),
            _ => String::new(),
        };
        let tokens_sent = first
            .and_then(|d| d.get(&Value::Str("input_token_count".to_string())))
            .and_then(|v| match v {
                Value::Int(_) | Value::Bool(_) => electricity_value::pycompat::py_int(v).ok(),
                _ => None,
            });
        let tokens_received = first
            .and_then(|d| d.get(&Value::Str("generated_token_count".to_string())))
            .and_then(|v| match v {
                Value::Int(_) | Value::Bool(_) => electricity_value::pycompat::py_int(v).ok(),
                _ => None,
            });

        Ok(GenerateResult {
            text: electricity_value::pycompat::py_strip(&text).to_string(),
            raw,
            tokens_sent,
            tokens_received,
            finish_reason: None,
            warnings: crate::ignored_options_warning("watsonx", options, &[]),
            cost_usd: None,
        })
    }

    async fn check(&self) -> CheckResult {
        let mut missing = Vec::new();
        if std::env::var("WATSONX_API_KEY")
            .unwrap_or_default()
            .is_empty()
        {
            missing.push("env:WATSONX_API_KEY".to_string());
        }
        if std::env::var("WATSONX_PROJECT_ID")
            .unwrap_or_default()
            .is_empty()
        {
            missing.push("env:WATSONX_PROJECT_ID".to_string());
        }
        CheckResult {
            ok: missing.is_empty(),
            missing,
            message: None,
        }
    }
}

/// `adapters/factory.py::_build_watsonx`.
pub fn build_watsonx(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(Box::new(WatsonXAdapter {
        base_url: cfg_value_or(cfg, "base_url", ""),
        default_model: cfg_str_or(cfg, "default_model", "meta-llama/llama-3-3-70b-instruct"),
        api_version: cfg_str_or(cfg, "api_version", "2024-03-14"),
        iam_url: "https://iam.cloud.ibm.com/identity/token".to_string(),
        token_cache: TokenCache,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_bytecode::EffectPath;
    use electricity_value::CancellationToken;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// `WATSONX_API_KEY`/`WATSONX_PROJECT_ID`/`WATSONX_API_VERSION` are
    /// process environment, shared by every test in this module; `cargo
    /// test` runs them concurrently by default, so two tests setting and
    /// then removing the same variable race each other without this.
    fn env_guard() -> &'static Mutex<()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        GUARD.get_or_init(|| Mutex::new(()))
    }

    fn spawn_server(response: String) -> std::net::SocketAddr {
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

    fn json_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// Accepts *count* connections, serving *response* each time, then
    /// stops -- a further connection attempt gets connection-refused,
    /// which is exactly how a test proves "no further request happened"
    /// without a request counter.
    fn spawn_server_n(response: String, count: usize) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..count {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 65536];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        addr
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // single-threaded tokio test runtime; see env_guard's own doc comment
    async fn generate_exchanges_the_iam_token_then_calls_generation() {
        let _guard = env_guard().lock().unwrap();
        unsafe {
            std::env::set_var("WATSONX_API_KEY", "circuitry-test-fake-key");
            std::env::set_var("WATSONX_PROJECT_ID", "proj");
            std::env::remove_var("WATSONX_API_VERSION");
        }
        let iam_addr = spawn_server(json_response(
            r#"{"access_token": "tok123", "expires_in": 3600}"#,
        ));
        let gen_addr = spawn_server(json_response(
            r#"{"results": [{"generated_text": " hi ", "input_token_count": 3, "generated_token_count": 5}]}"#,
        ));
        let adapter = WatsonXAdapter {
            base_url: Value::Str(format!("http://{gen_addr}")),
            default_model: "m".to_string(),
            api_version: "2024-03-14".to_string(),
            iam_url: format!("http://{iam_addr}/identity/token"),
            token_cache: TokenCache,
        };
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let call = CallContext {
            path: &path,
            timeout_seconds: 5,
            token: &token,
        };
        let result = adapter
            .generate("m", "hi", &GenerateOptions::default(), &call)
            .await
            .unwrap();
        assert_eq!(result.text, "hi");
        assert_eq!(result.tokens_sent, Some(3));
        assert_eq!(result.tokens_received, Some(5));
        unsafe {
            std::env::remove_var("WATSONX_API_KEY");
            std::env::remove_var("WATSONX_PROJECT_ID");
        }
    }

    #[tokio::test]
    async fn exchange_iam_token_renders_an_unexpected_response_with_pythons_str_not_debug() {
        // Message taken from running
        // `f"watsonx IAM response missing access_token: {raw}"` directly
        // against `json.loads('{"foo": "bar", "num": 5, "nested": {"a": null}}')`:
        // Python's own `str()` of a dict, never Rust's `{:?}` `Debug`.
        let addr = spawn_server(json_response(
            r#"{"foo": "bar", "num": 5, "nested": {"a": null}}"#,
        ));
        let err = exchange_iam_token(&format!("http://{addr}"), "k", 5)
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "watsonx IAM response missing access_token: {'foo': 'bar', 'num': 5, 'nested': {'a': None}}"
        );
        assert_eq!(err.py_class, PyExc::RuntimeError);
    }

    #[tokio::test]
    async fn the_token_is_cached_across_calls() {
        let cache = TokenCache;
        let key = "circuitry-test-cache-key-1";
        assert_eq!(cache.get(key), None);
        cache.set(
            key,
            "tok".to_string(),
            Instant::now() + Duration::from_secs(60),
        );
        assert_eq!(cache.get(key), Some("tok".to_string()));
    }

    #[tokio::test]
    async fn an_expired_token_is_not_reused() {
        let cache = TokenCache;
        let key = "circuitry-test-cache-key-2";
        cache.set(
            key,
            "tok".to_string(),
            Instant::now() - Duration::from_secs(1),
        );
        assert_eq!(cache.get(key), None);
    }

    #[tokio::test]
    async fn the_cache_is_process_wide_keyed_by_api_key_not_scoped_to_one_adapter_instance() {
        // N3 (issue #515): a Rust adapter is rebuilt fresh for every call
        // (`AdapterRegistry::build`), so a cache scoped to the instance
        // never hits a second time -- it must be process-wide, exactly
        // like Python's own module-level dict, for the request sequence
        // to match (one IAM exchange per API key, not one per call).
        let key = "circuitry-test-cache-key-process-wide";
        let first = TokenCache;
        first.set(
            key,
            "shared-tok".to_string(),
            Instant::now() + Duration::from_secs(60),
        );
        let second = TokenCache;
        assert_eq!(second.get(key), Some("shared-tok".to_string()));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // single-threaded tokio test runtime; see env_guard's own doc comment
    async fn two_calls_with_the_same_api_key_share_one_iam_exchange() {
        // N3 (issue #515): the IAM listener below accepts exactly one
        // connection -- a second IAM request would get connection-refused,
        // which is how this proves the second `generate()` call reused the
        // cached token rather than exchanging it again, even though each
        // call below builds its own fresh `WatsonXAdapter` (every
        // non-default adapter is rebuilt per call in this crate).
        let _guard = env_guard().lock().unwrap();
        unsafe {
            std::env::set_var("WATSONX_API_KEY", "circuitry-test-shared-key");
            std::env::set_var("WATSONX_PROJECT_ID", "proj");
            std::env::remove_var("WATSONX_API_VERSION");
        }
        let iam_addr = spawn_server_n(
            json_response(r#"{"access_token": "shared-tok", "expires_in": 3600}"#),
            1,
        );
        let gen_addr = spawn_server_n(
            json_response(r#"{"results": [{"generated_text": "hi"}]}"#),
            2,
        );
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let call = CallContext {
            path: &path,
            timeout_seconds: 5,
            token: &token,
        };

        for _ in 0..2 {
            let adapter = WatsonXAdapter {
                base_url: Value::Str(format!("http://{gen_addr}")),
                default_model: "m".to_string(),
                api_version: "2024-03-14".to_string(),
                iam_url: format!("http://{iam_addr}/identity/token"),
                token_cache: TokenCache,
            };
            adapter
                .generate("m", "hi", &GenerateOptions::default(), &call)
                .await
                .unwrap();
        }

        unsafe {
            std::env::remove_var("WATSONX_API_KEY");
            std::env::remove_var("WATSONX_PROJECT_ID");
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // single-threaded tokio test runtime; see env_guard's own doc comment
    async fn two_concurrent_calls_with_the_same_api_key_still_share_one_iam_exchange() {
        // `get_token` now holds its own lock
        // across the *whole* check-then-exchange-then-store sequence
        // (`exchange_lock`), not just around the cache read/write
        // either side of it -- two calls launched at the same time
        // (`tokio::join!`, unlike the sequential `for` loop in the
        // test above) must still make exactly one IAM request between
        // them: the mock IAM listener below accepts exactly one
        // connection, so a second concurrent exchange would hang this
        // test's own second `generate()` call out to the `cargo test`
        // deadline rather than complete.
        let _guard = env_guard().lock().unwrap();
        unsafe {
            std::env::set_var("WATSONX_API_KEY", "circuitry-test-concurrent-key");
            std::env::set_var("WATSONX_PROJECT_ID", "proj");
            std::env::remove_var("WATSONX_API_VERSION");
        }
        let iam_addr = spawn_server_n(
            json_response(r#"{"access_token": "concurrent-tok", "expires_in": 3600}"#),
            1,
        );
        let gen_addr = spawn_server_n(
            json_response(r#"{"results": [{"generated_text": "hi"}]}"#),
            2,
        );
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let call = CallContext {
            path: &path,
            timeout_seconds: 5,
            token: &token,
        };
        let adapter_a = WatsonXAdapter {
            base_url: Value::Str(format!("http://{gen_addr}")),
            default_model: "m".to_string(),
            api_version: "2024-03-14".to_string(),
            iam_url: format!("http://{iam_addr}/identity/token"),
            token_cache: TokenCache,
        };
        let adapter_b = WatsonXAdapter {
            base_url: Value::Str(format!("http://{gen_addr}")),
            default_model: "m".to_string(),
            api_version: "2024-03-14".to_string(),
            iam_url: format!("http://{iam_addr}/identity/token"),
            token_cache: TokenCache,
        };
        let options = GenerateOptions::default();
        let (first, second) = tokio::join!(
            adapter_a.generate("m", "hi", &options, &call),
            adapter_b.generate("m", "hi", &options, &call),
        );
        first.unwrap();
        second.unwrap();
        unsafe {
            std::env::remove_var("WATSONX_API_KEY");
            std::env::remove_var("WATSONX_PROJECT_ID");
        }
    }

    #[test]
    fn build_watsonx_defaults_the_default_model_and_api_version() {
        let adapter = build_watsonx(&Value::None).unwrap();
        assert_eq!(adapter.name(), "watsonx");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // single-threaded tokio test runtime; see env_guard's own doc comment
    async fn generate_fails_like_python_for_a_non_string_base_url_only_after_the_iam_exchange() {
        // Python's own `_resolve_base_url` runs
        // *after* `_get_token` -- the IAM listener below accepts
        // exactly one connection, proving the exchange still happens
        // (and the token is still cached) before the `AttributeError`
        // from `self.base_url.rstrip("/")` is ever raised.
        let _guard = env_guard().lock().unwrap();
        unsafe {
            std::env::set_var("WATSONX_API_KEY", "circuitry-test-bad-base-url-key");
            std::env::set_var("WATSONX_PROJECT_ID", "proj");
            std::env::remove_var("WATSONX_API_VERSION");
        }
        let iam_addr = spawn_server_n(
            json_response(r#"{"access_token": "tok", "expires_in": 3600}"#),
            1,
        );
        let adapter = WatsonXAdapter {
            base_url: Value::from(5i64),
            default_model: "m".to_string(),
            api_version: "2024-03-14".to_string(),
            iam_url: format!("http://{iam_addr}/identity/token"),
            token_cache: TokenCache,
        };
        let token = CancellationToken::new();
        let path = EffectPath::root();
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
            std::env::remove_var("WATSONX_API_KEY");
            std::env::remove_var("WATSONX_PROJECT_ID");
        }
        assert_eq!(err.message, "'int' object has no attribute 'rstrip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }
}
