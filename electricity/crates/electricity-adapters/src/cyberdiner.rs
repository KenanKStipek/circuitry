//! `adapters/cyberdiner.py::CyberdinerAdapter`, ported -- a job-queue
//! LLM broker: submit a job, poll it until a terminal state, block
//! until the job completes, then return the text. Python sends its
//! own requests with `urllib.request`, never `curl` (the only adapter
//! in this crate that doesn't shell out to curl's own convention) --
//! this module reproduces urllib's own request shape over
//! [`electricity_tools::http_client`] directly, the same transport
//! primitive the `http` tool (`electricity_tools::http`) uses, rather
//! than [`crate::transport::run_curl`]'s curl-flavored header set and
//! exit-code mapping.
//!
//! Wire format -- ground truth is `apps/expo/src/models/job.rs` in
//! KenanKStipek/CyberDiner: `POST {expo_url}/beta/jobs` with
//! `{prompt, tierName}` and a bearer token returns a `jobId`; `GET
//! {expo_url}/beta/jobs/{jobId}` is polled until the job leaves
//! `pending`/`assigned`/`running` and reaches a terminal state
//! (`complete`/`completed`/`failed`/`cancelled`/`timedOut`).
//!
//! Known, disclosed narrowing from the Python reference (none of
//! these are exercised by this crate's own conformance cases, which
//! only ever talk to a mock server on a non-default port with no
//! proxy configured):
//! - a 3xx response is followed only when its own `Location` header
//!   is an absolute `http(s)://` URL, capped at a few hops -- a
//!   relative `Location` is treated as the final response instead of
//!   joined against the current URL (urllib's own `urljoin`, which
//!   this module doesn't reproduce);
//! - `poll_interval_ms`/`timeout_seconds` read from config fall back
//!   to their own default on anything but a plain integer, rather
//!   than reproducing Python's own `int(x)` coercion (and its own
//!   exact failure text for a value that isn't one);
//! - a body-phase failure (a dropped connection mid-read) on an
//!   otherwise-successful response is reported as this adapter's own
//!   `RuntimeError`, not Python's own unwrapped `http.client`
//!   exception class, which `_request` never catches either;
//! - `max_in_flight`'s own per-instance backpressure semaphore
//!   (`adapters/cyberdiner.py`'s own module doc comment, circuitry#77)
//!   isn't ported at all -- a Rust adapter is rebuilt fresh for every
//!   call (`electricity_vm::adapter::AdapterRegistry`'s own doc
//!   comment), so a semaphore scoped to one instance would never
//!   coordinate across calls the way Python's long-lived instance
//!   does (the same class of bug `watsonx.rs`'s own IAM token cache
//!   had before issue #515 made it process-wide); giving this one a
//!   process-wide, config-keyed equivalent is its own follow-up.

use crate::config::{cfg_str_or, cfg_str_or_rstrip_target, value_is_truthy};
use crate::transport::{
    ParsedUrl, TransportFailure, default_port, host_header_value, parse_url, redact_url_for_error,
    resolve_curl_proxy,
};
use electricity_json::WriteMode;
use electricity_tools::http_client::{
    self, ConnectTarget, RawRequest, RawResponse, Scheme, SendError, TimeoutModel, redirect_policy,
};
use electricity_value::{Dict, Value};
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult, PyExc,
};
use electricity_vm::retry::RetryInfo;
use std::time::Duration;

/// `adapters/cyberdiner.py::_SUCCESS_STATUSES`.
const SUCCESS_STATUSES: [&str; 2] = ["complete", "completed"];
/// `adapters/cyberdiner.py::_TIMED_OUT_STATUSES`.
const TIMED_OUT_STATUSES: [&str; 1] = ["timedout"];
/// `adapters/cyberdiner.py::SEED_TIERS`.
const SEED_TIERS: [&str; 7] = [
    "cheap",
    "fast-cheap",
    "fast",
    "good-cheap",
    "good",
    "good-fast",
    "alpha",
];
/// A redirect loop guard -- Python's own urllib follows an unbounded
/// chain (bounded only by `HTTPRedirectHandler.max_redirections`,
/// which this module doesn't otherwise reproduce); any real CyberDiner
/// deployment that redirects at all does so once, so this is purely a
/// safety cap against a misbehaving mock, never a value a real
/// response is expected to hit.
const MAX_REDIRECTS: u32 = 5;

