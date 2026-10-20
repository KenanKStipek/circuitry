//! `adapters/cyberdiner.py::CyberdinerAdapter`, ported -- a job-queue
//! LLM broker: submit a job, poll it until a terminal state, block
//! until the job completes, then return the text. Python sends its
//! own requests with `urllib.request`, never `curl` (the only adapter
//! in this crate that doesn't shell out to curl's own convention) --
//! this module reproduces urllib's own request shape over
//! [`electricity_tools::http_client`] directly, the same transport
//! primitive the `http` tool (`electricity_tools::http`) uses, rather
//! than [`crate::transport::run_curl`]'s curl-flavored header set and
//! exit-code mapping. Issue #515's own review: every
//! point below reuses the `http` tool's own urllib logic directly
//! (`electricity_tools::http::resolve_proxy_target`/`http_url::urljoin`,
//! `TimeoutModel::PerOperation`) rather than re-deriving it, the same
//! ruling that produced `http.rs` in the first place.
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
//! proxy configured, no redirect, and a well-formed response body):
//! - a redirect loop past [`MAX_REPEATS`]/[`MAX_REDIRECTIONS`] falls
//!   through to the plain non-2xx error path rather than reproducing
//!   `HTTPRedirectHandler`'s own distinct "redirect loop" reason text;
//! - `poll_interval_ms`/`timeout_seconds`/`max_in_flight` read from
//!   config reproduce Python's own `int(x)` coercion (accepts an
//!   `int`/`float`/numeric `str`, raises the same `TypeError`/
//!   `ValueError` otherwise) at build time, matching
//!   `adapters/factory.py::_build_cyberdiner`'s own eager `int(...)`
//!   calls exactly;
//! - a cancellation while waiting for a free `max_in_flight` slot
//!   raises this crate's own text -- Python's `acquire_promptly`
//!   (`core/cancellation.py`) polls the semaphore in short slices with
//!   no raise of its own at all, relying on a delivered OS signal to
//!   interrupt the *process*, a mechanism this crate's own
//!   per-call `CancellationToken` doesn't reproduce.

use crate::config::{cfg_int_or, cfg_value_or, rstrip_base_url, value_is_truthy};
use crate::transport::{
    ParsedUrl, TransportFailure, default_port, parse_url, redact_url_for_error,
};
use electricity_json::WriteMode;
use electricity_tools::http::http_url::urljoin;
use electricity_tools::http_client::{
    self, ConnectTarget, RawRequest, RawResponse, Scheme, SendError, TimeoutModel, redirect_policy,
};
use electricity_value::{Dict, Value};
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult, PyExc,
};
use electricity_vm::retry::{RetryInfo, status_is_retryable};
use std::collections::HashMap;
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
/// `HTTPRedirectHandler.max_repeats` -- the same cap `http.rs` itself
/// uses for the `http` tool.
const MAX_REPEATS: u32 = 4;
/// `HTTPRedirectHandler.max_redirections`.
const MAX_REDIRECTIONS: usize = 10;

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

