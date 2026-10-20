//! `curl_support.py::run_curl`, ported to electricity's shared HTTP
//! client -- the one request path every real provider adapter in this
//! crate shares (issue #503's scope table). electricity never runs a
//! `curl` binary: this module sends the request itself, through
//! [`electricity_tools::http_client::send`], and reproduces what
//! Circuitry observably does with curl's own result -- the header
//! set on the wire, curl's proxy-environment convention, its
//! `--max-time` as one total deadline, its exit-code mapping, and
//! `run_tracked(text=True)`'s own strict-UTF-8/universal-newline body
//! decode.
//!
//! What this module does *not* do: build the provider-specific
//! request body, interpret a successful JSON response, or choose a
//! hint for a failed one -- that is each adapter's own job (`ollama`,
//! `openai_compat`, `openai`, `anthropic`, `replicate`, `watsonx`),
//! exactly as it is `curl_failure_message`'s own caller's job in the
//! Python reference. This module's own output, [`CurlOutcome`], is
//! deliberately shaped like Python's `subprocess.CompletedProcess`
//! (`returncode`/`stdout`/`stderr`) so every adapter's own curl-exit
//! branches port directly off `curl_support.py`'s own `if proc.
//! returncode != 0:` shape.
//!
//! Cancellation belongs to lane G (#498's own "Interaction with other
//! lanes" ruling): the caller races the whole `generate()` call
//! (which runs through this module) against its own cancellation
//! token and drops the future to abort -- dropping the future this
//! module returns drops the `send` future inside it, which is what
//! actually closes the socket (`http_client`'s own `AbortOnDrop`).
//! This module itself never polls a token.

use electricity_tools::http_client::{
    self, ConnectKind, ConnectTarget, HttpClient, ProxyTarget, RawRequest, Scheme, SendError,
    TimeoutModel,
};
use std::time::Duration;

/// `subprocess.CompletedProcess[str]`'s own three fields, the shape
/// every adapter's own curl-exit branch reads -- `returncode == 0` is
/// success, matching `curl_support.run_curl`'s own caller convention
/// exactly (`if proc.returncode != 0: ...`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurlOutcome {
    pub returncode: i32,
    pub stdout: String,
    pub stderr: String,
    /// The HTTP status, when the request reached a response and
    /// *returncode* is 22 -- electricity's own replacement for
    /// Python's own `_CURL_STATUS_RE` scrape of curl's stderr text
    /// (`classify_curl_exit`): the same information, read directly off
    /// the response rather than re-parsed out of a message this crate
    /// synthesizes itself.
    pub status: Option<u16>,
    /// The response's own `Retry-After` header, verbatim, when the
    /// request reached a response that carried one -- Python's own
    /// `extract_retry_after` reads this off curl's `--write-out`
    /// capture; electricity has the real response headers already, so
    /// it reads it directly instead.
    pub retry_after: Option<String>,
}

/// A failure raised *before* [`CurlOutcome`] exists at all --
/// `curl_support.py`'s own pre-flight `ValueError`s (a forbidden
/// control character, an oversized `--config` write) and
/// `run_tracked(text=True)`'s own strict-decode `UnicodeDecodeError`,
/// which happens *before* Python's own `if proc.returncode != 0:`
/// check ever runs, regardless of the HTTP status. Neither is
/// retryable (issue #503's own "Retry classification" section).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportFailure {
    /// `ValueError`'s own text, verbatim.
    Validation(String),
    /// `UnicodeDecodeError`'s own text, verbatim.
    InvalidUtf8(String),
}

impl std::fmt::Display for TransportFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportFailure::Validation(text) | TransportFailure::InvalidUtf8(text) => {
                write!(f, "{text}")
            }
        }
    }
}

/// curl's config-file write, `_curl_config_file`'s own forbidden
/// characters -- a bare CR, LF or NUL would end the line early inside
/// electricity's own in-process equivalent, exactly as it would in a
/// real `--config` file; electricity never writes one, but a value
/// containing one of these is refused all the same, by location, so
/// the security property (an operator-controlled value can't inject a
/// second curl option) stays identical whether or not a real config
/// file is in the picture.
const FORBIDDEN_CONFIG_CHARS: [char; 3] = ['\r', '\n', '\0'];

/// `curl_support.py::_MAX_CONFIG_BYTES` -- comfortably under the
/// smallest common pipe buffer size a real `--config` write would
/// block on.
const MAX_CONFIG_BYTES: usize = 60_000;

fn validate_no_control_chars(value: &str, label: &str) -> Result<(), TransportFailure> {
    if value.chars().any(|c| FORBIDDEN_CONFIG_CHARS.contains(&c)) {
        return Err(TransportFailure::Validation(format!(
            "{label} contains a forbidden control character"
        )));
    }
    Ok(())
}

