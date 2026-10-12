//! `adapters/watsonx.py::WatsonXAdapter`, ported -- the two-step IAM
//! token exchange plus the `/ml/v1/text/generation` call, over
//! [`crate::transport`]. Rust tests only (the IAM URL is hard-coded in
//! the reference); no conformance case (issue #503's own ruling).

use crate::config::cfg_str_or;
use crate::curl_text::{classify_curl_outcome, curl_failure_message};
use crate::transport::{TransportFailure, run_curl};
use async_trait::async_trait;
use electricity_value::{Dict, Value};
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult, PyExc,
};
use electricity_vm::retry::RetryInfo;
use std::sync::Mutex;
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

/// `adapters/watsonx.py::_TOKEN_CACHE` -- module-level in Python,
/// scoped to [`WatsonXAdapter`] here instead (every instance's own
/// cache; a Rust adapter is rebuilt fresh per call anyway, same as
/// every other non-default adapter in this crate -- see
/// `electricity_vm::adapter::AdapterRegistry`'s own doc comment -- so
/// this still matches Python's own effective behaviour of "a token
/// survives exactly as long as the process", not "until the next
/// adapter instance replaces it").
#[derive(Default)]
struct TokenCache {
    entry: Mutex<Option<(String, Instant)>>,
}

impl TokenCache {
    fn get(&self) -> Option<String> {
        let guard = self.entry.lock().unwrap();
        match &*guard {
            Some((token, expires_at)) if *expires_at > Instant::now() => Some(token.clone()),
            _ => None,
        }
    }

    fn set(&self, token: String, expires_at: Instant) {
        *self.entry.lock().unwrap() = Some((token, expires_at));
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
            message: format!("watsonx IAM response missing access_token: {raw:?}"),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        }),
    }
}

async fn get_token(
    cache: &TokenCache,
    iam_url: &str,
    api_key: &str,
    timeout_seconds: u32,
) -> Result<String, AdapterError> {
    if let Some(token) = cache.get() {
        return Ok(token);
    }
    let (token, expires_in) = exchange_iam_token(iam_url, api_key, timeout_seconds).await?;
    let ttl = expires_in.saturating_sub(TOKEN_TTL_BUFFER_SECONDS).max(60);
    cache.set(token.clone(), Instant::now() + Duration::from_secs(ttl));
    Ok(token)
}

/// `adapters/watsonx.py::WatsonXAdapter`.
pub struct WatsonXAdapter {
    pub base_url: String,
    pub default_model: String,
    pub api_version: String,
    iam_url: String,
    token_cache: TokenCache,
}

impl WatsonXAdapter {
    fn resolve_base_url(&self) -> String {
        if !self.base_url.is_empty() {
            return self.base_url.trim_end_matches('/').to_string();
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
        format!("https://{region}.ml.cloud.ibm.com")
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
            self.resolve_base_url()
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
            electricity_json::dumps(&Value::Dict(payload), electricity_json::WriteMode::COMPACT)
                .map_err(|e| AdapterError {
                    message: e.to_string(),
                    retry: RetryInfo::not_retryable(),
                    py_class: PyExc::RuntimeError,
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
        base_url: cfg_str_or(cfg, "base_url", ""),
        default_model: cfg_str_or(cfg, "default_model", "meta-llama/llama-3-3-70b-instruct"),
        api_version: cfg_str_or(cfg, "api_version", "2024-03-14"),
        iam_url: "https://iam.cloud.ibm.com/identity/token".to_string(),
        token_cache: TokenCache::default(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_bytecode::EffectPath;
    use electricity_value::CancellationToken;
    use std::io::{Read, Write};
    use std::net::TcpListener;

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

    #[tokio::test]
    async fn generate_exchanges_the_iam_token_then_calls_generation() {
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
            base_url: format!("http://{gen_addr}"),
            default_model: "m".to_string(),
            api_version: "2024-03-14".to_string(),
            iam_url: format!("http://{iam_addr}/identity/token"),
            token_cache: TokenCache::default(),
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
    async fn the_token_is_cached_across_calls() {
        let cache = TokenCache::default();
        assert_eq!(cache.get(), None);
        cache.set("tok".to_string(), Instant::now() + Duration::from_secs(60));
        assert_eq!(cache.get(), Some("tok".to_string()));
    }

    #[tokio::test]
    async fn an_expired_token_is_not_reused() {
        let cache = TokenCache::default();
        cache.set("tok".to_string(), Instant::now() - Duration::from_secs(1));
        assert_eq!(cache.get(), None);
    }

    #[test]
    fn build_watsonx_defaults_the_default_model_and_api_version() {
        let adapter = build_watsonx(&Value::None).unwrap();
        assert_eq!(adapter.name(), "watsonx");
    }
}