fn is_terminal(status: &str) -> bool {
    SUCCESS_STATUSES.contains(&status)
        || TIMED_OUT_STATUSES.contains(&status)
        || status == "failed"
        || status == "cancelled"
}

/// `adapters/cyberdiner.py::_normalize_status` -- lowercase,
/// alphanumerics only; a falsy *status* (absent, `None`, `""`, `0`,
/// `False`) normalizes to `""` the same way `str(status or "")` does.
fn normalize_status(status: &Value) -> String {
    let base = if value_is_truthy(status) {
        status.py_str()
    } else {
        String::new()
    };
    base.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// `adapters/cyberdiner.py::_as_int` -- `int | float` only (a `bool`,
/// despite being a Python `int` subclass, is explicitly excluded);
/// anything else, including a numeric *string*, is `None`.
fn as_int(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(v @ (Value::Int(_) | Value::Float(_))) => electricity_value::pycompat::py_int(v).ok(),
        _ => None,
    }
}

/// `adapters/cyberdiner.py::_error_detail` -- trailing `(errorCode=…,
/// errorMessage=…)` for whichever of the two keys expo actually sent
/// (both reprs, Python's own `{value!r}`), `""` when neither is
/// truthy.
fn error_detail(job: &Dict) -> String {
    let mut parts = Vec::new();
    for key in ["errorCode", "errorMessage"] {
        if let Some(value) = job.get(&Value::Str(key.to_string())) {
            if value_is_truthy(value) {
                parts.push(format!("{key}={}", value.py_repr()));
            }
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

/// `adapters/cyberdiner.py::_resolve_tier`.
fn resolve_tier(
    model: &str,
    default_tier: &str,
    valid_tiers: &[String],
) -> Result<String, AdapterError> {
    let trimmed_model = electricity_value::pycompat::py_strip(model);
    let tier = if !trimmed_model.is_empty() {
        trimmed_model.to_string()
    } else {
        electricity_value::pycompat::py_strip(default_tier).to_string()
    };
    if tier.is_empty() {
        return Err(AdapterError {
            message: "cyberdiner: no tier resolved. Pin `model:` in the orchestration or set \
                      runtime.adapters.cyberdiner.default_tier."
                .to_string(),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::ValueError,
        });
    }
    if !valid_tiers.is_empty() && !valid_tiers.iter().any(|t| t == &tier) {
        let valid = valid_tiers.join(", ");
        return Err(AdapterError {
            message: format!(
                "cyberdiner: unknown tier {}. Configured runtime.adapters.cyberdiner.valid_tiers: {valid}.",
                Value::Str(tier).py_repr()
            ),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::ValueError,
        });
    }
    Ok(tier)
}

/// `adapters/cyberdiner.py::_unwrap` -- peels expo's `ApiEnvelope` off
/// a job response; a response without a top-level `data` *object*
/// means this isn't talking to the API it thinks it is.
fn unwrap_envelope(body: &Value, url: &str) -> Result<Dict, AdapterError> {
    match body
        .as_dict()
        .and_then(|d| d.get(&Value::Str("data".to_string())))
    {
        Some(Value::Dict(data)) => Ok(data.clone()),
        _ => {
            let rendered = electricity_json::dumps(body, WriteMode::COMPACT).unwrap_or_default();
            Err(AdapterError {
                message: format!(
                    "cyberdiner: response from {url} is not a CyberDiner job envelope — expected \
                     a top-level `data` object (expo wraps every job in ApiEnvelope). Got: {}",
                    truncate_chars(&rendered, 200)
                ),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::RuntimeError,
            })
        }
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((byte_index, _)) => &text[..byte_index],
        None => text,
    }
}

fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// urllib's own default `addheaders` entry plus the handful every
/// urllib request carries automatically (`AbstractHTTPHandler.
/// do_request_`): `Host` (verbatim netloc, userinfo stripped),
/// `Accept-encoding: identity`, `Content-length` when *body* is
/// present, then the caller's own explicit headers (`Authorization`,
/// `Content-Type` when sending one), `User-agent`, and finally
/// `Connection: close`, which `do_open` always sets last, overriding
/// anything the caller supplied under that name.
fn urllib_headers(
    host_header: &str,
    body: Option<&str>,
    extra: &[(String, String)],
) -> Vec<(String, String)> {
    let mut headers = vec![
        ("Host".to_string(), host_header.to_string()),
        ("Accept-encoding".to_string(), "identity".to_string()),
    ];
    if let Some(body) = body {
        headers.push(("Content-length".to_string(), body.len().to_string()));
    }
    for (name, value) in extra {
        headers.push((name.clone(), value.clone()));
    }
    headers.push((
        "User-agent".to_string(),
        format!("electricity/{}", env!("CARGO_PKG_VERSION")),
    ));
    headers.push(("Connection".to_string(), "close".to_string()));
    headers
}

fn connect_target_for(parsed: &ParsedUrl, port: u16) -> Result<ConnectTarget, TransportFailure> {
    let scheme = if parsed.scheme == "https" {
        Scheme::Https
    } else {
        Scheme::Http
    };
    let proxy = resolve_curl_proxy(&parsed.scheme, &parsed.host)?;
    Ok(ConnectTarget {
        scheme,
        host: parsed.host.clone(),
        port,
        proxy,
    })
}

/// A connection-level failure (never reaching a status line) --
/// `adapters/cyberdiner.py::_request`'s own `except urllib.error.
/// URLError as exc: raise RuntimeError(f"cyberdiner: request to {url}
/// failed: {exc.reason}")`. [`SendError`]'s own `Display` is already
/// CPython's own `OSError`/`socket.timeout`/`http.client` exception
/// text by construction (`electricity_tools::http_client::SendError`'s
/// own doc comment) -- exactly what `exc.reason`'s own `str()` would
/// show, so no re-derivation is needed here.
fn connection_error(url: &str, err: SendError) -> AdapterError {
    AdapterError {
        message: format!("cyberdiner: request to {url} failed: {err}"),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    }
}

fn transport_failure_error(url: &str, failure: TransportFailure) -> AdapterError {
    let message = match failure {
        TransportFailure::Validation(_) => {
            format!(
                "cyberdiner: request to {} failed: unsupported URL",
                redact_url_for_error(url)
            )
        }
        TransportFailure::InvalidUtf8(message) => message,
    };
    AdapterError {
        message,
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    }
}

/// `adapters/cyberdiner.py::_request` -- *payload* is serialized to
/// JSON and sent as the body of a `POST`/`PATCH`-shaped request;
/// `None` sends no body at all (a `GET`). Follows an absolute-URL 3xx
/// per [`redirect_policy`] (urllib's own decision table, genuinely
/// shared with the `http` tool), up to [`MAX_REDIRECTS`] hops.
async fn request_json(
    method: &str,
    url: &str,
    token: &str,
    payload: Option<&Value>,
    timeout: Duration,
) -> Result<Dict, AdapterError> {
    let body = match payload {
        Some(value) => Some(
            electricity_json::dumps(value, WriteMode::COMPACT).map_err(|e| AdapterError {
                message: e.to_string(),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::TypeError,
            })?,
        ),
        None => None,
    };

    let client = http_client::build_client().map_err(|message| AdapterError {
        message,
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    })?;

    let mut current_method = method.to_string();
    let mut current_url = url.to_string();
    let mut current_body = body;

    for _ in 0..=MAX_REDIRECTS {
        let parsed =
            parse_url(&current_url).map_err(|e| transport_failure_error(&current_url, e))?;
        let port = parsed.port.unwrap_or_else(|| default_port(&parsed.scheme));
        let connect_target = connect_target_for(&parsed, port)
            .map_err(|e| transport_failure_error(&current_url, e))?;
        let host_header = host_header_value(&parsed.host, port, &parsed.scheme);

        let mut extra_headers = vec![("Authorization".to_string(), format!("Bearer {token}"))];
        if current_body.is_some() {
            extra_headers.push(("Content-type".to_string(), "application/json".to_string()));
        }
        let headers = urllib_headers(&host_header, current_body.as_deref(), &extra_headers);

        let raw_request = RawRequest {
            method: current_method.clone(),
            target: parsed.target.clone(),
            headers,
            body: current_body.as_ref().map(|b| b.as_bytes().to_vec()),
        };

        let response = http_client::send(
            &client,
            &connect_target,
            &raw_request,
            TimeoutModel::Total(timeout),
        )
        .await
        .map_err(|err| connection_error(&current_url, err))?;

        if (300..=399).contains(&response.status) {
            if let Some(redirect) = redirect_policy(&current_method, response.status) {
                if let Some(location) = find_header(&response.headers, "location") {
                    if location.starts_with("http://") || location.starts_with("https://") {
                        current_url = location.to_string();
                        current_method = redirect.method;
                        if redirect.drop_body {
                            current_body = None;
                        }
                        continue;
                    }
                }
            }
        }

        return interpret_response(&current_url, response);
    }

    Err(AdapterError {
        message: format!("cyberdiner: request to {url} failed: too many redirects"),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    })
}

fn interpret_response(url: &str, response: RawResponse) -> Result<Dict, AdapterError> {
    // A body-phase failure after the status line arrived -- never
    // Python's own unwrapped exception class (`_request` has no
    // handler for one at all), but not tested by any conformance case
    // either; see this module's own top-of-file doc comment.
    if response.body_error.is_some() {
        return Err(AdapterError {
            message: format!("cyberdiner: response from {url} ended before it was complete."),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        });
    }
    let text = String::from_utf8_lossy(&response.body).into_owned();
    if response.status >= 400 {
        let detail = truncate_chars(text.trim(), 2000);
        let message = if detail.is_empty() {
            format!(
                "cyberdiner: HTTP {} from {url}: {}",
                response.status, response.reason
            )
        } else {
            format!(
                "cyberdiner: HTTP {} from {url}: {} — {detail}",
                response.status, response.reason
            )
        };
        return Err(AdapterError {
            message,
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        });
    }
    if text.is_empty() {
        return Ok(Dict::new());
    }
    match electricity_json::loads(&text) {
        Ok(Value::Dict(ref dict)) => Ok(dict.clone()),
        _ => Err(AdapterError {
            message: format!(
                "cyberdiner: non-JSON response from {url}: {}",
                truncate_chars(&text, 200)
            ),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        }),
    }
}

fn cancelled_error(job_id: &Value) -> AdapterError {
    AdapterError {
        message: format!(
            "cyberdiner: cancelled while waiting for job {} to complete.",
            job_id.py_str()
        ),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    }
}

/// `adapters/cyberdiner.py::CyberdinerAdapter`.
pub struct CyberdinerAdapter {
    pub expo_url: String,
    pub token: String,
    pub default_tier: String,
    pub valid_tiers: Vec<String>,
    pub poll_interval_ms: i64,
    pub timeout_seconds: u32,
}

impl CyberdinerAdapter {
    async fn generate_inner(
        &self,
        model: &str,
        prompt: &str,
        timeout_seconds: u32,
        call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        if self.expo_url.is_empty() {
            return Err(AdapterError {
                message: "cyberdiner: expo_url not configured. Set \
                          runtime.adapters.cyberdiner.expo_url to your CyberDiner expo \
                          deployment's root URL."
                    .to_string(),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::RuntimeError,
            });
        }
        if self.token.is_empty() {
            return Err(AdapterError {
                message: "cyberdiner: token not configured. Set \
                          runtime.adapters.cyberdiner.token to a CyberDiner API key (ck_...), \
                          minted via expo's api_keys routes or the web app."
                    .to_string(),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::RuntimeError,
            });
        }

        let tier = resolve_tier(model, &self.default_tier, &self.valid_tiers)?;
        let base = self.expo_url.trim_end_matches('/');
        let req_timeout = Duration::from_secs_f64(
            (self.timeout_seconds as f64)
                .min(timeout_seconds as f64)
                .max(0.001),
        );
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(u64::from(timeout_seconds));

        let submit_url = format!("{base}/beta/jobs");
        let mut payload = Dict::new();
        payload.insert(
            Value::Str("prompt".to_string()),
            Value::Str(prompt.to_string()),
        );
        payload.insert(Value::Str("tierName".to_string()), Value::Str(tier.clone()));
        let mut job = unwrap_envelope(
            &Value::Dict(
                request_json(
                    "POST",
                    &submit_url,
                    &self.token,
                    Some(&Value::Dict(payload)),
                    req_timeout,
                )
                .await?,
            ),
            &submit_url,
        )?;
        let job_id = job
            .get(&Value::Str("jobId".to_string()))
            .cloned()
            .unwrap_or(Value::None);
        if !value_is_truthy(&job_id) {
            return Err(AdapterError {
                message: format!(
                    "cyberdiner: submit response missing jobId: {}",
                    Value::Dict(job.clone()).py_repr()
                ),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::RuntimeError,
            });
        }

        let poll_url = format!("{base}/beta/jobs/{}", job_id.py_str());
        let poll_interval = Duration::from_secs_f64((self.poll_interval_ms.max(0) as f64) / 1000.0);
        let mut status = job
            .get(&Value::Str("status".to_string()))
            .cloned()
            .unwrap_or(Value::None);

        while !is_terminal(&normalize_status(&status)) {
            if tokio::time::Instant::now() >= deadline {
                return Err(timed_out_error(&job_id, timeout_seconds, &status));
            }
            tokio::select! {
                biased;
                () = call.token.cancelled() => return Err(cancelled_error(&job_id)),
                () = tokio::time::sleep(poll_interval) => {},
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(timed_out_error(&job_id, timeout_seconds, &status));
            }
            job = unwrap_envelope(
                &Value::Dict(request_json("GET", &poll_url, &self.token, None, req_timeout).await?),
                &poll_url,
            )?;
            status = job
                .get(&Value::Str("status".to_string()))
                .cloned()
                .unwrap_or(Value::None);
        }

        let terminal = normalize_status(&status);
        if TIMED_OUT_STATUSES.contains(&terminal.as_str()) {
            return Err(AdapterError {
                message: format!(
                    "cyberdiner: job {} timed out server-side before being claimed \
                     (reason=claim_timeout) — no cook served tier {} in time.{}",
                    job_id.py_str(),
                    Value::Str(tier).py_repr(),
                    error_detail(&job)
                ),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::RuntimeError,
            });
        }
        if terminal == "failed" {
            let error_message = job
                .get(&Value::Str("errorMessage".to_string()))
                .filter(|v| value_is_truthy(v))
                .map(Value::py_str)
                .unwrap_or_else(|| "unknown error".to_string());
            return Err(AdapterError {
                message: format!(
                    "cyberdiner: job {} failed: {error_message}",
                    job_id.py_str()
                ),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::RuntimeError,
            });
        }
        if terminal == "cancelled" {
            return Err(AdapterError {
                message: format!("cyberdiner: job {} was cancelled.", job_id.py_str()),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::RuntimeError,
            });
        }

        let text = match job.get(&Value::Str("result".to_string())) {
            Some(v) if value_is_truthy(v) => v.py_str(),
            _ => String::new(),
        };
        let mut raw = Dict::new();
        raw.insert(Value::Str("jobId".to_string()), job_id.clone());
        raw.insert(Value::Str("status".to_string()), status.clone());
        raw.insert(Value::Str("tierName".to_string()), Value::Str(tier));
        raw.insert(
            Value::Str("durationMs".to_string()),
            job.get(&Value::Str("durationMs".to_string()))
                .cloned()
                .unwrap_or(Value::None),
        );
        raw.insert(Value::Str("data".to_string()), Value::Dict(job.clone()));

        Ok(GenerateResult {
            text,
            raw: Value::Dict(raw),
            tokens_sent: None,
            tokens_received: as_int(job.get(&Value::Str("tokensProcessed".to_string()))),
            finish_reason: None,
            warnings: crate::ignored_options_warning(
                "cyberdiner",
                &GenerateOptions::default(),
                &[],
            ),
            cost_usd: None,
        })
    }
}

fn timed_out_error(job_id: &Value, timeout_seconds: u32, status: &Value) -> AdapterError {
    AdapterError {
        message: format!(
            "cyberdiner: timed out after {timeout_seconds}s waiting for job {} to complete \
             (last status={}).",
            job_id.py_str(),
            status.py_repr()
        ),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    }
}

#[async_trait::async_trait(?Send)]
impl Adapter for CyberdinerAdapter {
    fn name(&self) -> &str {
        "cyberdiner"
    }

    async fn generate(
        &self,
        model: &str,
        prompt: &str,
        options: &GenerateOptions,
        call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        let mut result = self
            .generate_inner(model, prompt, call.timeout_seconds, call)
            .await?;
        // A job carries only a prompt and a tier -- everything else in
        // `options` is ignored, the same as Python's own `generate()`.
        result.warnings = crate::ignored_options_warning("cyberdiner", options, &[]);
        Ok(result)
    }

    /// `adapters/cyberdiner.py::CyberdinerAdapter.list_models` -- no
    /// network call: expo has no tier-listing endpoint.
    fn list_models(&self) -> Vec<String> {
        if self.valid_tiers.is_empty() {
            SEED_TIERS.iter().map(|s| s.to_string()).collect()
        } else {
            self.valid_tiers.clone()
        }
    }

    async fn check(&self) -> CheckResult {
        let mut missing = Vec::new();
        if self.token.is_empty() {
            missing.push("env:CYBERDINER token (runtime.adapters.cyberdiner.token)".to_string());
        }
        if self.expo_url.is_empty() {
            missing
                .push("env:CYBERDINER expo_url (runtime.adapters.cyberdiner.expo_url)".to_string());
            return CheckResult {
                ok: false,
                missing,
                message: None,
            };
        }

        let url = format!("{}/beta/jobs", self.expo_url.trim_end_matches('/'));
        let reachable = probe(&url, &self.token, Duration::from_secs(2)).await;
        if !reachable {
            missing.push(format!(
                "host:{}",
                crate::curl_text::redact_host_url(&self.expo_url)
            ));
        }
        CheckResult {
            ok: missing.is_empty(),
            missing,
            message: None,
        }
    }
}

/// `adapters/cyberdiner.py::CyberdinerAdapter.check`'s own probe --
/// any HTTP response (even 401/404/405) means the host answered; only
/// a connection-level failure counts as unreachable.
async fn probe(url: &str, token: &str, timeout: Duration) -> bool {
    let Ok(parsed) = parse_url(url) else {
        return false;
    };
    let port = parsed.port.unwrap_or_else(|| default_port(&parsed.scheme));
    let Ok(connect_target) = connect_target_for(&parsed, port) else {
        return false;
    };
    let Ok(client) = http_client::build_client() else {
        return false;
    };
    let host_header = host_header_value(&parsed.host, port, &parsed.scheme);
    let headers = urllib_headers(
        &host_header,
        None,
        &[("Authorization".to_string(), format!("Bearer {token}"))],
    );
    let raw_request = RawRequest {
        method: "GET".to_string(),
        target: parsed.target.clone(),
        headers,
        body: None,
    };
    http_client::send(
        &client,
        &connect_target,
        &raw_request,
        TimeoutModel::Total(timeout),
    )
    .await
    .is_ok()
}

/// `adapters/factory.py::_build_cyberdiner`.
pub fn build_cyberdiner(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    let raw_valid_tiers = crate::config::cfg_get(cfg, "valid_tiers")
        .and_then(Value::as_list)
        .map(|items| {
            items
                .iter()
                .map(|t| electricity_value::pycompat::py_strip(&t.py_str()).to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let poll_interval_ms = crate::config::cfg_get(cfg, "poll_interval_ms")
        .and_then(|v| match v {
            Value::Int(_) => electricity_value::pycompat::py_int(v).ok(),
            _ => None,
        })
        .filter(|v| *v != 0)
        .unwrap_or(500);
    let timeout_seconds = crate::config::cfg_get(cfg, "timeout_seconds")
        .and_then(|v| match v {
            Value::Int(_) => electricity_value::pycompat::py_int(v).ok(),
            _ => None,
        })
        .filter(|v| *v != 0)
        .unwrap_or(30);
    Ok(Box::new(CyberdinerAdapter {
        expo_url: cfg_str_or_rstrip_target(cfg, "expo_url", "")?,
        token: cfg_str_or(cfg, "token", ""),
        default_tier: cfg_str_or(cfg, "default_tier", "cheap"),
        valid_tiers: raw_valid_tiers,
        poll_interval_ms,
        timeout_seconds: timeout_seconds.max(0) as u32,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_bytecode::EffectPath;
    use electricity_value::CancellationToken;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn call_ctx<'a>(path: &'a EffectPath, token: &'a CancellationToken) -> CallContext<'a> {
        CallContext {
            path,
            timeout_seconds: 5,
            token,
        }
    }

    fn http_response(status: u16, reason: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// Accepts one connection per scripted response, in order -- a
    /// further connection attempt past the end of the list gets
    /// connection-refused, which is how a test proves "no further
    /// request happened" without its own counter.
    fn spawn_sequence_server(responses: Vec<String>) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 65536];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        addr
    }

    fn adapter(base_url: &str) -> CyberdinerAdapter {
        CyberdinerAdapter {
            expo_url: base_url.to_string(),
            token: "ck_test_token".to_string(),
            default_tier: "cheap".to_string(),
            valid_tiers: Vec::new(),
            poll_interval_ms: 1,
            timeout_seconds: 30,
        }
    }

    #[tokio::test]
    async fn generate_submits_polls_once_while_running_then_completes() {
        let addr = spawn_sequence_server(vec![
            http_response(200, "OK", r#"{"data":{"jobId":"j1","status":"pending"}}"#),
            http_response(200, "OK", r#"{"data":{"jobId":"j1","status":"running"}}"#),
            http_response(
                200,
                "OK",
                r#"{"data":{"jobId":"j1","status":"completed","result":" hi ","tokensProcessed":12,"durationMs":42}}"#,
            ),
        ]);
        let a = adapter(&format!("http://{addr}"));
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let result = a
            .generate(
                "cheap",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap();
        assert_eq!(result.text, " hi ");
        assert_eq!(result.tokens_sent, None);
        assert_eq!(result.tokens_received, Some(12));
        let raw = result.raw.as_dict().unwrap();
        assert_eq!(
            raw.get(&Value::Str("jobId".to_string())),
            Some(&Value::Str("j1".to_string()))
        );
        assert_eq!(
            raw.get(&Value::Str("tierName".to_string())),
            Some(&Value::Str("cheap".to_string()))
        );
    }

    #[tokio::test]
    async fn generate_fails_the_job() {
        let addr = spawn_sequence_server(vec![
            http_response(200, "OK", r#"{"data":{"jobId":"j1","status":"pending"}}"#),
            http_response(
                200,
                "OK",
                r#"{"data":{"jobId":"j1","status":"failed","errorMessage":"boom"}}"#,
            ),
        ]);
        let a = adapter(&format!("http://{addr}"));
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let err = a
            .generate(
                "cheap",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert_eq!(err.message, "cyberdiner: job j1 failed: boom");
        assert!(!err.retry.retryable);
    }

    #[tokio::test]
    async fn generate_reports_a_cancelled_job() {
        let addr = spawn_sequence_server(vec![
            http_response(200, "OK", r#"{"data":{"jobId":"j1","status":"pending"}}"#),
            http_response(200, "OK", r#"{"data":{"jobId":"j1","status":"cancelled"}}"#),
        ]);
        let a = adapter(&format!("http://{addr}"));
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let err = a
            .generate(
                "cheap",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert_eq!(err.message, "cyberdiner: job j1 was cancelled.");
    }

    #[tokio::test]
    async fn generate_reports_a_server_side_timed_out_job() {
        let addr = spawn_sequence_server(vec![
            http_response(200, "OK", r#"{"data":{"jobId":"j1","status":"pending"}}"#),
            http_response(200, "OK", r#"{"data":{"jobId":"j1","status":"timedOut"}}"#),
        ]);
        let a = adapter(&format!("http://{addr}"));
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let err = a
            .generate(
                "cheap",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert!(
            err.message.contains("timed out server-side"),
            "{}",
            err.message
        );
        assert!(err.message.contains("job j1 timed out"));
        assert!(err.message.contains("'cheap'"));
    }

    #[tokio::test]
    async fn generate_raises_when_jobid_is_missing_from_submit() {
        let addr = spawn_sequence_server(vec![http_response(
            200,
            "OK",
            r#"{"data":{"status":"pending"}}"#,
        )]);
        let a = adapter(&format!("http://{addr}"));
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let err = a
            .generate(
                "cheap",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert!(
            err.message
                .starts_with("cyberdiner: submit response missing jobId:")
        );
    }

    #[tokio::test]
    async fn generate_surfaces_an_http_error_on_submit() {
        let addr = spawn_sequence_server(vec![http_response(
            401,
            "Unauthorized",
            r#"{"error":"bad token"}"#,
        )]);
        let a = adapter(&format!("http://{addr}"));
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let err = a
            .generate(
                "cheap",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("HTTP 401"), "{}", err.message);
        assert!(err.message.contains("Unauthorized"));
        assert!(err.message.contains("bad token"));
    }

    #[tokio::test]
    async fn generate_requires_expo_url_and_token() {
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let no_url = CyberdinerAdapter {
            expo_url: String::new(),
            token: "ck_x".to_string(),
            default_tier: "cheap".to_string(),
            valid_tiers: Vec::new(),
            poll_interval_ms: 500,
            timeout_seconds: 30,
        };
        let err = no_url
            .generate(
                "cheap",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("expo_url not configured"));

        let no_token = CyberdinerAdapter {
            expo_url: "https://expo.example.test".to_string(),
            token: String::new(),
            default_tier: "cheap".to_string(),
            valid_tiers: Vec::new(),
            poll_interval_ms: 500,
            timeout_seconds: 30,
        };
        let err = no_token
            .generate(
                "cheap",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("token not configured"));
    }

    #[test]
    fn resolve_tier_falls_back_to_default_tier_when_model_is_blank() {
        assert_eq!(resolve_tier("", "cheap", &[]).unwrap(), "cheap");
        assert_eq!(resolve_tier("  ", "cheap", &[]).unwrap(), "cheap");
        assert_eq!(resolve_tier("good", "cheap", &[]).unwrap(), "good");
    }

    #[test]
    fn resolve_tier_raises_when_nothing_resolves() {
        let err = resolve_tier("", "", &[]).unwrap_err();
        assert_eq!(err.py_class, PyExc::ValueError);
        assert!(err.message.contains("no tier resolved"));
    }

    #[test]
    fn resolve_tier_validates_against_a_configured_allowlist() {
        let valid = vec!["good".to_string(), "fast".to_string()];
        assert_eq!(resolve_tier("good", "cheap", &valid).unwrap(), "good");
        let err = resolve_tier("bogus", "cheap", &valid).unwrap_err();
        assert_eq!(err.py_class, PyExc::ValueError);
        assert!(err.message.contains("unknown tier 'bogus'"));
        assert!(err.message.contains("good, fast"));
    }

    #[test]
    fn normalize_status_folds_every_spelling_onto_one_key() {
        assert_eq!(
            normalize_status(&Value::Str("TimedOut".to_string())),
            "timedout"
        );
        assert_eq!(
            normalize_status(&Value::Str("timed_out".to_string())),
            "timedout"
        );
        assert_eq!(
            normalize_status(&Value::Str("Complete".to_string())),
            "complete"
        );
        assert_eq!(normalize_status(&Value::None), "");
    }

    #[test]
    fn as_int_excludes_bool_and_non_numeric_values() {
        assert_eq!(as_int(Some(&Value::from(5i64))), Some(5));
        assert_eq!(as_int(Some(&Value::Float(5.9))), Some(5));
        assert_eq!(as_int(Some(&Value::Bool(true))), None);
        assert_eq!(as_int(Some(&Value::Str("5".to_string()))), None);
        assert_eq!(as_int(None), None);
    }

    #[test]
    fn list_models_defaults_to_the_seed_tiers_with_no_network_call() {
        let a = adapter("https://expo.example.test");
        assert_eq!(a.list_models(), SEED_TIERS.to_vec());
    }

    #[test]
    fn list_models_prefers_a_configured_allowlist() {
        let mut a = adapter("https://expo.example.test");
        a.valid_tiers = vec!["good".to_string(), "fast".to_string()];
        assert_eq!(
            a.list_models(),
            vec!["good".to_string(), "fast".to_string()]
        );
    }

    #[tokio::test]
    async fn check_reports_missing_token_and_expo_url() {
        let a = CyberdinerAdapter {
            expo_url: String::new(),
            token: String::new(),
            default_tier: "cheap".to_string(),
            valid_tiers: Vec::new(),
            poll_interval_ms: 500,
            timeout_seconds: 30,
        };
        let r = a.check().await;
        assert!(!r.ok);
        assert!(
            r.missing
                .contains(&"env:CYBERDINER token (runtime.adapters.cyberdiner.token)".to_string())
        );
        assert!(r.missing.contains(
            &"env:CYBERDINER expo_url (runtime.adapters.cyberdiner.expo_url)".to_string()
        ));
    }

    #[tokio::test]
    async fn check_is_reachable_for_an_http_error_response() {
        let addr = spawn_sequence_server(vec![http_response(401, "Unauthorized", "{}")]);
        let a = adapter(&format!("http://{addr}"));
        let r = a.check().await;
        assert!(r.ok, "{:?}", r.missing);
    }

    #[tokio::test]
    async fn check_reports_missing_host_masking_userinfo_for_a_closed_port() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let a = adapter(&format!("http://user:secret@{addr}"));
        let r = a.check().await;
        assert!(!r.ok);
        assert_eq!(r.missing, vec![format!("host:http://{addr}")]);
        assert!(!r.missing.iter().any(|m| m.contains("secret")));
    }

    #[test]
    fn build_cyberdiner_defaults_poll_interval_and_timeout() {
        let adapter = build_cyberdiner(&Value::None).unwrap();
        assert_eq!(adapter.name(), "cyberdiner");
    }
}