/// `_escape_config_value` -- curl's own double-quoted config value
/// escaping, `\` and `"` only.
fn escape_config_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// `_curl_config_file`'s own byte count, for the size check only --
/// electricity never actually writes this text anywhere.
fn curl_config_bytes(url: &str, headers: &[(String, String)]) -> usize {
    let mut text = format!("url = \"{}\"\n", escape_config_value(url));
    for (name, value) in headers {
        text.push_str(&format!(
            "header = \"{name}: {}\"\n",
            escape_config_value(value)
        ));
    }
    text.len()
}

fn validate_request(url: &str, headers: &[(String, String)]) -> Result<(), TransportFailure> {
    validate_no_control_chars(url, "url")?;
    for (name, value) in headers {
        validate_no_control_chars(name, "header name")?;
        validate_no_control_chars(value, "header value")?;
    }
    let config_bytes = curl_config_bytes(url, headers);
    if config_bytes > MAX_CONFIG_BYTES {
        return Err(TransportFailure::Validation(format!(
            "url + headers are {config_bytes} bytes, over the {MAX_CONFIG_BYTES}-byte limit \
             for a single curl --config write"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// A minimal `url = "scheme://[user:pass@]host[:port][/path][?query]"`
// splitter -- electricity's own equivalent of what curl itself parses
// out of the `--config` file's `url =` line; narrower than
// `urllib.parse.urlsplit` (no fragment, no relative references) since
// every URL this module ever sees is already absolute (an adapter's
// own `base_url.rstrip('/') + path`).
// ---------------------------------------------------------------------

pub(crate) struct ParsedUrl {
    pub(crate) scheme: String,
    pub(crate) userinfo: Option<(String, String)>,
    pub(crate) host: String,
    pub(crate) port: Option<u16>,
    pub(crate) target: String,
}

pub(crate) fn default_port(scheme: &str) -> u16 {
    if scheme == "https" { 443 } else { 80 }
}

/// A URL for use in electricity's own refusal text -- never the raw
/// value, which can carry userinfo or a credential-looking query
/// parameter from a misconfigured `base_url`/proxy: the same two
/// maskers [`crate::curl_text::curl_failure_message`] itself applies
/// to a successful request's own failure text, applied here too since
/// these errors happen *before* a [`CurlOutcome`] exists at all and
/// so never reach that function.
pub(crate) fn redact_url_for_error(url: &str) -> String {
    crate::curl_text::strip_url_userinfo(&crate::curl_text::mask_credential_query_params(url))
}

pub(crate) fn parse_url(url: &str) -> Result<ParsedUrl, TransportFailure> {
    // curl's own convention, same as a proxy value
    // ([`resolve_curl_proxy_from`]): a URL with no `scheme://` prefix
    // (a configured `base_url` of `localhost:11434`, say) defaults to
    // `http://` rather than being refused -- confirmed directly
    // against a real curl invocation with a schemeless `--config`
    // `url =` line.
    let normalized = if url.contains("://") {
        std::borrow::Cow::Borrowed(url)
    } else {
        std::borrow::Cow::Owned(format!("http://{url}"))
    };
    let (scheme, rest) = normalized
        .split_once("://")
        .expect("normalized always contains \"://\"");
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(TransportFailure::Validation(format!(
            "electricity: unsupported URL scheme '{scheme}'"
        )));
    }
    let split_at = rest.find(['/', '?']);
    let (authority, path_and_query) = match split_at {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    let target = if path_and_query.is_empty() {
        "/".to_string()
    } else if path_and_query.starts_with('?') {
        format!("/{path_and_query}")
    } else {
        path_and_query.to_string()
    };
    let (userinfo_raw, hostport) = match authority.rfind('@') {
        Some(idx) => (Some(&authority[..idx]), &authority[idx + 1..]),
        None => (None, authority),
    };
    let userinfo = userinfo_raw.map(|raw| match raw.split_once(':') {
        Some((user, pass)) => (user.to_string(), pass.to_string()),
        None => (raw.to_string(), String::new()),
    });
    let (host, port) = if let Some(rest) = hostport.strip_prefix('[') {
        match rest.split_once(']') {
            Some((addr, after)) => (
                addr.to_string(),
                after.strip_prefix(':').and_then(|p| p.parse::<u16>().ok()),
            ),
            None => (hostport.to_string(), None),
        }
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
                (h.to_string(), p.parse::<u16>().ok())
            }
            _ => (hostport.to_string(), None),
        }
    };
    if host.is_empty() {
        return Err(TransportFailure::Validation(format!(
            "electricity: unsupported URL: {}",
            redact_url_for_error(url)
        )));
    }
    Ok(ParsedUrl {
        scheme,
        userinfo,
        host,
        port,
        target,
    })
}

// ---------------------------------------------------------------------
// curl's own proxy-environment convention (issue #503's "Settled
// questions": lower-case-only `http_proxy`, curl ignores `HTTP_PROXY`;
// `https_proxy`/`HTTPS_PROXY`; `all_proxy`/`ALL_PROXY` as the
// fallback; `no_proxy`/`NO_PROXY`, `*` or comma-separated host
// suffixes) -- distinct from `electricity_tools::http_client::
// resolve_proxy`, which is urllib's own convention and stays the
// `http` tool's alone.
// ---------------------------------------------------------------------

fn env_first(env: &[(String, String)], names: &[&str]) -> Option<String> {
    for name in names {
        if let Some((_, value)) = env.iter().find(|(k, _)| k == name) {
            if !value.is_empty() {
                return Some(value.clone());
            }
        }
    }
    None
}

fn curl_no_proxy_bypasses(no_proxy: &str, host: &str) -> bool {
    if no_proxy.trim() == "*" {
        return true;
    }
    let host_lower = host.to_ascii_lowercase();
    no_proxy.split(',').any(|raw_entry| {
        let entry = raw_entry
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        !entry.is_empty() && (host_lower == entry || host_lower.ends_with(&format!(".{entry}")))
    })
}

/// curl's own proxy value for *scheme*/*host*, `None` when unset or
/// `no_proxy` bypasses *host* -- *env* is an explicit snapshot (this
/// lane's own environment-isolation rule: every caller that cares
/// about proxy behaviour sets or clears every `*_proxy`/`*_PROXY`
/// variable explicitly rather than relying on the real process
/// environment inside a test).
fn curl_proxy_value(scheme: &str, host: &str, env: &[(String, String)]) -> Option<String> {
    let no_proxy = env_first(env, &["no_proxy", "NO_PROXY"]).unwrap_or_default();
    if curl_no_proxy_bypasses(&no_proxy, host) {
        return None;
    }
    let scheme_specific = match scheme {
        "http" => env_first(env, &["http_proxy"]),
        "https" => env_first(env, &["https_proxy", "HTTPS_PROXY"]),
        _ => None,
    };
    scheme_specific.or_else(|| env_first(env, &["all_proxy", "ALL_PROXY"]))
}

/// The real process environment, read once per request -- the public
/// entry point every adapter call uses; [`curl_proxy_value`] above is
/// the pure, test-driven form.
pub(crate) fn resolve_curl_proxy(
    scheme: &str,
    host: &str,
) -> Result<Option<ProxyTarget>, TransportFailure> {
    let env: Vec<(String, String)> = std::env::vars().collect();
    resolve_curl_proxy_from(scheme, host, &env)
}

fn resolve_curl_proxy_from(
    scheme: &str,
    host: &str,
    env: &[(String, String)],
) -> Result<Option<ProxyTarget>, TransportFailure> {
    let Some(value) = curl_proxy_value(scheme, host, env) else {
        return Ok(None);
    };
    // curl's own convention: a proxy value with no `scheme://` prefix
    // (`http_proxy=proxy.corp:3128`) defaults to `http://`, same as a
    // value that spells it out.
    let value = if value.contains("://") {
        value
    } else {
        format!("http://{value}")
    };
    let proxy_scheme = value
        .split_once("://")
        .map(|(scheme, _)| scheme.to_ascii_lowercase())
        .unwrap_or_default();
    if proxy_scheme != "http" {
        return Err(TransportFailure::Validation(format!(
            "electricity: unsupported proxy scheme '{proxy_scheme}'; only http:// proxies are \
             supported"
        )));
    }
    let parsed = parse_url(&value)?;
    let port = parsed.port.unwrap_or_else(|| default_port(&parsed.scheme));
    let basic_auth = parsed.userinfo.map(|(user, password)| {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    });
    Ok(Some(ProxyTarget {
        host: parsed.host,
        port,
        basic_auth,
    }))
}

// ---------------------------------------------------------------------
// Body decoding: `run_tracked(text=True)`'s own strict UTF-8 decode
// with universal newlines.
// ---------------------------------------------------------------------

fn unicode_decode_error_text(bytes: &[u8], err: std::str::Utf8Error) -> String {
    let pos = err.valid_up_to();
    let byte = bytes[pos];
    let reason = match err.error_len() {
        None => "unexpected end of data",
        Some(_) if (0x80..=0xC1).contains(&byte) || byte >= 0xF5 => "invalid start byte",
        Some(_) => "invalid continuation byte",
    };
    format!("'utf-8' codec can't decode byte 0x{byte:02x} in position {pos}: {reason}")
}

/// `text=True`'s own universal-newline translation, applied *after* a
/// successful decode: `\r\n` and a lone `\r` both become `\n`.
fn translate_universal_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

fn decode_curl_text(bytes: &[u8]) -> Result<String, TransportFailure> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(translate_universal_newlines(text)),
        Err(err) => Err(TransportFailure::InvalidUtf8(unicode_decode_error_text(
            bytes, err,
        ))),
    }
}

// ---------------------------------------------------------------------
// curl's own exit-code mapping (issue #503's own table).
// ---------------------------------------------------------------------

fn connect_kind_exit(kind: ConnectKind) -> i32 {
    match kind {
        ConnectKind::Dns => 6,
        ConnectKind::Refused | ConnectKind::Other => 7,
        ConnectKind::Timeout => 28,
        ConnectKind::TlsHandshake => 35,
        ConnectKind::TlsCertificate => 60,
    }
}

fn send_error_outcome(err: SendError) -> CurlOutcome {
    let (code, text) = match err {
        SendError::Connect { kind, text } => (connect_kind_exit(kind), text),
        SendError::Timeout => (28, "Operation timed out".to_string()),
        SendError::RemoteDisconnected => (52, "Empty reply from server".to_string()),
        SendError::IncompleteRead { read, expected } => (
            18,
            format!("transfer closed with {expected} bytes remaining to read (got {read})"),
        ),
        SendError::Other(text) => (2, text),
    };
    CurlOutcome {
        returncode: code,
        stdout: String::new(),
        stderr: format!("curl: ({code}) {text}"),
        status: None,
        retry_after: None,
    }
}

/// A body-phase failure recorded on an otherwise-successful response
/// ([`http_client::RawResponse::body_error`]) -- always a failure
/// that happened *after* the response began, so it maps to curl's own
/// "after the response began" exits (56/18), never 52 (issue #503's
/// own table: 52 is "closed before any response", only ever reachable
/// as the outer [`SendError`] above, never through this path).
///
/// *partial_body* ([`http_client::RawResponse::partial_body`]) is
/// never empty on this path unless the connection genuinely dropped
/// before a single byte of the body arrived -- real curl keeps
/// writing to stdout as bytes arrive, so whatever it received before
/// the connection died is still there when it exits nonzero (issue
/// #515: the prior version of this function always
/// discarded it, matching `http_client`'s own urllib-parity clearing
/// of [`http_client::RawResponse::body`] instead -- correct for the
/// `http` *tool*, wrong for a curl-shelling adapter). A partial
/// multi-byte UTF-8 sequence cut off mid-codepoint is decoded lossily
/// here rather than raised as its own failure: it is only ever used
/// as an error message's own *detail* text (`curl_text::
/// parse_error_body`), never parsed as the success path's JSON.
fn body_error_outcome(err: SendError, partial_body: &[u8]) -> CurlOutcome {
    let (code, text) = match err {
        SendError::IncompleteRead { read, expected } => (
            18,
            format!("transfer closed with {expected} bytes remaining to read (got {read})"),
        ),
        SendError::RemoteDisconnected => (56, "Recv failure: Connection reset by peer".to_string()),
        SendError::Timeout => (28, "Operation timed out".to_string()),
        SendError::Connect { kind, text } => (connect_kind_exit(kind), text),
        SendError::Other(text) => (2, text),
    };
    CurlOutcome {
        returncode: code,
        stdout: translate_universal_newlines(&String::from_utf8_lossy(partial_body)),
        stderr: format!("curl: ({code}) {text}"),
        status: None,
        retry_after: None,
    }
}

/// curl exit codes a failed request can retry on (`adapters/_retry.py::
/// RETRYABLE_CURL_EXIT_CODES`) -- shared here so every adapter's own
/// retry-info construction reads the same set this module's own exit
/// mapping produces, rather than re-listing it.
pub fn is_retryable_exit(returncode: i32) -> bool {
    matches!(returncode, 6 | 7 | 28 | 35 | 52 | 56)
}

// ---------------------------------------------------------------------
// The shared request path.
// ---------------------------------------------------------------------

/// One header electricity itself adds, title-cased the way curl's own
/// request line is (the mock harness compares headers by name,
/// case-insensitively, so casing on the wire is cosmetic only).
pub(crate) fn host_header_value(host: &str, port: u16, scheme: &str) -> String {
    if port == default_port(scheme) {
        host.to_string()
    } else {
        format!("{host}:{port}")
    }
}

/// Sends one curl-equivalent request: *url* and *headers* are exactly
/// what an adapter would have handed `curl_support.run_curl` (the
/// adapter's own headers, Bearer/API-key included -- never `Host`/
/// `Accept`/`Content-Length`/`User-Agent`, which this function adds
/// itself, matching curl's own behaviour); *body*, when present,
/// implies `POST` the same way curl's own `--data-binary` does (curl
/// has no explicit `-X` from any caller in this crate: every request
/// either carries a body and is a `POST`, or carries none and is a
/// `GET`). *timeout_seconds* is curl's own `--max-time`, one total
/// deadline; `0` means no limit.
pub async fn run_curl(
    url: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    timeout_seconds: u32,
) -> Result<CurlOutcome, TransportFailure> {
    validate_request(url, headers)?;
    let parsed = parse_url(url)?;
    let port = parsed.port.unwrap_or_else(|| default_port(&parsed.scheme));
    let proxy = resolve_curl_proxy(&parsed.scheme, &parsed.host)?;
    let scheme_enum = if parsed.scheme == "https" {
        Scheme::Https
    } else {
        Scheme::Http
    };

    let mut wire_headers: Vec<(String, String)> = Vec::new();
    wire_headers.push((
        "Host".to_string(),
        host_header_value(&parsed.host, port, &parsed.scheme),
    ));
    wire_headers.push(("Accept".to_string(), "*/*".to_string()));
    let has_authorization = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("authorization"));
    if !has_authorization {
        if let Some((user, password)) = &parsed.userinfo {
            use base64::Engine;
            let creds =
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
            wire_headers.push(("Authorization".to_string(), format!("Basic {creds}")));
        }
    }
    for (name, value) in headers {
        wire_headers.push((name.clone(), value.clone()));
    }
    if let Some(body) = body {
        wire_headers.push(("Content-Length".to_string(), body.len().to_string()));
    }
    wire_headers.push((
        "User-Agent".to_string(),
        format!("electricity/{}", env!("CARGO_PKG_VERSION")),
    ));

    let wire_target = match (&proxy, scheme_enum) {
        (Some(_), Scheme::Http) => {
            let host_display = host_header_value(&parsed.host, port, &parsed.scheme);
            format!("{}://{}{}", parsed.scheme, host_display, parsed.target)
        }
        _ => parsed.target.clone(),
    };

    let connect_target = ConnectTarget {
        scheme: scheme_enum,
        host: parsed.host.clone(),
        port,
        proxy,
    };
    let raw_request = RawRequest {
        method: if body.is_some() { "POST" } else { "GET" }.to_string(),
        target: wire_target,
        headers: wire_headers,
        body: body.map(|b| b.as_bytes().to_vec()),
    };

    let client = build_client()?;
    let duration = if timeout_seconds == 0 {
        Duration::from_secs(u32::MAX as u64)
    } else {
        Duration::from_secs(u64::from(timeout_seconds))
    };
    let timeout = TimeoutModel::Total(duration);

    match http_client::send(&client, &connect_target, &raw_request, timeout).await {
        Err(err) => Ok(send_error_outcome(err)),
        Ok(response) => {
            let retry_after = response
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
                .map(|(_, value)| value.clone());
            if let Some(err) = response.body_error {
                let mut outcome = body_error_outcome(err, &response.partial_body);
                outcome.retry_after = retry_after;
                return Ok(outcome);
            }
            let stdout = decode_curl_text(&response.body)?;
            if response.status >= 400 {
                Ok(CurlOutcome {
                    returncode: 22,
                    stdout,
                    stderr: format!(
                        "curl: (22) The requested URL returned error: {}",
                        response.status
                    ),
                    status: Some(response.status),
                    retry_after,
                })
            } else {
                Ok(CurlOutcome {
                    returncode: 0,
                    stdout,
                    stderr: String::new(),
                    status: None,
                    retry_after,
                })
            }
        }
    }
}