/// `adapters/cyberdiner.py::_resolve_tier` -- *default_tier* is raw
/// config (a truthy non-string value raises the
/// same `AttributeError` Python's own `(default_tier or "").strip()`
/// does, but only when *model* is blank -- Python's own `or` never
/// evaluates the right side at all when the left is already truthy).
fn resolve_tier(
    model: &str,
    default_tier: &Value,
    valid_tiers: &[String],
) -> Result<String, AdapterError> {
    let trimmed_model = electricity_value::pycompat::py_strip(model);
    let tier = if !trimmed_model.is_empty() {
        trimmed_model.to_string()
    } else if value_is_truthy(default_tier) {
        match default_tier {
            Value::Str(s) => electricity_value::pycompat::py_strip(s).to_string(),
            other => return Err(attribute_error(other, "strip")),
        }
    } else {
        String::new()
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

/// `req.host` -- host[:port] exactly as the current hop's own URL
/// writes it: an explicit port is never dropped even when it matches
/// the scheme's own default, and none is ever filled in for an
/// implicit one (`electricity_tools::http::
/// http.rs`'s own `netloc_host_as_written` doc comment -- confirmed
/// directly against CPython, distinct from `transport.rs::
/// host_header_value`'s curl-flavored default-port omission every
/// other adapter in this crate uses).
fn urllib_host_header(parsed: &ParsedUrl) -> String {
    match parsed.port {
        Some(port) => format!("{}:{port}", parsed.host),
        None => parsed.host.clone(),
    }
}

/// The netloc (`[user[:pass]@]host[:port]`) `resolve_proxy_target`
/// needs for its own `no_proxy` bypass check -- reconstructed from
/// [`ParsedUrl`]'s already-split fields, equivalent to the substring
/// `urllib.parse.urlsplit` itself would have produced.
fn netloc_for_proxy(parsed: &ParsedUrl) -> String {
    let hostport = urllib_host_header(parsed);
    match &parsed.userinfo {
        Some((user, password)) if !password.is_empty() => format!("{user}:{password}@{hostport}"),
        Some((user, _)) => format!("{user}@{hostport}"),
        None => hostport,
    }
}

/// urllib's own environment
/// convention and proxy-URL/auth parsing
/// (`electricity_tools::http::resolve_proxy_target`, shared with the
/// `http` tool directly), never `transport::resolve_curl_proxy`'s
/// curl-flavored rules (`HTTP_PROXY` ignored for `http://`, `ALL_PROXY`
/// honoured, ...), which belong to every other adapter in this crate
/// alone.
fn connect_target_for(parsed: &ParsedUrl, port: u16) -> ConnectTarget {
    let scheme = if parsed.scheme == "https" {
        Scheme::Https
    } else {
        Scheme::Http
    };
    let proxy =
        electricity_tools::http::resolve_proxy_target(&parsed.scheme, &netloc_for_proxy(parsed));
    ConnectTarget {
        scheme,
        host: parsed.host.clone(),
        port,
        proxy,
    }
}

/// A [`SendError`] this crate's own `http`
/// tool already classifies correctly (`electricity_tools::http::
/// send_error_to_tool_error`'s own doc comment) -- *url* is used only
/// for [`SendError::Connect`]'s own wrapped shape
/// (`RuntimeError(...) from exc`, `urllib.error.URLError`'s own
/// embedding); every other variant is Python's own *unwrapped*
/// exception (`_request` has no handler for it at all: `TimeoutError`/
/// `http.client.RemoteDisconnected`/`http.client.IncompleteRead`
/// propagate past `except urllib.error.URLError` untouched, since
/// none of the three is a `URLError`), so its message/class carries
/// no `cyberdiner: ...`/*url* framing at all. Retry classification
/// mirrors `adapters/_retry.py::_classify_cause_chain`: `URLError`
/// (`Connect`), `TimeoutError` (`Timeout`) and `ConnectionError`
/// (`RemoteDisconnected`, a `ConnectionResetError` subclass) are all
/// unconditionally retryable; `IncompleteRead` (an `http.client.
/// HTTPException`, none of the three) is not.
fn send_error(url: &str, err: SendError) -> AdapterError {
    match err {
        SendError::Connect { text, .. } => AdapterError {
            message: format!("cyberdiner: request to {url} failed: {text}"),
            retry: RetryInfo::retryable(),
            py_class: PyExc::RuntimeError,
        },
        SendError::Timeout => AdapterError {
            message: "timed out".to_string(),
            retry: RetryInfo::retryable(),
            py_class: PyExc::TimeoutError,
        },
        SendError::RemoteDisconnected => AdapterError {
            message: "Remote end closed connection without response".to_string(),
            retry: RetryInfo::retryable(),
            py_class: PyExc::RemoteDisconnected,
        },
        SendError::IncompleteRead { read, expected } => AdapterError {
            message: format!("IncompleteRead({read} bytes read, {expected} more expected)"),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::IncompleteRead,
        },
        SendError::Other(text) => AdapterError {
            message: format!("cyberdiner: request to {url} failed: {text}"),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        },
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

/// A non-2xx response that was never followed as a redirect --
/// `urllib.error.HTTPError`'s own shape (`_request`'s own `except
/// urllib.error.HTTPError as exc: ... raise RuntimeError(f"cyberdiner:
/// HTTP {exc.code} from {url}: {exc.reason}" + (f" — {detail}" if
/// detail else ""))`). *detail* is `exc.read()`'s own text,
/// **unchanged** (no trim, no truncation -- Python
/// only ever omits it when it's the empty string) -- when the body
/// read itself failed mid-transfer, Python's own `except Exception:
/// detail = ""` means *detail* is simply absent, not an error of its
/// own (`exc.fp` is checked first, so a `None` body is never attempted
/// either): a 4xx/5xx has no handler of its own to re-raise through at
/// all, unlike a 2xx (see [`interpret_outcome`]'s own `body_error`
/// branch).
fn http_error(
    url: &str,
    status: u16,
    reason: &str,
    detail: &str,
    retry_after: Option<&str>,
) -> AdapterError {
    let message = if detail.is_empty() {
        format!("cyberdiner: HTTP {status} from {url}: {reason}")
    } else {
        format!("cyberdiner: HTTP {status} from {url}: {reason} — {detail}")
    };
    AdapterError {
        message,
        retry: RetryInfo {
            retryable: status_is_retryable(status),
            status: Some(status),
            retry_after: retry_after.map(str::to_string),
        },
        py_class: PyExc::RuntimeError,
    }
}

fn non_json_error(url: &str, text: &str) -> AdapterError {
    AdapterError {
        message: format!(
            "cyberdiner: non-JSON response from {url}: {}",
            truncate_chars(text, 200)
        ),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    }
}

/// The successful (2xx) half of [`interpret_outcome`] -- a body-read
/// failure here has no `except HTTPError` handler of its own to
/// swallow it into (unlike a 4xx/5xx), so it propagates the same
/// unwrapped shape [`send_error`] gives a hard [`http_client::send`]
/// failure -- the body error is checked before the body is parsed,
/// the same order every other call site here uses.
fn interpret_success(url: &str, response: RawResponse) -> Result<Dict, AdapterError> {
    if let Some(body_error) = response.body_error {
        return Err(send_error(url, body_error));
    }
    let text = String::from_utf8_lossy(&response.body).into_owned();
    if text.is_empty() {
        return Ok(Dict::new());
    }
    match electricity_json::loads(&text) {
        Ok(Value::Dict(ref dict)) => Ok(dict.clone()),
        _ => Err(non_json_error(url, &text)),
    }
}

/// The non-2xx-and-not-followed half of [`interpret_outcome`] --
/// *detail* is the body urllib's own `exc.read()` would have returned:
/// when the body read failed mid-transfer, Python's own `except
/// Exception: detail = ""` swallows that failure entirely (a 4xx/5xx
/// has no `with urlopen(...)` context manager of its own waiting to
/// re-raise it), so this reads whatever bytes *did* arrive
/// ([`RawResponse::partial_body`]) rather than the (cleared, `http`-
/// tool-specific) [`RawResponse::body`].
fn interpret_error(url: &str, response: RawResponse) -> AdapterError {
    let detail_source = if response.body_error.is_some() {
        &response.partial_body
    } else {
        &response.body
    };
    let detail = String::from_utf8_lossy(detail_source).into_owned();
    let retry_after = find_header(&response.headers, "retry-after");
    http_error(url, response.status, &response.reason, &detail, retry_after)
}

/// `adapters/cyberdiner.py::_request` -- *payload* is serialized to
/// JSON and sent as the body of a `POST`/`PATCH`-shaped request;
/// `None` sends no body at all (a `GET`). Follows a 3xx exactly as
/// urllib does: [`urljoin`] resolves a relative `Location` against
/// the current URL, [`redirect_policy`] decides whether the method/
/// status pair is followed at all, and an unfollowed 3xx (no
/// `Location`, a disallowed method/status, or the redirect cap below)
/// falls straight into the ordinary non-2xx error path -- urllib
/// treats *any* non-2xx as an `HTTPError`, never only `>= 400`.
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
    let mut visited: HashMap<String, u32> = HashMap::new();

    loop {
        let parsed =
            parse_url(&current_url).map_err(|e| transport_failure_error(&current_url, e))?;
        let port = parsed.port.unwrap_or_else(|| default_port(&parsed.scheme));
        let connect_target = connect_target_for(&parsed, port);
        let host_header = urllib_host_header(&parsed);

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
            TimeoutModel::PerOperation(timeout),
        )
        .await
        .map_err(|err| send_error(&current_url, err))?;

        let status = response.status;
        if (200..300).contains(&status) {
            return interpret_success(&current_url, response);
        }

        if (300..=399).contains(&status) {
            if let Some(location) = find_header(&response.headers, "location") {
                if let Some(redirect) = redirect_policy(&current_method, status) {
                    let resolved = urljoin(&current_url, location);
                    let repeat_count = visited.get(&resolved).copied().unwrap_or(0);
                    if repeat_count < MAX_REPEATS && visited.len() < MAX_REDIRECTIONS {
                        *visited.entry(resolved.clone()).or_insert(0) += 1;
                        current_url = resolved;
                        current_method = redirect.method;
                        if redirect.drop_body {
                            current_body = None;
                        }
                        continue;
                    }
                }
            }
        }

        return Err(interpret_error(&current_url, response));
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
    /// Raw -- `cfg.get("expo_url") or ""`. See `openai.rs::
    /// OpenAiAdapter::base_url`'s own doc comment:
    /// `.rstrip("/")`'d in `generate()`/`check()` themselves, after
    /// the same earlier checks Python runs first.
    pub expo_url: Value,
    /// Raw -- `cfg.get("token") or ""`. Never itself a source of a
    /// non-string failure: Python's own
    /// `f"Bearer {self.token}"` renders any value with `str()`,
    /// never raising.
    pub token: Value,
    /// Raw -- `cfg.get("default_tier") or "cheap"`. See
    /// [`resolve_tier`]'s own doc comment.
    pub default_tier: Value,
    pub valid_tiers: Vec<String>,
    pub poll_interval_ms: i64,
    pub timeout_seconds: i64,
    /// `None` when `max_in_flight` is unset/`0` (unbounded, the
    /// default) -- a Rust adapter's *default*
    /// instance is reused across every call on one run
    /// (`exec/prompt.rs`'s own doc comment), exactly like Python's
    /// long-lived `self`, so a semaphore scoped to this instance
    /// coordinates the same way Python's own `threading.Semaphore`
    /// does; it is only inert for a `provider:` override, which
    /// rebuilds a fresh instance per call in both engines alike.
    semaphore: Option<tokio::sync::Semaphore>,
}

impl CyberdinerAdapter {
    async fn generate_inner(
        &self,
        model: &str,
        prompt: &str,
        timeout_seconds: u32,
        call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        if !value_is_truthy(&self.expo_url) {
            return Err(AdapterError {
                message: "cyberdiner: expo_url not configured. Set \
                          runtime.adapters.cyberdiner.expo_url to your CyberDiner expo \
                          deployment's root URL."
                    .to_string(),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::RuntimeError,
            });
        }
        if !value_is_truthy(&self.token) {
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
        let base = rstrip_base_url(&self.expo_url)?;
        let token = self.token.py_str();
        let req_timeout = Duration::from_secs_f64(
            (self.timeout_seconds as f64)
                .min(f64::from(timeout_seconds))
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
                    &token,
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
                &Value::Dict(request_json("GET", &poll_url, &token, None, req_timeout).await?),
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

fn semaphore_cancelled_error() -> AdapterError {
    AdapterError {
        message: "cyberdiner: cancelled while waiting for a free max_in_flight slot.".to_string(),
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
        // `max_in_flight` bounds how many
        // `generate()` calls may hold a submitted-but-not-yet-terminal
        // job at once, enforced with the same semaphore shared across
        // every call on this adapter instance Python's own
        // `threading.Semaphore` is.
        let _permit = match &self.semaphore {
            Some(sem) => {
                tokio::select! {
                    biased;
                    () = call.token.cancelled() => return Err(semaphore_cancelled_error()),
                    permit = sem.acquire() => Some(permit.expect("semaphore is never closed")),
                }
            }
            None => None,
        };
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
        if !value_is_truthy(&self.token) {
            missing.push("env:CYBERDINER token (runtime.adapters.cyberdiner.token)".to_string());
        }
        if !value_is_truthy(&self.expo_url) {
            missing
                .push("env:CYBERDINER expo_url (runtime.adapters.cyberdiner.expo_url)".to_string());
            return CheckResult {
                ok: false,
                missing,
                message: None,
            };
        }

        // Python's own `req = urllib.request.Request(self.expo_url.
        // rstrip("/") + "/beta/jobs", ...)` sits *outside* the
        // `try/except Exception` around the probe itself -- a
        // non-string `expo_url` raises uncaught out of `check()`
        // entirely, for `preflight.py::call_check`'s own "check()
        // raised ..." fold (confirmed by running
        // `CyberdinerAdapter(expo_url=5, token="x").check()` directly:
        // `missing=[]`, discarding whatever this function already
        // pushed above, exactly like `scripted.rs`'s own
        // `check() raised {type}: {}` convention).
        let base = match rstrip_base_url(&self.expo_url) {
            Ok(base) => base,
            Err(err) => {
                return CheckResult {
                    ok: false,
                    missing: Vec::new(),
                    message: Some(format!(
                        "check() raised {}: {}",
                        err.py_class.name(),
                        err.message
                    )),
                };
            }
        };

        let url = format!("{base}/beta/jobs");
        let token = self.token.py_str();
        let reachable = probe(&url, &token, Duration::from_secs(2)).await;
        if !reachable {
            missing.push(format!(
                "host:{}",
                crate::curl_text::redact_host_url(&self.expo_url.py_str())
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
    let connect_target = connect_target_for(&parsed, port);
    let Ok(client) = http_client::build_client() else {
        return false;
    };
    let host_header = urllib_host_header(&parsed);
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
        TimeoutModel::PerOperation(timeout),
    )
    .await
    .is_ok()
}

fn new_semaphore(max_in_flight: i64) -> Option<tokio::sync::Semaphore> {
    if max_in_flight > 0 {
        Some(tokio::sync::Semaphore::new(max_in_flight as usize))
    } else {
        None
    }
}

/// `adapters/factory.py::_build_cyberdiner`.
pub fn build_cyberdiner(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    // `raw_valid_tiers = cfg.get("valid_tiers") or ()`; `if
    // isinstance(raw_valid_tiers, str): raw_valid_tiers = (raw_valid_tiers,)`
    // -- a configured string wraps as a single
    // tier, rather than being dropped as a non-list.
    let valid_tiers: Vec<String> = match crate::config::cfg_get(cfg, "valid_tiers") {
        Some(Value::Str(s)) if !s.is_empty() => {
            let trimmed = electricity_value::pycompat::py_strip(s);
            if trimmed.is_empty() {
                Vec::new()
            } else {
                vec![trimmed.to_string()]
            }
        }
        Some(v) if value_is_truthy(v) => v
            .as_list()
            .map(|items| {
                items
                    .iter()
                    .map(|t| electricity_value::pycompat::py_strip(&t.py_str()).to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let max_in_flight = cfg_int_or(cfg, "max_in_flight", 0)?;
    Ok(Box::new(CyberdinerAdapter {
        expo_url: cfg_value_or(cfg, "expo_url", ""),
        token: cfg_value_or(cfg, "token", ""),
        default_tier: cfg_value_or(cfg, "default_tier", "cheap"),
        valid_tiers,
        poll_interval_ms: cfg_int_or(cfg, "poll_interval_ms", 500)?,
        timeout_seconds: cfg_int_or(cfg, "timeout_seconds", 30)?,
        semaphore: new_semaphore(max_in_flight),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_bytecode::EffectPath;
    use electricity_value::{CancellationToken, Dict as VDict};
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
            expo_url: Value::Str(base_url.to_string()),
            token: Value::Str("ck_test_token".to_string()),
            default_tier: Value::Str("cheap".to_string()),
            valid_tiers: Vec::new(),
            poll_interval_ms: 1,
            timeout_seconds: 30,
            semaphore: None,
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
        assert!(!err.retry.retryable);
        assert_eq!(err.retry.status, Some(401));
    }

    #[tokio::test]
    async fn generate_retries_a_503_on_submit() {
        // An HTTP error classifies by status the
        // same way a curl-based adapter's own `classify_curl_exit`
        // does -- a 503 is retryable.
        let addr = spawn_sequence_server(vec![http_response(503, "Service Unavailable", "")]);
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
        assert!(err.retry.retryable);
        assert_eq!(err.retry.status, Some(503));
    }

    #[tokio::test]
    async fn generate_retries_a_connection_refused_submit() {
        // `urllib.error.URLError` (a connect-phase
        // failure) is unconditionally retryable, the same as a curl
        // exit this crate already retries (6/7/28/35/52/56).
        let addr = crate::test_support::closed_port();
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
        assert!(err.retry.retryable, "{:?}", err.retry);
        assert!(err.message.contains("cyberdiner: request to"));
    }

    #[tokio::test]
    async fn generate_requires_expo_url_and_token() {
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let no_url = CyberdinerAdapter {
            expo_url: Value::Str(String::new()),
            token: Value::Str("ck_x".to_string()),
            default_tier: Value::Str("cheap".to_string()),
            valid_tiers: Vec::new(),
            poll_interval_ms: 500,
            timeout_seconds: 30,
            semaphore: None,
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
            expo_url: Value::Str("https://expo.example.test".to_string()),
            token: Value::Str(String::new()),
            default_tier: Value::Str("cheap".to_string()),
            valid_tiers: Vec::new(),
            poll_interval_ms: 500,
            timeout_seconds: 30,
            semaphore: None,
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

    #[tokio::test]
    async fn generate_fails_like_python_for_a_non_string_default_tier_when_model_is_blank() {
        // `(default_tier or "").strip()` raises
        // when *model* is blank and `default_tier` is a truthy
        // non-string -- never reached at all when a model is pinned.
        let mut a = adapter("https://expo.example.test");
        a.default_tier = Value::from(5i64);
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let err = a
            .generate(
                "",
                "hi",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert_eq!(err.message, "'int' object has no attribute 'strip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }

    #[test]
    fn resolve_tier_falls_back_to_default_tier_when_model_is_blank() {
        assert_eq!(
            resolve_tier("", &Value::Str("cheap".to_string()), &[]).unwrap(),
            "cheap"
        );
        assert_eq!(
            resolve_tier("  ", &Value::Str("cheap".to_string()), &[]).unwrap(),
            "cheap"
        );
        assert_eq!(
            resolve_tier("good", &Value::Str("cheap".to_string()), &[]).unwrap(),
            "good"
        );
    }

    #[test]
    fn resolve_tier_raises_when_nothing_resolves() {
        let err = resolve_tier("", &Value::Str(String::new()), &[]).unwrap_err();
        assert_eq!(err.py_class, PyExc::ValueError);
        assert!(err.message.contains("no tier resolved"));
    }

    #[test]
    fn resolve_tier_validates_against_a_configured_allowlist() {
        let valid = vec!["good".to_string(), "fast".to_string()];
        assert_eq!(
            resolve_tier("good", &Value::Str("cheap".to_string()), &valid).unwrap(),
            "good"
        );
        let err = resolve_tier("bogus", &Value::Str("cheap".to_string()), &valid).unwrap_err();
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
            expo_url: Value::Str(String::new()),
            token: Value::Str(String::new()),
            default_tier: Value::Str("cheap".to_string()),
            valid_tiers: Vec::new(),
            poll_interval_ms: 500,
            timeout_seconds: 30,
            semaphore: None,
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
        let addr = crate::test_support::closed_port();
        let a = adapter(&format!("http://user:secret@{addr}"));
        let r = a.check().await;
        assert!(!r.ok);
        assert_eq!(r.missing, vec![format!("host:http://{addr}")]);
        assert!(!r.missing.iter().any(|m| m.contains("secret")));
    }

    #[tokio::test]
    async fn check_reports_check_raised_for_a_non_string_expo_url() {
        // Confirmed by running `CyberdinerAdapter(expo_url=5, token="x")
        // .check()` directly: unlike `ollama`'s own
        // `check()`, cyberdiner's has no `try/except` around the
        // `.rstrip("/")` call at all, so it propagates uncaught for
        // `call_check`'s own fold.
        let a = CyberdinerAdapter {
            expo_url: Value::from(5i64),
            token: Value::Str("ck_x".to_string()),
            default_tier: Value::Str("cheap".to_string()),
            valid_tiers: Vec::new(),
            poll_interval_ms: 500,
            timeout_seconds: 30,
            semaphore: None,
        };
        let r = a.check().await;
        assert!(!r.ok);
        assert!(r.missing.is_empty());
        assert_eq!(
            r.message,
            Some(
                "check() raised AttributeError: 'int' object has no attribute 'rstrip'".to_string()
            )
        );
    }

    #[test]
    fn build_cyberdiner_defaults_poll_interval_and_timeout() {
        let adapter = build_cyberdiner(&Value::None).unwrap();
        assert_eq!(adapter.name(), "cyberdiner");
    }

    #[test]
    fn build_cyberdiner_wraps_a_string_valid_tiers_as_a_single_tier() {
        let mut cfg = VDict::new();
        cfg.insert(
            Value::Str("valid_tiers".to_string()),
            Value::Str("good".to_string()),
        );
        let adapter = build_cyberdiner(&Value::Dict(cfg)).unwrap();
        assert_eq!(adapter.list_models(), vec!["good".to_string()]);
    }

    #[test]
    fn build_cyberdiner_coerces_a_float_poll_interval_and_timeout() {
        let mut cfg = VDict::new();
        cfg.insert(
            Value::Str("poll_interval_ms".to_string()),
            Value::Float(250.9),
        );
        cfg.insert(
            Value::Str("timeout_seconds".to_string()),
            Value::Float(45.9),
        );
        // Built successfully -- `int(250.9)`/`int(45.9)` truncate rather
        // than raising, matching Python's own `int()` builtin.
        build_cyberdiner(&Value::Dict(cfg)).unwrap();
    }

    #[test]
    fn build_cyberdiner_raises_cpythons_text_for_a_bad_timeout_seconds() {
        let mut cfg = VDict::new();
        cfg.insert(
            Value::Str("timeout_seconds".to_string()),
            Value::Str("x".to_string()),
        );
        let err = match build_cyberdiner(&Value::Dict(cfg)) {
            Err(err) => err,
            Ok(_) => panic!("expected build_cyberdiner to fail"),
        };
        assert_eq!(err.message, "invalid literal for int() with base 10: 'x'");
        assert_eq!(err.py_class, PyExc::ValueError);
    }

    #[tokio::test]
    async fn max_in_flight_bounds_concurrent_jobs() {
        // A `max_in_flight: 1` instance lets only
        // one `generate()` call hold a submitted job at a time -- the
        // second call's own submit request only reaches the mock
        // server after the first call's own job completes and frees
        // the permit (proven the same way `spawn_sequence_server`
        // proves every other request-sequence test here: the mock
        // only ever accepts one connection at a time, so a racing,
        // out-of-order second submit would hang this test out until
        // the overall `cargo test` deadline rather than complete).
        let addr = spawn_sequence_server(vec![
            http_response(
                200,
                "OK",
                r#"{"data":{"jobId":"j1","status":"completed","result":"a"}}"#,
            ),
            http_response(
                200,
                "OK",
                r#"{"data":{"jobId":"j2","status":"completed","result":"b"}}"#,
            ),
        ]);
        let mut a = adapter(&format!("http://{addr}"));
        a.semaphore = new_semaphore(1);
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let call = call_ctx(&path, &token);
        let options = GenerateOptions::default();
        let (first, second) = tokio::join!(
            a.generate("cheap", "hi", &options, &call),
            a.generate("cheap", "hi", &options, &call),
        );
        let mut texts = vec![first.unwrap().text, second.unwrap().text];
        texts.sort();
        assert_eq!(texts, vec!["a".to_string(), "b".to_string()]);
    }
}