fn build_client() -> Result<HttpClient, TransportFailure> {
    http_client::build_client().map_err(TransportFailure::Validation)
}

/// Ollama's own `check()` probe (`adapters/ollama.py:220-244`):
/// `HEAD {base_url}/api/tags` run through the real `curl` binary,
/// `-q --silent --max-time 2 --head` -- which, with no explicit
/// `--proxy`/`-u` of its own, still honors the proxy environment and
/// a URL's own userinfo exactly as any other curl invocation does.
/// This port reproduces both rather than special-casing the probe:
/// one 2-second total deadline, headers `Host`/`Accept: */*` plus a
/// `Basic` `Authorization` from URL userinfo when present (same as
/// [`run_curl`]), through the same proxy resolution. **Any** HTTP
/// response (status included -- no `--fail`) means reachable; `false`
/// for a connection-level failure, a refused proxy scheme, or the
/// timeout.
pub async fn head_probe(url: &str, timeout: Duration) -> bool {
    let Ok(parsed) = parse_url(url) else {
        return false;
    };
    let port = parsed.port.unwrap_or_else(|| default_port(&parsed.scheme));
    let scheme_enum = if parsed.scheme == "https" {
        Scheme::Https
    } else {
        Scheme::Http
    };
    let Ok(proxy) = resolve_curl_proxy(&parsed.scheme, &parsed.host) else {
        return false;
    };
    let Ok(client) = http_client::build_client() else {
        return false;
    };
    let host_display = host_header_value(&parsed.host, port, &parsed.scheme);
    let target = match (&proxy, scheme_enum) {
        (Some(_), Scheme::Http) => {
            format!("{}://{}{}", parsed.scheme, host_display, parsed.target)
        }
        _ => parsed.target.clone(),
    };
    let connect_target = ConnectTarget {
        scheme: scheme_enum,
        host: parsed.host.clone(),
        port,
        proxy,
    };
    let mut headers = vec![
        ("Host".to_string(), host_display),
        ("Accept".to_string(), "*/*".to_string()),
    ];
    if let Some((user, password)) = &parsed.userinfo {
        use base64::Engine;
        let creds = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
        headers.push(("Authorization".to_string(), format!("Basic {creds}")));
    }
    let raw_request = RawRequest {
        method: "HEAD".to_string(),
        target,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn spawn_server(response: &'static str) -> std::net::SocketAddr {
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
    async fn a_successful_json_response_decodes_with_returncode_zero() {
        let addr =
            spawn_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        let url = format!("http://{addr}/path");
        let outcome = run_curl(&url, &[("X-Test".to_string(), "1".to_string())], None, 5)
            .await
            .unwrap();
        assert_eq!(outcome.returncode, 0);
        assert_eq!(outcome.stdout, "ok");
        assert_eq!(outcome.stderr, "");
    }

    #[tokio::test]
    async fn the_exact_header_set_is_sent_with_no_expect_or_accept_encoding() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 65536];
            let n = stream.read(&mut buf).unwrap();
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            text
        });
        let url = format!("http://{addr}/chat");
        run_curl(
            &url,
            &[("Authorization".to_string(), "Bearer t".to_string())],
            Some("{}"),
            5,
        )
        .await
        .unwrap();
        let request_text = handle.join().unwrap();
        let lower = request_text.to_ascii_lowercase();
        assert!(lower.contains("host:"));
        assert!(lower.contains("accept: */*"));
        assert!(lower.contains("content-length: 2"));
        assert!(lower.contains("authorization: bearer t"));
        assert!(!lower.contains("expect:"));
        assert!(!lower.contains("accept-encoding"));
    }

    #[tokio::test]
    async fn a_scheme_less_request_url_defaults_to_http_like_curl() {
        let addr =
            spawn_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        let url = format!("{addr}/path");
        let outcome = run_curl(&url, &[], None, 5).await.unwrap();
        assert_eq!(outcome.returncode, 0);
        assert_eq!(outcome.stdout, "ok");
    }

    #[tokio::test]
    async fn request_url_userinfo_becomes_a_basic_auth_header_like_curl() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 65536];
            let n = stream.read(&mut buf).unwrap();
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            text
        });
        let url = format!("http://user:pass@{addr}/x");
        run_curl(&url, &[], None, 5).await.unwrap();
        let request_text = handle.join().unwrap();
        let lower = request_text.to_ascii_lowercase();
        assert!(lower.contains("authorization: basic dxnlcjpwyxnz"));
    }

    #[tokio::test]
    async fn request_url_userinfo_never_overrides_an_explicit_authorization_header() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 65536];
            let n = stream.read(&mut buf).unwrap();
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            text
        });
        let url = format!("http://user:pass@{addr}/x");
        run_curl(
            &url,
            &[("Authorization".to_string(), "Bearer t".to_string())],
            None,
            5,
        )
        .await
        .unwrap();
        let request_text = handle.join().unwrap();
        let lower = request_text.to_ascii_lowercase();
        assert!(lower.contains("authorization: bearer t"));
        assert!(!lower.contains("basic"));
    }

    #[tokio::test]
    async fn a_connection_refused_maps_to_exit_7_and_is_retryable() {
        let addr = crate::test_support::closed_port();
        let url = format!("http://{addr}/x");
        let outcome = run_curl(&url, &[], None, 5).await.unwrap();
        assert_eq!(outcome.returncode, 7);
        assert!(is_retryable_exit(outcome.returncode));
    }

    #[tokio::test]
    async fn a_closed_connection_before_any_response_maps_to_exit_52() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            drop(stream);
        });
        let url = format!("http://{addr}/x");
        let outcome = run_curl(&url, &[], None, 5).await.unwrap();
        assert_eq!(outcome.returncode, 52);
        assert!(is_retryable_exit(52));
    }

    #[tokio::test]
    async fn a_reset_mid_body_maps_to_exit_56() {
        // No `Content-Length`: a chunked body with no `Content-Length`
        // header is the one shape that reaches `SendError::
        // RemoteDisconnected` during the body phase (`classify_body_
        // phase_error`'s own `content_length: None` branch) -- a
        // `Content-Length`-framed short body (below) is `IncompleteRead`
        // (exit 18) instead, a different curl exit this test must not
        // collide with.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nHe",
            );
        });
        let url = format!("http://{addr}/x");
        let outcome = run_curl(&url, &[], None, 5).await.unwrap();
        assert_eq!(outcome.returncode, 56);
        assert!(is_retryable_exit(56));
    }

    #[tokio::test]
    async fn a_short_body_against_content_length_maps_to_exit_18() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n12345",
            );
        });
        let url = format!("http://{addr}/x");
        let outcome = run_curl(&url, &[], None, 5).await.unwrap();
        assert_eq!(outcome.returncode, 18);
        // Issue #515: real curl keeps whatever it already wrote
        // to stdout before the connection died -- this must not come back
        // empty the way `http_client`'s own urllib-parity clearing (for
        // the `http` tool) would otherwise make it.
        assert_eq!(outcome.stdout, "12345");
    }

    #[tokio::test]
    async fn a_timeout_maps_to_exit_28_and_is_retryable() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            std::thread::sleep(Duration::from_secs(2));
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n");
        });
        let url = format!("http://{addr}/x");
        // `timeout_seconds` is whole seconds; 1 is the smallest that
        // still lets the test run fast while the server sleeps 2s.
        let outcome = run_curl(&url, &[], None, 1).await.unwrap();
        assert_eq!(outcome.returncode, 28);
        assert!(is_retryable_exit(28));
    }

    #[tokio::test]
    async fn a_4xx_status_maps_to_exit_22_with_the_body_as_stdout() {
        let addr = spawn_server(
            "HTTP/1.1 404 Not Found\r\nContent-Length: 20\r\nConnection: close\r\n\r\n\
             {\"error\": \"missing\"}",
        );
        let url = format!("http://{addr}/x");
        let outcome = run_curl(&url, &[], None, 5).await.unwrap();
        assert_eq!(outcome.returncode, 22);
        assert_eq!(outcome.stdout, "{\"error\": \"missing\"}");
        assert_eq!(
            outcome.stderr,
            "curl: (22) The requested URL returned error: 404"
        );
        assert!(!is_retryable_exit(22));
    }

    #[tokio::test]
    async fn a_5xx_status_is_still_retryable_through_exit_22() {
        let addr = spawn_server("HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n");
        let url = format!("http://{addr}/x");
        let outcome = run_curl(&url, &[], None, 5).await.unwrap();
        assert_eq!(outcome.returncode, 22);
        // 22 alone doesn't say whether it's retryable -- that's read
        // from the status by each adapter's own call into
        // `electricity_vm::retry::status_is_retryable`, not by this
        // module (curl_text.rs is where that happens).
        assert_eq!(
            outcome.stderr,
            "curl: (22) The requested URL returned error: 503"
        );
    }

    #[tokio::test]
    async fn an_invalid_utf8_body_raises_rather_than_returning_an_outcome() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let mut response =
                b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n".to_vec();
            response.push(0xFF);
            let _ = stream.write_all(&response);
        });
        let url = format!("http://{addr}/x");
        let err = run_curl(&url, &[], None, 5).await.unwrap_err();
        match err {
            TransportFailure::InvalidUtf8(text) => {
                assert_eq!(
                    text,
                    "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte"
                );
            }
            other => panic!("expected InvalidUtf8, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn crlf_and_lone_cr_bodies_both_translate_to_lone_lf() {
        let addr = spawn_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\na\r\nb\rc",
        );
        let url = format!("http://{addr}/x");
        let outcome = run_curl(&url, &[], None, 5).await.unwrap();
        assert_eq!(outcome.stdout, "a\nb\nc");
    }

    #[test]
    fn a_control_character_in_a_header_value_is_refused_before_any_io() {
        let err = validate_request(
            "http://example.com/",
            &[("X".to_string(), "a\nb".to_string())],
        )
        .unwrap_err();
        assert_eq!(
            err,
            TransportFailure::Validation(
                "header value contains a forbidden control character".to_string()
            )
        );
    }

    #[test]
    fn an_oversized_config_is_refused_before_any_io() {
        let huge = "x".repeat(70_000);
        let err = validate_request("http://example.com/", &[("X".to_string(), huge)]).unwrap_err();
        match err {
            TransportFailure::Validation(text) => {
                assert!(text.contains("over the 60000-byte limit"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn curl_proxy_convention_ignores_uppercase_http_proxy() {
        let env = vec![("HTTP_PROXY".to_string(), "http://proxy:1".to_string())];
        assert_eq!(curl_proxy_value("http", "example.com", &env), None);
    }

    #[test]
    fn curl_proxy_convention_accepts_lowercase_http_proxy() {
        let env = vec![("http_proxy".to_string(), "http://proxy:1".to_string())];
        assert_eq!(
            curl_proxy_value("http", "example.com", &env),
            Some("http://proxy:1".to_string())
        );
    }

    #[test]
    fn curl_proxy_convention_accepts_either_case_for_https() {
        let env = vec![("HTTPS_PROXY".to_string(), "http://proxy:1".to_string())];
        assert_eq!(
            curl_proxy_value("https", "example.com", &env),
            Some("http://proxy:1".to_string())
        );
    }

    #[test]
    fn curl_proxy_convention_falls_back_to_all_proxy() {
        let env = vec![("all_proxy".to_string(), "http://proxy:2".to_string())];
        assert_eq!(
            curl_proxy_value("https", "example.com", &env),
            Some("http://proxy:2".to_string())
        );
    }

    #[test]
    fn curl_no_proxy_star_bypasses_everything() {
        let env = vec![
            ("http_proxy".to_string(), "http://proxy:1".to_string()),
            ("no_proxy".to_string(), "*".to_string()),
        ];
        assert_eq!(curl_proxy_value("http", "example.com", &env), None);
    }

    #[test]
    fn curl_no_proxy_matches_a_suffix() {
        let env = vec![
            ("http_proxy".to_string(), "http://proxy:1".to_string()),
            ("no_proxy".to_string(), "example.com".to_string()),
        ];
        assert_eq!(curl_proxy_value("http", "api.example.com", &env), None);
        assert_eq!(
            curl_proxy_value("http", "other.com", &env),
            Some("http://proxy:1".to_string())
        );
    }

    #[test]
    fn a_non_http_proxy_scheme_is_refused_before_any_io() {
        let env = vec![("http_proxy".to_string(), "socks5://proxy:1".to_string())];
        let err = resolve_curl_proxy_from("http", "example.com", &env).unwrap_err();
        match err {
            TransportFailure::Validation(text) => {
                assert!(text.contains("unsupported proxy scheme"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn a_non_http_proxy_scheme_refusal_never_names_the_credential() {
        let env = vec![(
            "all_proxy".to_string(),
            "socks5://user:s3cr3t@proxy:1080".to_string(),
        )];
        let err = resolve_curl_proxy_from("http", "example.com", &env).unwrap_err();
        match err {
            TransportFailure::Validation(text) => {
                assert!(text.contains("unsupported proxy scheme 'socks5'"));
                assert!(!text.contains("s3cr3t"));
                assert!(!text.contains("user"));
                assert!(!text.contains("proxy:1080"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn a_scheme_less_proxy_value_defaults_to_http_like_curl() {
        let env = vec![("http_proxy".to_string(), "proxy.corp:3128".to_string())];
        let target = resolve_curl_proxy_from("http", "example.com", &env)
            .unwrap()
            .unwrap();
        assert_eq!(target.host, "proxy.corp");
        assert_eq!(target.port, 3128);
    }

    #[test]
    fn an_unsupported_url_scheme_refusal_never_names_the_credential() {
        match parse_url("gopher://user:s3cr3t@example.com/path?api_key=sk-test") {
            Err(TransportFailure::Validation(text)) => {
                assert!(text.contains("unsupported URL scheme 'gopher'"));
                assert!(!text.contains("s3cr3t"));
                assert!(!text.contains("sk-test"));
            }
            other => panic!("expected a Validation error, got {}", matches_label(&other)),
        }
    }

    #[test]
    fn an_unsupported_url_refusal_masks_userinfo_and_credential_query_params() {
        match parse_url("https://?api_key=sk-test") {
            Err(TransportFailure::Validation(text)) => {
                assert!(!text.contains("sk-test"));
                assert!(text.contains("unsupported URL"));
            }
            other => panic!("expected a Validation error, got {}", matches_label(&other)),
        }
        match parse_url("https://user:s3cr3t@/path") {
            Err(TransportFailure::Validation(text)) => {
                assert!(!text.contains("s3cr3t"));
                assert!(!text.contains("user"));
            }
            other => panic!("expected a Validation error, got {}", matches_label(&other)),
        }
    }

    fn matches_label(result: &Result<ParsedUrl, TransportFailure>) -> &'static str {
        match result {
            Ok(_) => "Ok",
            Err(TransportFailure::Validation(_)) => "Validation",
            Err(TransportFailure::InvalidUtf8(_)) => "InvalidUtf8",
        }
    }

    #[test]
    fn url_userinfo_becomes_a_basic_auth_proxy_credential() {
        let env = vec![(
            "http_proxy".to_string(),
            "http://user:pass@proxy:3128".to_string(),
        )];
        let target = resolve_curl_proxy_from("http", "example.com", &env)
            .unwrap()
            .unwrap();
        assert_eq!(target.host, "proxy");
        assert_eq!(target.port, 3128);
        assert_eq!(target.basic_auth, Some("dXNlcjpwYXNz".to_string()));
    }

    #[tokio::test]
    async fn head_probe_succeeds_on_any_response_status() {
        let addr = spawn_server(
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let url = format!("http://{addr}/api/tags");
        assert!(head_probe(&url, Duration::from_secs(2)).await);
    }

    #[tokio::test]
    async fn head_probe_fails_on_a_closed_port() {
        let addr = crate::test_support::closed_port();
        let url = format!("http://{addr}/api/tags");
        assert!(!head_probe(&url, Duration::from_secs(2)).await);
    }

    #[tokio::test]
    async fn head_probe_accepts_a_scheme_less_base_url_like_curl() {
        let addr =
            spawn_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let url = format!("{addr}/api/tags");
        assert!(head_probe(&url, Duration::from_secs(2)).await);
    }

    #[tokio::test]
    async fn head_probe_sends_basic_auth_from_url_userinfo() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 65536];
            let n = stream.read(&mut buf).unwrap();
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            text
        });
        let url = format!("http://user:pass@{addr}/api/tags");
        assert!(head_probe(&url, Duration::from_secs(2)).await);
        let request_text = handle.join().unwrap();
        assert!(
            request_text
                .to_ascii_lowercase()
                .contains("authorization: basic dxnlcjpwyxnz")
        );
    }
}
