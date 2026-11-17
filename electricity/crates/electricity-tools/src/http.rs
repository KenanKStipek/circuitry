//! `plugins/http.py::HttpPlugin`, ported -- a single HTTP request/
//! response over the shared client ([`crate::http_client`]), with
//! urllib's own redirect rule and error shapes (issue #485, lane E).
//! The proxy convention is Circuitry's own now (issue #778), not
//! urllib's (`http_client::resolve_proxy`/`resolve_proxy_target`
//! below). See [`http_url`] for the small `urllib.parse` subset
//! (`urlsplit`/`urljoin`/`quote`) this file's own redirect handling
//! needs.

use crate::http_client::{self, ConnectTarget, RawRequest, Scheme, SendError, TimeoutModel};
use crate::{CheckResult, ErrorCause, PyExc, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::pycompat::py_strip;
use electricity_value::{Dict, Value};
use indexmap::IndexMap;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn get<'a>(params: &'a Value, key: &str) -> Option<&'a Value> {
    params.as_dict()?.get(&Value::Str(key.to_string()))
}

fn value_error(message: impl Into<String>) -> ToolError {
    ToolError {
        py_class: PyExc::ValueError,
        message: message.into(),
        cause: ErrorCause::None,
    }
}

fn runtime_error(message: impl Into<String>) -> ToolError {
    ToolError {
        py_class: PyExc::RuntimeError,
        message: message.into(),
        cause: ErrorCause::None,
    }
}

/// [`SendError`]'s own raise shape (issue #485's own "Raised
/// failures" section) -- shared between a hard failure from
/// [`http_client::send`] itself and a deferred
/// [`http_client::RawResponse::body_error`] a 2xx has no handler to
/// swallow.
fn send_error_to_tool_error(err: SendError, original_url: &str) -> ToolError {
    match err {
        SendError::Connect { text, .. } => ToolError {
            py_class: PyExc::RuntimeError,
            message: format!(
                "HTTP request to {} failed: {text}",
                crate::http_conventions::redact_request_url(original_url)
            ),
            cause: ErrorCause::NoReply,
        },
        SendError::Timeout => ToolError {
            py_class: PyExc::TimeoutError,
            message: "timed out".to_string(),
            cause: ErrorCause::NoReply,
        },
        SendError::RemoteDisconnected => ToolError {
            py_class: PyExc::RemoteDisconnected,
            message: "Remote end closed connection without response".to_string(),
            cause: ErrorCause::NoReply,
        },
        SendError::IncompleteRead { read, expected } => ToolError {
            py_class: PyExc::IncompleteRead,
            message: format!("IncompleteRead({read} bytes read, {expected} more expected)"),
            cause: ErrorCause::None,
        },
        SendError::Other(text) => runtime_error(text),
        // Never actually reached through this function: `save_to`'s own
        // dispatch (the only caller that ever builds a `BodySink`)
        // intercepts both of these itself, with `plugins/http.py::
        // _save_body`'s own exact text. Kept here only so this match
        // stays exhaustive.
        SendError::BodyOverCap { declared } => runtime_error(match declared {
            Some(n) => format!("the response body is {n} bytes, over the cap"),
            None => "the response body is over the cap".to_string(),
        }),
        SendError::Sink(text) => runtime_error(text),
    }
}

/// A scheme other than `http`/`https` (issue #778) -- the
/// initial URL or a redirect target alike -- is refused before any I/O,
/// with Circuitry's own text ([`crate::http_conventions::check_scheme`]),
/// never retried (`ErrorCause::None`): `cof run` itself now refuses the
/// same way, before ever reaching urllib's own `FTPHandler`/`FileHandler`/
/// `DataHandler` or its `unknown_open`'s `URLError`.
fn scheme_refused(scheme: &str, original_url: &str) -> ToolError {
    let message = crate::http_conventions::check_scheme(scheme, original_url, &["http", "https"])
        .expect_err("scheme_refused is only called once check_scheme has already failed");
    ToolError {
        py_class: PyExc::ValueError,
        message,
        cause: ErrorCause::None,
    }
}

/// `http_conventions.check_url` (issue #778) -- *url*'s scheme (RFC 3986's own grammar via
/// [`http_url::urlsplit`], never [`splittype`]'s looser, urllib-parse-
/// time-only one) must be `http`/`https`, and its port must pass
/// [`crate::http_conventions::strict_port`]. Called on *url* before
/// [`parse_params`] -- before `save_to`'s own filesystem checks, and
/// before `params['params']` is appended -- the same order and the
/// same shared text `plugins/http.py::HttpPlugin.execute`'s own
/// `http_conventions.check_url(url)` call uses, never `Request`'s own
/// "unknown url type" text.
fn check_url(url: &str) -> Result<(), ToolError> {
    let split = http_url::urlsplit(url, "");
    crate::http_conventions::check_scheme(&split.scheme, url, &["http", "https"])
        .map_err(value_error)?;
    crate::http_conventions::strict_port(&split.netloc, url).map_err(value_error)?;
    Ok(())
}

/// `HTTPRedirectHandler.http_error_302`'s own `inf_msg + msg` (the two
/// loop limits, `max_repeats`/`max_redirections`) -- pinned against
/// real CPython in `generate_http_corpus.py`'s own
/// `redirect_reason_cases` (issue #485), since this file never
/// actually raises through a real `HTTPRedirectHandler` to get the
/// text from CPython itself.
pub fn redirect_loop_reason(msg: &str) -> String {
    format!(
        "The HTTP server returned a redirect error that would lead to an infinite loop.\nThe last 30x error message was:\n{msg}"
    )
}

// ---------------------------------------------------------------------
// `urllib.parse`'s own subset: `urlsplit`/`urljoin`/`quote`.
// ---------------------------------------------------------------------

/// `urllib.parse.urlsplit`/`urljoin`/`quote`, narrowed to exactly what
/// `HttpPlugin` needs (query-string construction aside -- that's
/// `electricity_value::pycompat::urlencode`'s own job): scheme/netloc/
/// path/query/fragment splitting, RFC 3986 §5.3 relative-reference
/// resolution for a `Location`/`URI` redirect header, and the one
/// `quote(..., safe=string.punctuation)` call `HTTPRedirectHandler`
/// itself applies to a resolved redirect URL before following it.
/// Deliberately not the general-purpose `url` crate: that crate's own
/// WHATWG parser normalizes host/path differently than CPython's
/// `urllib.parse` does in several places this plugin has to match
/// exactly (IDNA, percent-encoding an already-partially-encoded path,
/// a `data:`/non-`http` scheme's own opaque-path handling) -- a hand
/// port of the same, narrow RFC 3986 algorithm CPython itself runs
/// stays predictable where the two would otherwise diverge.
pub mod http_url {
    /// A `urllib.parse.SplitResult` -- *netloc* keeps any userinfo
    /// (`user:pass@host:port`) verbatim; this plugin never uses it for
    /// authentication (plain `urlopen()` doesn't either, with no
    /// custom `HTTPBasicAuthHandler` installed), only for the request
    /// text redaction already handles like any other credential-
    /// shaped string.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Split {
        pub scheme: String,
        pub netloc: String,
        pub path: String,
        pub query: String,
        pub fragment: String,
    }

    fn is_scheme_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
    }

    /// `urlsplit(url, scheme=default_scheme)` -- *default_scheme* is
    /// only ever used by [`urljoin`] (the relative reference's own
    /// missing-scheme default, `urlparse(url, bscheme)`'s own second
    /// argument); every other caller passes `""`.
    pub fn urlsplit(url: &str, default_scheme: &str) -> Split {
        let mut rest = url;
        let mut scheme = String::new();
        if let Some(colon) = rest.find(':') {
            let candidate = &rest[..colon];
            let mut chars = candidate.chars();
            let valid = match chars.next() {
                Some(first) if first.is_ascii_alphabetic() => chars.all(is_scheme_char),
                _ => false,
            };
            if valid {
                scheme = candidate.to_ascii_lowercase();
                rest = &rest[colon + 1..];
            }
        }
        if scheme.is_empty() {
            scheme = default_scheme.to_string();
        }
        let mut netloc = String::new();
        if let Some(after) = rest.strip_prefix("//") {
            let end = after.find(['/', '?', '#']).unwrap_or(after.len());
            netloc = after[..end].to_string();
            rest = &after[end..];
        }
        let mut fragment = String::new();
        if let Some(idx) = rest.find('#') {
            fragment = rest[idx + 1..].to_string();
            rest = &rest[..idx];
        }
        let mut query = String::new();
        if let Some(idx) = rest.find('?') {
            query = rest[idx + 1..].to_string();
            rest = &rest[..idx];
        }
        Split {
            scheme,
            netloc,
            path: rest.to_string(),
            query,
            fragment,
        }
    }

    pub fn urlunsplit(s: &Split) -> String {
        let mut out = String::new();
        if !s.scheme.is_empty() {
            out.push_str(&s.scheme);
            out.push(':');
        }
        if !s.netloc.is_empty() || !s.scheme.is_empty() {
            out.push_str("//");
            out.push_str(&s.netloc);
            if !s.path.is_empty() && !s.path.starts_with('/') {
                out.push('/');
            }
        }
        out.push_str(&s.path);
        if !s.query.is_empty() {
            out.push('?');
            out.push_str(&s.query);
        }
        if !s.fragment.is_empty() {
            out.push('#');
            out.push_str(&s.fragment);
        }
        out
    }

    const RELATIVE_SCHEMES: &[&str] = &["", "ftp", "http", "https"];

    /// `urllib.parse.urljoin` -- RFC 3986 §5.3's own merge algorithm,
    /// narrowed to the schemes [`RELATIVE_SCHEMES`] lists (every
    /// scheme this plugin's own redirect handling ever resolves
    /// against; CPython's own `uses_relative`/`uses_netloc` tables are
    /// wider, but agree with this list on every one of them).
    pub fn urljoin(base: &str, url: &str) -> String {
        if base.is_empty() {
            return url.to_string();
        }
        if url.is_empty() {
            return base.to_string();
        }
        let b = urlsplit(base, "");
        let mut u = urlsplit(url, &b.scheme);
        if u.scheme != b.scheme || !RELATIVE_SCHEMES.contains(&u.scheme.as_str()) {
            return url.to_string();
        }
        if !u.netloc.is_empty() {
            return urlunsplit(&u);
        }
        u.netloc = b.netloc.clone();
        if u.path.is_empty() {
            u.path = b.path.clone();
            if u.query.is_empty() {
                u.query = b.query.clone();
            }
            return urlunsplit(&u);
        }

        let mut base_parts: Vec<&str> = b.path.split('/').collect();
        if base_parts.last() != Some(&"") {
            base_parts.pop();
        }
        let mut segments: Vec<String> = if u.path.starts_with('/') {
            u.path.split('/').map(str::to_string).collect()
        } else {
            let mut combined: Vec<String> = base_parts.iter().map(|s| s.to_string()).collect();
            combined.extend(u.path.split('/').map(str::to_string));
            // `segments[1:-1] = filter(None, segments[1:-1])`: an empty
            // interior segment from a doubled `/` collapses away,
            // the first and last segment kept even if empty (the
            // leading empty segment marks an absolute path; a
            // trailing one marks a directory).
            if combined.len() > 2 {
                let last = combined.pop().unwrap();
                let first = combined.remove(0);
                combined.retain(|s| !s.is_empty());
                combined.insert(0, first);
                combined.push(last);
            }
            combined
        };

        let last_is_dot_or_dotdot =
            matches!(segments.last().map(String::as_str), Some(".") | Some(".."));
        let mut resolved: Vec<String> = Vec::new();
        for seg in segments.drain(..) {
            if seg == ".." {
                resolved.pop();
            } else if seg == "." {
                continue;
            } else {
                resolved.push(seg);
            }
        }
        if last_is_dot_or_dotdot {
            resolved.push(String::new());
        }
        let joined_path = resolved.join("/");
        u.path = if joined_path.is_empty() {
            "/".to_string()
        } else {
            joined_path
        };
        urlunsplit(&u)
    }

    const ALWAYS_SAFE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.-~";

    /// `urllib.parse.quote(s, safe=safe)`, iso-8859-1 input: each
    /// `char`'s own codepoint (0-255 only, this plugin's own values
    /// are always ASCII Location headers in practice) is the single
    /// byte `quote` percent-encodes; unlike [`super::super::quote_plus_wire`]
    /// this never turns a space into `+`.
    pub fn quote(s: &str, extra_safe: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            let code = c as u32;
            if code < 0x80 && (ALWAYS_SAFE.contains(c) || extra_safe.contains(c)) {
                out.push(c);
            } else if code <= 0xFF {
                out.push_str(&format!("%{code:02X}"));
            } else {
                for byte in c.to_string().as_bytes() {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
        }
        out
    }

    /// `string.punctuation` -- the extra `safe` set
    /// `HTTPRedirectHandler`'s own `quote(newurl, safe=string.punctuation)`
    /// call uses when re-quoting a resolved redirect URL.
    pub const STRING_PUNCTUATION: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

    /// `urllib.parse.unquote(s, errors='replace')` -- percent-decodes
    /// *s* as UTF-8 bytes, an invalid sequence replaced the same way
    /// `bytes.decode('utf-8', 'replace')` already is elsewhere in this
    /// crate. [`super::resolve_proxy_target`]'s own `_parse_proxy`
    /// port is the only caller: a proxy URL's userinfo and hostport
    /// are both `unquote`-d before use (`ProxyHandler.proxy_open`).
    /// Not corpus-pinned against CPython (no conformance case reaches
    /// a percent-encoded proxy credential); a reasonable, documented
    /// port, not a byte-for-byte replication of `unquote`'s own
    /// surrogate-escape edge cases.
    pub fn unquote(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                    if let Ok(byte) = u8::from_str_radix(hex, 16) {
                        out.push(byte);
                        i += 3;
                        continue;
                    }
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

// ---------------------------------------------------------------------
// `urllib.request.Request.add_header`'s own capitalize-collapse.
// ---------------------------------------------------------------------

/// `str.capitalize()` -- the first codepoint uppercased, every other
/// codepoint lowercased (not title-case: `"content-type".capitalize()`
/// is `"Content-type"`, lower-case `t`) -- `Request.add_header`'s own
/// storage key, which is why a `Content-Type` a `json` body's own
/// `setdefault` adds collides with a user `content-type` header into
/// one case-collapsed key.
fn python_capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => {
            let mut out: String = first.to_uppercase().collect();
            out.extend(chars.flat_map(char::to_lowercase));
            out
        }
    }
}

/// `Request.add_header(k, v)` applied to every `(k, v)` pair in order
/// -- later pairs overwrite an earlier one's *value* but never its
/// *position* (Python dict assignment semantics; `IndexMap::insert`
/// already matches this for an existing key).
fn add_headers(pairs: &[(String, String)]) -> IndexMap<String, String> {
    let mut headers = IndexMap::new();
    for (k, v) in pairs {
        headers.insert(python_capitalize(k), v.clone());
    }
    headers
}

// ---------------------------------------------------------------------
// `bytes(body)` -- the non-`str` `body:` coercion.
// ---------------------------------------------------------------------

/// Python's `bytes(value)` constructor, as far as a non-`str` `body:`
/// ever reaches it: a list/tuple of ints (each `0..=255`, else
/// `ValueError`/`OverflowError`) becomes those bytes; a bare int `n`
/// (`n >= 0`, else `ValueError`) becomes `n` zero bytes; anything else
/// (a `float`, in particular) is `TypeError: cannot convert '<type>'
/// object to bytes`.
fn python_bytes_ctor(value: &Value) -> Result<Vec<u8>, ToolError> {
    match value {
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::Int(_) | Value::Bool(_) => {
                        let n = electricity_value::pycompat::py_int(item).ok();
                        match n {
                            Some(n) if (0..=255).contains(&n) => out.push(n as u8),
                            _ => {
                                return Err(ToolError {
                                    py_class: PyExc::ValueError,
                                    message: "bytes must be in range(0, 256)".to_string(),
                                    cause: ErrorCause::None,
                                });
                            }
                        }
                    }
                    other => {
                        return Err(value_error(format!(
                            "'{}' object cannot be interpreted as an integer",
                            other.type_name()
                        )));
                    }
                }
            }
            Ok(out)
        }
        Value::Int(_) => match electricity_value::pycompat::py_int(value) {
            Ok(n) if n >= 0 => Ok(vec![0u8; n as usize]),
            Ok(_) => Err(value_error("negative count")),
            Err(err) => Err(value_error(err.message)),
        },
        Value::Bytes(b) => Ok(b.clone()),
        Value::Bool(b) => Ok(vec![0u8; usize::from(*b)]),
        other => Err(ToolError {
            py_class: PyExc::TypeError,
            message: format!("cannot convert '{}' object to bytes", other.type_name()),
            cause: ErrorCause::None,
        }),
    }
}

// ---------------------------------------------------------------------
// `http_error_excerpt` -- corrected signature (issue #485's own "stub
// signatures are this lane's to change").
// ---------------------------------------------------------------------

/// `plugins/base.py::http_error_excerpt(body: str, *, max_chars=500)`
/// -- *body* is already the decoded response text (never raw bytes:
/// the M1 gate's own stub signature took `(status, &[u8])`, matching
/// neither Python's parameters nor its contract; corrected here, the
/// one stub this file is explicitly free to change). A bounded,
/// redacted explanation of a non-2xx response body: a JSON object's
/// own `error.message`/`error`/`message`/`detail` field when present,
/// else the redacted body itself (object or scalar), sliced to
/// *max_chars* Unicode scalar values (never split bytes), never the
/// unredacted raw text.
/// *scrub*, when given (issue #600), also replaces a resolved secret
/// with its placeholder in whichever field this returns: a raw-text
/// scrub of *body* before it ever reaches this function (as every
/// caller already does) still misses a secret the server wrote with
/// JSON escapes (`\/` for a literal `/`, `\uXXXX` for a non-ASCII
/// character) -- decoding undoes the escaping, so the parsed
/// `error.message` this picks out can hold the literal value even
/// when the undecoded raw text never matched.
pub fn http_error_excerpt(
    body: &str,
    max_chars: usize,
    scrub: Option<&dyn Fn(&str) -> String>,
) -> String {
    let text = py_strip(body);
    if text.is_empty() {
        return String::new();
    }
    // Issue #600: *scrub* runs on the whole field before the cut, not
    // after -- a secret JSON decoding exposed (an escaped slash, a
    // `\uXXXX` character) can start anywhere in the text, including
    // just before `max_chars`, and a scrub of the already-sliced tail
    // would never see the rest of it.
    let slice_chars = |s: &str| -> String {
        let scrubbed = match scrub {
            Some(f) => f(s),
            None => s.to_string(),
        };
        match scrubbed.char_indices().nth(max_chars) {
            Some((byte_idx, _)) => scrubbed[..byte_idx].to_string(),
            None => scrubbed,
        }
    };
    let parsed = match electricity_json::loads(text) {
        Ok(v) => v,
        Err(_) => {
            let redacted = electricity_redaction::redact(Value::Str(text.to_string()));
            return match &redacted {
                Value::Str(s) => slice_chars(s),
                other => slice_chars(&other.py_str()),
            };
        }
    };
    match parsed {
        Value::Dict(_) => {
            let redacted = electricity_redaction::redact(parsed);
            if let Value::Dict(ref d) = redacted {
                if let Some(Value::Dict(error)) = d.get(&Value::Str("error".to_string())) {
                    if let Some(Value::Str(message)) = error.get(&Value::Str("message".to_string()))
                    {
                        if !message.is_empty() {
                            return slice_chars(message);
                        }
                    }
                }
                if let Some(Value::Str(error)) = d.get(&Value::Str("error".to_string())) {
                    if !error.is_empty() {
                        return slice_chars(error);
                    }
                }
                for key in ["message", "detail"] {
                    if let Some(Value::Str(value)) = d.get(&Value::Str(key.to_string())) {
                        if !value.is_empty() {
                            return slice_chars(value);
                        }
                    }
                }
            }
            let dumped = electricity_json::dumps_default_str(
                &redacted,
                electricity_json::WriteMode {
                    indent: None,
                    sort_keys: false,
                    ensure_ascii: false,
                    separators: electricity_json::Separators::Default,
                },
            )
            .unwrap_or_default();
            slice_chars(&dumped)
        }
        Value::List(_) => {
            let redacted = electricity_redaction::redact(parsed);
            let dumped = electricity_json::dumps_default_str(
                &redacted,
                electricity_json::WriteMode {
                    indent: None,
                    sort_keys: false,
                    ensure_ascii: false,
                    separators: electricity_json::Separators::Default,
                },
            )
            .unwrap_or_default();
            slice_chars(&dumped)
        }
        // A parsed JSON scalar (`None`/bool/number) falls through to
        // the same `redact(text)` the no-JSON-at-all branch above
        // uses -- the *raw wire text* (`"null"`, `"42"`, ...), never
        // `str()` of the parsed value (`None.py_str()` would give
        // Python's `"None"`, not the JSON literal `"null"` actually
        // on the wire; confirmed against CPython directly,
        // `generate_http_corpus.py`'s own `http_error_excerpt_cases`).
        _ => {
            let redacted = electricity_redaction::redact(Value::Str(text.to_string()));
            match &redacted {
                Value::Str(s) => slice_chars(s),
                other => slice_chars(&other.py_str()),
            }
        }
    }
}

const ERROR_EXCERPT_MAX_CHARS: usize = 500;

// ---------------------------------------------------------------------
// Netloc -> host/port (userinfo stripped -- plain `urlopen()` never
// turns it into an `Authorization` header).
// ---------------------------------------------------------------------

/// `urllib.request._splittype`'s own full result (scheme lowercased,
/// plus everything after the first matched `:`) -- only
/// [`resolve_proxy_target`]'s own `_parse_proxy` port still needs this
/// (to tell a URL-form proxy value, `http://user:pass@host:port`,
/// apart from an authority-form one, `host:port` or `user:pass@host:
/// port` with no scheme at all); `execute`'s own scheme check uses
/// [`check_url`]/[`http_url::urlsplit`] instead, now that it runs on
/// the raw URL before this file ever calls `Request`'s own
/// urllib-parse-time regex (issue #778: `plugins/http.py` never
/// raises `Request`'s "unknown url type" text either, only
/// `http_conventions.check_url`'s own).
fn splittype_with_rest(url: &str) -> (Option<String>, &str) {
    if let Some(colon) = url.find(':') {
        let candidate = &url[..colon];
        if !candidate.is_empty() && !candidate.contains('/') {
            return (Some(candidate.to_ascii_lowercase()), &url[colon + 1..]);
        }
    }
    (None, url)
}

fn split_netloc(netloc: &str) -> (String, Option<u16>) {
    let hostinfo = match netloc.rfind('@') {
        Some(idx) => &netloc[idx + 1..],
        None => netloc,
    };
    if let Some(rest) = hostinfo.strip_prefix('[') {
        if let Some((addr, after)) = rest.split_once(']') {
            let port = after.strip_prefix(':').and_then(|p| p.parse::<u16>().ok());
            return (addr.to_string(), port);
        }
    }
    match hostinfo.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            (host.to_string(), port.parse::<u16>().ok())
        }
        _ => (hostinfo.to_string(), None),
    }
}

fn default_port(scheme: &str) -> u16 {
    if scheme == "https" { 443 } else { 80 }
}

/// The port part of a netloc's host, the way
/// `http.client.HTTPConnection._get_hostport` splits it: the text after
/// the last `:`, but only when that `:` comes after any `]` (an IPv6
/// literal's own colons are never a port separator).
#[derive(Debug, PartialEq, Eq)]
enum PortText<'a> {
    /// No port separator at all: the scheme's default port.
    Absent,
    /// `host:` -- `_get_hostport` maps an empty port to the default port
    /// too (`http://foo.com:/` == `http://foo.com/`).
    Empty,
    /// `host:<text>`, still to be read with `int()`.
    Given(&'a str),
}

/// `(host, port text)` for *netloc* (userinfo dropped, see
/// [`split_netloc`]), with the port separator and, for an IPv6 literal,
/// the surrounding brackets removed -- `_get_hostport`'s own split.
fn split_hostport(netloc: &str) -> (&str, PortText<'_>) {
    let hostinfo = match netloc.rfind('@') {
        Some(idx) => &netloc[idx + 1..],
        None => netloc,
    };
    let (host, port) = match (hostinfo.rfind(':'), hostinfo.rfind(']')) {
        (Some(colon), bracket) if bracket.is_none_or(|b| colon > b) => {
            let text = &hostinfo[colon + 1..];
            let port = if text.is_empty() {
                PortText::Empty
            } else {
                PortText::Given(text)
            };
            (&hostinfo[..colon], port)
        }
        _ => (hostinfo, PortText::Absent),
    };
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    (host, port)
}

/// The request's `(host, port)`, the strict port rule
/// ([`crate::http_conventions::strict_port`]) -- refused before any I/O
/// with Circuitry's own text, naming *original_url* with any userinfo
/// removed, for anything but an absent/empty port or a 1-65535
/// ASCII-digit value. No bad port ever falls back to the scheme's
/// default port.
fn request_host_port(
    netloc: &str,
    scheme: &str,
    original_url: &str,
) -> Result<(String, u16), ToolError> {
    let port = crate::http_conventions::strict_port(netloc, original_url).map_err(|message| {
        ToolError {
            py_class: PyExc::ValueError,
            message,
            cause: ErrorCause::None,
        }
    })?;
    let (host, _) = split_hostport(netloc);
    Ok((
        host.to_string(),
        port.unwrap_or_else(|| default_port(scheme)),
    ))
}

/// A header value's own latin-1 encodability -- `http.client`'s own
/// `putheader` encode step. Returns the first codepoint that doesn't
/// fit, 0-indexed by Unicode scalar value (`UnicodeEncodeError.start`),
/// for [`unicode_encode_error_text`].
fn first_non_latin1_char(s: &str) -> Option<(usize, char)> {
    s.chars().enumerate().find(|(_, c)| (*c as u32) > 0xFF)
}

/// `str(UnicodeEncodeError)` for a single bad character, latin-1,
/// `'strict'`: `'latin-1' codec can't encode character '\uXXXX' in
/// position N: ordinal not in range(256)`.
fn unicode_encode_error_text(pos: usize, ch: char) -> String {
    format!(
        "'latin-1' codec can't encode character '\\u{:04x}' in position {pos}: ordinal not in range(256)",
        ch as u32
    )
}

/// `http.client`'s own `_contains_disallowed_url_pchar_re` check
/// (`[\x00-\x20\x7f]`) against a request target -- the first match's
/// own character, for [`invalid_url_text`].
fn first_disallowed_target_char(target: &str) -> Option<char> {
    target
        .chars()
        .find(|c| (*c as u32) <= 0x20 || *c as u32 == 0x7f)
}

fn invalid_url_text(target: &str, found: char) -> String {
    format!(
        "URL can't contain control characters. {} (found at least {})",
        Value::Str(target.to_string()).py_repr(),
        Value::Str(found.to_string()).py_repr()
    )
}

// ---------------------------------------------------------------------
// Parameter parsing.
// ---------------------------------------------------------------------

struct ParsedRequest {
    /// The request URL this plugin built, query appended -- `raw.url`,
    /// unchanged across any later redirect (`plugins/http.py`'s own
    /// `url` local variable is never reassigned by `urlopen`, which
    /// handles every redirect internally).
    url: String,
    method: String,
    /// `dict(params.get("headers") or {})`'s own result, already with
    /// a `json:` body's `Content-Type` `setdefault` applied -- *not*
    /// yet capitalize-collapsed ([`add_headers`] does that at send
    /// time, once per hop, since a redirect drops some of these).
    header_pairs: Vec<(Value, Value)>,
    body: Option<Vec<u8>>,
    parse_mode: String,
    fail_on_error: bool,
    redirects: RedirectMode,
    /// `params.save_to` (issue #726), already checked: present only
    /// when the response body is written to a file instead of parsed.
    save: Option<SaveTarget>,
}

/// `params.redirects` (`plugins/http.py`'s `REDIRECT_MODES`, issue #598).
/// Absent is `Follow`: the default is `strip_auth` only when a header came
/// from a secret, and electricity refuses a document that names one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedirectMode {
    Follow,
    Never,
    StripAuth,
}

impl RedirectMode {
    /// `plugins/http.py`'s `_REFUSED_REDIRECT_NOTES`: what a refused
    /// redirect's `stderr` adds after the reason.
    fn refused_note(self) -> &'static str {
        match self {
            RedirectMode::StripAuth => {
                "redirect not followed (redirects: strip_auth follows https only)"
            }
            RedirectMode::Follow | RedirectMode::Never => {
                "redirect not followed (redirects: never)"
            }
        }
    }
}

fn parse_redirects(
    value: Option<&Value>,
    default: RedirectMode,
) -> Result<RedirectMode, ToolError> {
    match value {
        None | Some(Value::None) => Ok(default),
        Some(Value::Str(s)) if s == "follow" => Ok(RedirectMode::Follow),
        Some(Value::Str(s)) if s == "never" => Ok(RedirectMode::Never),
        Some(Value::Str(s)) if s == "strip_auth" => Ok(RedirectMode::StripAuth),
        Some(other) => Err(value_error(format!(
            "HttpPlugin: params['redirects'] must be one of 'follow', 'never', \
             'strip_auth', got {}.",
            other.py_repr()
        ))),
    }
}

/// `plugins/http.py::_origin`: `(scheme, host, port)` of *url*, the
/// scheme's default port filled in, the host lower-cased; `None` (which
/// matches no origin) for a URL with no host or a port that isn't 0-65535
/// in ASCII digits.
fn redirect_origin(url: &str) -> Option<(String, String, Option<u16>)> {
    let split = http_url::urlsplit(url, "");
    let scheme = split.scheme.to_lowercase();
    let (host, port_text) = split_hostport(&split.netloc);
    if host.is_empty() {
        return None;
    }
    let port = match port_text {
        PortText::Absent | PortText::Empty => match scheme.as_str() {
            "http" => Some(80),
            "https" => Some(443),
            _ => None,
        },
        PortText::Given(text) => {
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some(text.parse::<u16>().ok()?)
        }
    };
    Some((scheme, host.to_lowercase(), port))
}

fn present(value: Option<&Value>) -> Option<&Value> {
    value.filter(|v| !matches!(v, Value::None))
}

/// `dict(value)` -- *value* is `params.get("headers")`, already
/// defaulted to `Value::None` meaning "absent" (`or {}` in Python).
/// A [`Value::Dict`] is copied as-is (already deduplicated,
/// insertion-ordered, exactly as a real Python `dict` literal would
/// be); a [`Value::List`] of 2-element lists builds one the same way
/// `dict([[k, v], ...])` does (first position, last value -- an
/// [`IndexMap`]'s own `insert` on an existing key already matches
/// this). Anything else raises CPython's own `dict()` wording
/// (third-party text, not matched byte for byte -- DESIGN.md's own
/// convention for a third-party error this crate doesn't otherwise
/// pin).
/// Python truthiness, for `dict(params.get("headers") or {})`'s own
/// `or {}` fallback below -- `None`/`False`/a numeric zero/any empty
/// `str`/`list`/`dict` all take the `{}` branch, never reaching
/// `dict()` at all (issue #485: a template that
/// renders `headers:` to `""` is the realistic case this matters
/// for). Mirrors `electricity_template::render::truthy` exactly;
/// duplicated rather than shared across the crate boundary for one
/// small, stable function.
fn headers_param_is_falsy(value: &Value) -> bool {
    match value {
        Value::None => true,
        Value::Bool(b) => !b,
        Value::Int(i) => i.is_zero(),
        Value::Float(f) => *f == 0.0,
        Value::Str(s) => s.is_empty(),
        Value::Bytes(b) => b.is_empty(),
        Value::List(items) => items.is_empty(),
        Value::Dict(d) => d.is_empty(),
        Value::Date(_) | Value::DateTime(..) => false,
    }
}

fn coerce_headers_param(value: Option<&Value>) -> Result<Vec<(Value, Value)>, ToolError> {
    if let Some(v) = value {
        if headers_param_is_falsy(v) {
            return Ok(Vec::new());
        }
    }
    match present(value) {
        None => Ok(Vec::new()),
        Some(Value::Dict(d)) => Ok(d.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
        Some(Value::List(items)) => {
            let mut ordered: IndexMap<Value, Value> = IndexMap::new();
            for (i, item) in items.iter().enumerate() {
                match item {
                    Value::List(pair) if pair.len() == 2 => {
                        ordered.insert(pair[0].clone(), pair[1].clone());
                    }
                    Value::List(pair) => {
                        return Err(value_error(format!(
                            "dictionary update sequence element #{i} has length {}; 2 is required",
                            pair.len()
                        )));
                    }
                    _ => {
                        return Err(ToolError {
                            py_class: PyExc::TypeError,
                            message: format!(
                                "cannot convert dictionary update sequence element #{i} to a sequence"
                            ),
                            cause: ErrorCause::None,
                        });
                    }
                }
            }
            Ok(ordered.into_iter().collect())
        }
        Some(other) => Err(ToolError {
            py_class: PyExc::TypeError,
            message: format!("'{}' object is not iterable", other.type_name()),
            cause: ErrorCause::None,
        }),
    }
}

/// `params['url']`, stripped and checked non-empty -- the only piece of
/// `parse_params` extracted before `execute`'s own scheme/port check
/// ([`check_url`]), which must run before `parse_params`'s own
/// `save_target` call touches the filesystem, or `params['params']` is
/// appended to the URL (issue #778).
fn extract_url(params: &Value) -> Result<String, ToolError> {
    match get(params, "url") {
        Some(Value::Str(s)) if !py_strip(s).is_empty() => Ok(py_strip(s).to_string()),
        _ => Err(value_error(
            "HttpPlugin requires params['url'] as a non-empty string.",
        )),
    }
}

fn parse_params(
    url: String,
    params: &Value,
    secret_headers: &[String],
) -> Result<ParsedRequest, ToolError> {
    let method = get(params, "method")
        .cloned()
        .unwrap_or_else(|| Value::Str("GET".to_string()))
        .py_str()
        .to_uppercase();

    let mut header_pairs = coerce_headers_param(get(params, "headers"))?;

    let parse_mode = get(params, "parse")
        .cloned()
        .unwrap_or_else(|| Value::Str("auto".to_string()))
        .py_str()
        .to_lowercase();

    let fail_on_error = crate::fs::as_bool(get(params, "fail_on_error"), true);
    // `plugins/http.py`'s own default: `strip_auth` once a header came
    // from a secret (issue #725), `follow` otherwise.
    let default_redirects = if secret_headers.is_empty() {
        RedirectMode::Follow
    } else {
        RedirectMode::StripAuth
    };
    let redirects = parse_redirects(get(params, "redirects"), default_redirects)?;

    let json_body = present(get(params, "json"));
    let raw_body = present(get(params, "body"));
    if json_body.is_some() && raw_body.is_some() {
        return Err(value_error(
            "HttpPlugin: pass either params['json'] or params['body'], not both.",
        ));
    }

    // `plugins/http.py`'s own order (issue #778): `save_target`'s
    // filesystem checks run only after `execute`'s own scheme/port
    // check on the raw URL, and before `params['params']` is appended
    // below.
    let save = save_target(params)?;

    let mut url = url;
    if let Some(Value::Dict(query)) = get(params, "params") {
        if !query.is_empty() {
            let pairs: Vec<(String, Value)> =
                query.iter().map(|(k, v)| (k.py_str(), v.clone())).collect();
            let query_string =
                electricity_value::pycompat::urlencode(&pairs, true).map_err(value_error)?;
            let sep = if url.contains('?') { "&" } else { "?" };
            url = format!("{url}{sep}{query_string}");
        }
    }

    let body = if let Some(json_value) = json_body {
        let has_content_type = header_pairs
            .iter()
            .any(|(k, _)| matches!(k, Value::Str(s) if s == "Content-Type"));
        if !has_content_type {
            header_pairs.push((
                Value::Str("Content-Type".to_string()),
                Value::Str("application/json".to_string()),
            ));
        }
        let text = electricity_json::dumps_default_str(
            json_value,
            electricity_json::WriteMode {
                indent: None,
                sort_keys: false,
                ensure_ascii: true,
                separators: electricity_json::Separators::Default,
            },
        )
        .map_err(|err| runtime_error(err.to_string()))?;
        Some(text.into_bytes())
    } else if let Some(raw) = raw_body {
        match raw {
            Value::Str(s) => Some(s.as_bytes().to_vec()),
            other => Some(python_bytes_ctor(other)?),
        }
    } else {
        None
    };

    if save.is_some() && parse_mode != "auto" {
        return Err(value_error(
            "HttpPlugin: params['parse'] must be 'auto' (or unset) with params['save_to']: \
             the body is saved as bytes, never parsed.",
        ));
    }

    Ok(ParsedRequest {
        url,
        method,
        header_pairs,
        body,
        parse_mode,
        fail_on_error,
        redirects,
        save,
    })
}

// ---------------------------------------------------------------------
// Response assembly.
// ---------------------------------------------------------------------

fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// `json.loads(text)` per `parse:`/`Content-Type` (`plugins/http.py`'s
/// own post-try/except parse block) -- can raise even for a non-2xx
/// response: parsing runs unconditionally on whatever `text` ended up
/// being, success or `HTTPError`-caught alike.
fn parse_body(
    parse_mode: &str,
    headers: &[(String, String)],
    text: &str,
) -> Result<Value, ToolError> {
    let content_type = find_header(headers, "content-type")
        .unwrap_or("")
        .to_lowercase();
    let looks_like_json = content_type.contains("json");
    if parse_mode == "json" || (parse_mode == "auto" && looks_like_json) {
        if text.is_empty() {
            return Ok(Value::None);
        }
        match electricity_json::loads(text) {
            Ok(v) => Ok(v),
            Err(err) if parse_mode == "json" => Err(runtime_error(format!(
                "HttpPlugin: parse='json' but response is not JSON: {err}"
            ))),
            Err(_) => Ok(Value::Str(text.to_string())),
        }
    } else {
        Ok(Value::Str(text.to_string()))
    }
}

// ---------------------------------------------------------------------
// `save_to` (issue #726).
// ---------------------------------------------------------------------

/// `plugins/http.py::DEFAULT_SAVE_TO_MAX_BYTES`.
const DEFAULT_SAVE_TO_MAX_BYTES: u64 = 100 * 1024 * 1024;

/// `core/prompt_files.py::path_is_inside`: *path* is *root* itself or
/// lies under it. Both are expected already canonicalized by the
/// caller -- this is a pure component comparison.
fn path_is_inside(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

/// `pathlib.Path.resolve(strict=False)`'s own algorithm, enough of it for
/// [`save_target`]'s own need: *base* is already absolute and fully
/// resolved; walks *relative*'s own components onto it, following a
/// symlink the moment a `Normal` component's own path exists as one
/// (bounded, the same `ELOOP`-style cap a real filesystem enforces), and
/// popping on `ParentDir` the same way `resolve()` collapses a lexical
/// `..` -- even past a component that was never created, so `a/../b`
/// still lands inside *base* and never touches a literal `a` on disk.
/// Every component this ever sees is `Normal`/`CurDir`/`ParentDir`
/// (never `RootDir`/`Prefix`, since *relative* is already checked
/// non-absolute by the caller).
fn resolve_existing_prefix(base: &Path, relative: &Path) -> Result<PathBuf, ToolError> {
    let mut current = base.to_path_buf();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(name) => {
                current.push(name);
                current = resolve_symlink_chain(current)?;
            }
            std::path::Component::ParentDir => {
                current.pop();
            }
            std::path::Component::CurDir => {}
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                unreachable!("save_target already checked relative is not absolute")
            }
        }
    }
    Ok(current)
}

/// Follows *path* through every symlink it already is, on disk, right
/// now -- a component that doesn't exist yet (the common case: a
/// directory `save_to` is about to create) is returned unchanged, a
/// `..` or sibling file name intact.
fn resolve_symlink_chain(path: PathBuf) -> Result<PathBuf, ToolError> {
    let mut current = path;
    for _ in 0..40 {
        match std::fs::symlink_metadata(&current) {
            Err(_) => return Ok(current),
            Ok(meta) if meta.file_type().is_symlink() => {
                let target = std::fs::read_link(&current).map_err(|e| {
                    runtime_error(format!(
                        "HttpPlugin: could not read {}: {e}",
                        current.display()
                    ))
                })?;
                current = if target.is_absolute() {
                    target
                } else {
                    current
                        .parent()
                        .expect("symlink_metadata succeeded, so current has a parent")
                        .join(target)
                };
            }
            Ok(_) => return Ok(current),
        }
    }
    Err(runtime_error(format!(
        "HttpPlugin: too many levels of symlinks resolving {}.",
        current.display()
    )))
}

/// `plugins/http.py::_SaveTarget` -- *path*'s own directory is not
/// necessarily created yet; [`make_save_directories`] does that.
struct SaveTarget {
    base: PathBuf,
    path: PathBuf,
    overwrite: bool,
    max_bytes: u64,
}

/// `plugins/http.py::_save_target` -- *params*`['save_to']` as a
/// checked [`SaveTarget`], or `None` without one. `Err` for every path
/// this effect may not write, before anything is requested.
fn save_target(params: &Value) -> Result<Option<SaveTarget>, ToolError> {
    let Some(raw_value) = get(params, "save_to") else {
        return Ok(None);
    };
    let Value::Str(raw) = raw_value else {
        return Err(value_error(format!(
            "HttpPlugin: params['save_to'] must be a non-empty relative path, got {}.",
            raw_value.py_repr()
        )));
    };
    if py_strip(raw).is_empty() {
        return Err(value_error(format!(
            "HttpPlugin: params['save_to'] must be a non-empty relative path, got {}.",
            raw_value.py_repr()
        )));
    }
    if raw.contains('\0') {
        return Err(value_error(
            "HttpPlugin: params['save_to'] contains a NUL byte.",
        ));
    }
    let relative = Path::new(raw.as_str());
    if relative.is_absolute() {
        return Err(value_error(format!(
            "HttpPlugin: params['save_to'] must be a relative path, got {}.",
            raw_value.py_repr()
        )));
    }
    let within = get(params, "within");
    let (base, where_text) = match within {
        None | Some(Value::None) => (
            std::env::current_dir().map_err(|e| runtime_error(e.to_string()))?,
            "the working directory".to_string(),
        ),
        Some(Value::Str(within_str)) if Path::new(within_str).is_absolute() => {
            let base = PathBuf::from(within_str);
            if !base.is_dir() {
                return Err(value_error(format!(
                    "HttpPlugin: params['within'] {} is not a directory.",
                    within_value_repr(within_str)
                )));
            }
            (base, "params['within']".to_string())
        }
        Some(other) => {
            return Err(value_error(format!(
                "HttpPlugin: params['within'] must be an absolute directory path, got {}.",
                other.py_repr()
            )));
        }
    };
    let base = base
        .canonicalize()
        .map_err(|e| runtime_error(e.to_string()))?;
    // `lexical.name in ("", ".", "..")` -- the *literal* final `/`-
    // delimited segment of `raw`, before `Path`'s own parsing drops a
    // trailing `.` component the way `Path::file_name` does (`out/.`
    // would otherwise silently name `out`).
    let last_segment = raw.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    if matches!(last_segment, "" | "." | "..") {
        return Err(value_error(format!(
            "HttpPlugin: params['save_to'] {} does not name a file.",
            raw_value.py_repr()
        )));
    }
    let file_name = relative
        .file_name()
        .expect("the last_segment check above already rejects every case file_name would not");
    let directory = resolve_existing_prefix(&base, relative.parent().unwrap_or(Path::new("")))?;
    if !path_is_inside(&directory, &base) {
        return Err(value_error(format!(
            "HttpPlugin: params['save_to'] {} resolves outside {where_text} ({}).",
            raw_value.py_repr(),
            base.display()
        )));
    }
    let path = directory.join(file_name);

    let overwrite = get(params, "overwrite")
        .map(|v| crate::fs::as_bool(Some(v), false))
        .unwrap_or(false);
    if path.is_symlink() {
        return Err(value_error(format!(
            "HttpPlugin: params['save_to'] {} is a symlink; it is never replaced.",
            raw_value.py_repr()
        )));
    }
    if path.exists() {
        if !path.is_file() {
            return Err(value_error(format!(
                "HttpPlugin: params['save_to'] {} exists and is not a regular file.",
                raw_value.py_repr()
            )));
        }
        if !overwrite {
            return Err(value_error(format!(
                "HttpPlugin: params['save_to'] {} already exists; set overwrite: true to \
                 replace it.",
                raw_value.py_repr()
            )));
        }
    }

    let max_bytes = match get(params, "max_bytes") {
        None | Some(Value::None) => DEFAULT_SAVE_TO_MAX_BYTES,
        Some(value) => positive_int_param(value, "max_bytes")?,
    };

    Ok(Some(SaveTarget {
        base,
        path,
        overwrite,
        max_bytes,
    }))
}

fn within_value_repr(text: &str) -> String {
    Value::Str(text.to_string()).py_repr()
}

/// `params['max_bytes']` as a positive integer, Python's own `int()`
/// coercion (a numeric string included).
fn positive_int_param(value: &Value, name: &str) -> Result<u64, ToolError> {
    // `isinstance(max_bytes, bool)` is checked and rejected *before* the
    // plain `int` check -- Python's `bool` is an `int` subclass, so
    // `isinstance(True, int)` alone would otherwise accept it.
    let as_int = match value {
        Value::Bool(_) => None,
        Value::Int(_) => electricity_value::pycompat::py_int(value).ok(),
        // `max_bytes.strip().isdigit()` -- ASCII digits only, no sign;
        // `int()`'s own looser grammar (leading `+`/`-`, surrounding
        // whitespace it would still strip again) never applies here.
        Value::Str(s) => {
            let stripped = py_strip(s);
            if !stripped.is_empty() && stripped.chars().all(|c| c.is_ascii_digit()) {
                stripped.parse::<i64>().ok()
            } else {
                None
            }
        }
        _ => None,
    };
    match as_int {
        Some(n) if n > 0 => Ok(n as u64),
        _ => Err(value_error(format!(
            "HttpPlugin: params['{name}'] must be a positive integer, got {}.",
            value.py_repr()
        ))),
    }
}

/// `plugins/http.py::_make_directories` -- creates *target*'s missing
/// directories one at a time, each checked inside the base once
/// symlinks are resolved, and returns the last.
fn make_save_directories(target: &SaveTarget) -> Result<PathBuf, ToolError> {
    let parent = target
        .path
        .parent()
        .expect("save_target always builds path as directory.join(file_name)");
    let relative = parent
        .strip_prefix(&target.base)
        .expect("save_target builds path under base");
    let mut current = target.base.clone();
    for part in relative.components() {
        current.push(part);
        if !current.exists() {
            std::fs::create_dir(&current).map_err(|e| {
                runtime_error(format!(
                    "HttpPlugin: could not create {}: {e}",
                    current.display()
                ))
            })?;
        }
        current = current
            .canonicalize()
            .map_err(|e| runtime_error(e.to_string()))?;
        if !path_is_inside(&current, &target.base) || !current.is_dir() {
            return Err(runtime_error(format!(
                "HttpPlugin: {} is not a directory inside {}; nothing was saved.",
                current.display(),
                target.base.display()
            )));
        }
    }
    if current != *parent {
        return Err(runtime_error(format!(
            "HttpPlugin: {} changed during the download; nothing was saved.",
            parent.display()
        )));
    }
    Ok(current)
}

struct SavedFile {
    path: PathBuf,
    bytes: u64,
    sha256: String,
}

/// `plugins/http.py::_save_body`/`_move_into_place` -- *body* is
/// already fully received (issue #726's own DESIGN.md divergence row:
/// unlike `cof run`'s own chunked `resp.read(...)` loop, this build's
/// shared HTTP client always buffers a response in full before this
/// plugin ever sees it, so the body is written to the temporary file
/// in one pass rather than streamed off the wire -- `max_bytes` still
/// caps what ever reaches disk, and a body over it leaves no file
/// behind, exactly as `cof run`'s own cap does). Written to a
/// temporary file next to the target first, moved into place only
/// once complete, so a failure leaves no partial file.
fn save_response_body(body: &[u8], target: &SaveTarget) -> Result<SavedFile, ToolError> {
    if body.len() as u64 > target.max_bytes {
        return Err(runtime_error(format!(
            "HttpPlugin: the response body is {} bytes, over max_bytes ({}); nothing was saved.",
            body.len(),
            target.max_bytes
        )));
    }
    let directory = make_save_directories(target)?;
    let file_name = target
        .path
        .file_name()
        .expect("save_target always builds path with a file name");
    let temp_name = format!(
        ".{}.{:016x}.part",
        file_name.to_string_lossy(),
        random_u64()
    );
    let temp = directory.join(temp_name);
    write_temp_file(&temp, body)?;
    let mut digest = Sha256::new();
    digest.update(body);
    let hash = format!("{:x}", digest.finalize());
    if let Err(err) = move_into_place(&temp, target) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    Ok(SavedFile {
        path: target.path.clone(),
        bytes: body.len() as u64,
        sha256: hash,
    })
}

fn random_u64() -> u64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    RandomState::new().build_hasher().finish()
}

#[cfg(unix)]
fn write_temp_file(temp: &Path, body: &[u8]) -> Result<(), ToolError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o666);
    options.custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(temp).map_err(|e| {
        runtime_error(format!(
            "HttpPlugin: could not save {}: {e}",
            temp.display()
        ))
    })?;
    std::io::Write::write_all(&mut file, body).map_err(|e| {
        runtime_error(format!(
            "HttpPlugin: could not save {}: {e}",
            temp.display()
        ))
    })
}

#[cfg(not(unix))]
fn write_temp_file(temp: &Path, body: &[u8]) -> Result<(), ToolError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(temp).map_err(|e| {
        runtime_error(format!(
            "HttpPlugin: could not save {}: {e}",
            temp.display()
        ))
    })?;
    std::io::Write::write_all(&mut file, body).map_err(|e| {
        runtime_error(format!(
            "HttpPlugin: could not save {}: {e}",
            temp.display()
        ))
    })
}

/// `plugins/http.py::_move_into_place` -- replacing a regular file
/// under `overwrite`, and otherwise only where nothing exists yet.
fn move_into_place(temp: &Path, target: &SaveTarget) -> Result<(), ToolError> {
    let path = &target.path;
    if path.is_symlink() || (path.exists() && !path.is_file()) {
        return Err(runtime_error(format!(
            "HttpPlugin: {} became a symlink or a directory during the download; nothing was \
             saved.",
            path.display()
        )));
    }
    if target.overwrite {
        std::fs::rename(temp, path).map_err(|e| {
            runtime_error(format!(
                "HttpPlugin: could not save {}: {e}",
                path.display()
            ))
        })
    } else {
        match std::fs::hard_link(temp, path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(runtime_error(format!(
                "HttpPlugin: {} appeared during the download; nothing was replaced.",
                path.display()
            ))),
            // No hard links here (FAT, exFAT, some network shares):
            // claim the name exclusively, then replace the placeholder.
            Err(_) => replace_exclusively(temp, path),
        }
    }
}

#[cfg(unix)]
fn replace_exclusively(temp: &Path, path: &Path) -> Result<(), ToolError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o666);
    options.custom_flags(libc::O_NOFOLLOW);
    options.open(path).map_err(|e| {
        runtime_error(format!(
            "HttpPlugin: could not save {}: {e}",
            path.display()
        ))
    })?;
    if let Err(e) = std::fs::rename(temp, path) {
        let _ = std::fs::remove_file(path);
        return Err(runtime_error(format!(
            "HttpPlugin: could not save {}: {e}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn replace_exclusively(temp: &Path, path: &Path) -> Result<(), ToolError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    options.open(path).map_err(|e| {
        runtime_error(format!(
            "HttpPlugin: could not save {}: {e}",
            path.display()
        ))
    })?;
    if let Err(e) = std::fs::rename(temp, path) {
        let _ = std::fs::remove_file(path);
        return Err(runtime_error(format!(
            "HttpPlugin: could not save {}: {e}",
            path.display()
        )));
    }
    Ok(())
}

/// A temporary file created for one hop's own streamed download --
/// deleted on `Drop` unless [`TempFileGuard::keep`] disarms it first
/// (issue #726: every exit from the redirect loop but the one true
/// 2xx success leaves no file behind, whatever return point it takes).
struct TempFileGuard(Option<PathBuf>);

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl TempFileGuard {
    fn path(&self) -> Option<&Path> {
        self.0.as_deref()
    }

    fn keep(mut self) -> PathBuf {
        self.0
            .take()
            .expect("TempFileGuard::keep called at most once")
    }
}

/// Creates a fresh, empty temporary file beside *target*'s own path,
/// the same naming/open-flags [`write_temp_file`] uses for an
/// already-complete body -- opened for incremental writes instead.
#[cfg(unix)]
fn open_save_temp_file(target: &SaveTarget) -> Result<(PathBuf, std::fs::File), ToolError> {
    use std::os::unix::fs::OpenOptionsExt;
    let directory = make_save_directories(target)?;
    let file_name = target
        .path
        .file_name()
        .expect("save_target always builds path with a file name");
    let temp_name = format!(
        ".{}.{:016x}.part",
        file_name.to_string_lossy(),
        random_u64()
    );
    let temp = directory.join(temp_name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o666);
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options.open(&temp).map_err(|e| {
        runtime_error(format!(
            "HttpPlugin: could not save {}: {e}",
            temp.display()
        ))
    })?;
    Ok((temp, file))
}

#[cfg(not(unix))]
fn open_save_temp_file(target: &SaveTarget) -> Result<(PathBuf, std::fs::File), ToolError> {
    let directory = make_save_directories(target)?;
    let file_name = target
        .path
        .file_name()
        .expect("save_target always builds path with a file name");
    let temp_name = format!(
        ".{}.{:016x}.part",
        file_name.to_string_lossy(),
        random_u64()
    );
    let temp = directory.join(temp_name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let file = options.open(&temp).map_err(|e| {
        runtime_error(format!(
            "HttpPlugin: could not save {}: {e}",
            temp.display()
        ))
    })?;
    Ok((temp, file))
}

/// [`send_hop`]'s own success-with-save-to result: the body streamed
/// straight to *guard*'s own temp file, never collected into memory
/// (issue #726) -- *bytes*/*sha256* are the digest [`BodySink::write`]
/// updated per chunk, so [`finalize_streamed_save`] never has to read
/// the file back just to hash it again.
struct StreamedSave {
    guard: TempFileGuard,
    bytes: u64,
    sha256: String,
}

/// `plugins/http.py::_save_body`'s own `RuntimeError`/`ConnectionError`
/// texts for `send_to_sink`'s own two failure shapes -- *save* is
/// always `Some` here (the only caller [`send_hop`] ever builds a
/// [`http_client::BodySink`] for).
fn save_over_cap_error(declared: Option<usize>, save: &SaveTarget) -> ToolError {
    let text = match declared {
        Some(n) => format!(
            "HttpPlugin: the response body is {n} bytes, over max_bytes ({}); nothing was saved.",
            save.max_bytes
        ),
        None => format!(
            "HttpPlugin: the response body is over max_bytes ({}); nothing was saved.",
            save.max_bytes
        ),
    };
    runtime_error(text)
}

/// One hop's own request/response, streaming straight to a fresh
/// temporary file when *save* is set (issue #726) instead of
/// collecting the body into memory at all -- bounded by *save*'s own
/// `max_bytes`, the same way [`http_client::send_capped`] bounds a
/// different caller's own cap, but erroring (with [`TempFileGuard`]
/// deleting whatever was already written) rather than silently
/// truncating, matching `plugins/http.py::_save_body`'s own
/// `_SAVE_CHUNK_BYTES` loop. `RawResponse::body` is always empty in
/// that case; the caller reads the temp file back itself for a
/// non-2xx error excerpt (bounded the same way -- a documented,
/// narrow divergence from `cof run`'s own unbounded `exc.read()` on
/// that path, DESIGN.md §13).
async fn send_hop(
    client: &http_client::HttpClient,
    connect_target: &http_client::ConnectTarget,
    raw_request: &http_client::RawRequest,
    timeout: http_client::TimeoutModel,
    save: Option<&SaveTarget>,
    original_url: &str,
) -> Result<(http_client::RawResponse, Option<StreamedSave>), ToolError> {
    let Some(save) = save else {
        let response = http_client::send(client, connect_target, raw_request, timeout)
            .await
            .map_err(|e| send_error_to_tool_error(e, original_url))?;
        return Ok((response, None));
    };
    let (temp_path, mut file) = open_save_temp_file(save)?;
    let guard = TempFileGuard(Some(temp_path));
    let mut digest = Sha256::new();
    let mut written: u64 = 0;
    let max_bytes = usize::try_from(save.max_bytes).unwrap_or(usize::MAX);
    let result = {
        let mut write = |chunk: &[u8]| -> Result<(), String> {
            digest.update(chunk);
            written += chunk.len() as u64;
            std::io::Write::write_all(&mut file, chunk).map_err(|e| e.to_string())
        };
        let sink = http_client::BodySink {
            max_bytes,
            write: &mut write,
        };
        http_client::send_to_sink(client, connect_target, raw_request, timeout, sink).await
    };
    match result {
        Ok(response) => Ok((
            response,
            Some(StreamedSave {
                guard,
                bytes: written,
                sha256: format!("{:x}", digest.finalize()),
            }),
        )),
        Err(http_client::SendError::BodyOverCap { declared }) => {
            Err(save_over_cap_error(declared, save))
        }
        Err(http_client::SendError::Sink(text)) => Err(runtime_error(format!(
            "HttpPlugin: could not save {}: {text}",
            guard
                .path()
                .map(Path::display)
                .map(|d| d.to_string())
                .unwrap_or_default()
        ))),
        Err(err) => Err(send_error_to_tool_error(err, original_url)),
    }
}

/// The one true 2xx-success return under `params.save_to` streaming
/// (issue #726): moves [`StreamedSave`]'s own already-written,
/// already-hashed temp file into place -- the only place this module
/// ever calls [`TempFileGuard::keep`].
fn finalize_streamed_save(
    parsed: &ParsedRequest,
    original_url: &str,
    response: http_client::RawResponse,
    save: &SaveTarget,
    streamed: StreamedSave,
) -> Result<ToolResult, ToolError> {
    let status = response.status;
    let ok = !(parsed.fail_on_error && status >= 400);
    let content_type = find_header(&response.headers, "content-type").map(str::to_string);
    let temp_path = streamed.guard.keep();
    if let Err(err) = move_into_place(&temp_path, save) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    let mut headers_dict = Dict::new();
    for (k, v) in &response.headers {
        headers_dict.insert(Value::Str(k.clone()), Value::Str(v.clone()));
    }
    let mut raw = Dict::new();
    raw.insert(
        Value::Str("status".to_string()),
        Value::from(i64::from(status)),
    );
    raw.insert(Value::Str("headers".to_string()), Value::Dict(headers_dict));
    raw.insert(
        Value::Str("url".to_string()),
        Value::Str(original_url.to_string()),
    );
    let mut value = Dict::new();
    value.insert(
        Value::Str("path".to_string()),
        Value::Str(save.path.display().to_string()),
    );
    value.insert(
        Value::Str("bytes".to_string()),
        Value::from(streamed.bytes as i64),
    );
    value.insert(
        Value::Str("sha256".to_string()),
        Value::Str(streamed.sha256),
    );
    value.insert(
        Value::Str("content_type".to_string()),
        content_type.map(Value::Str).unwrap_or(Value::None),
    );
    value.insert(
        Value::Str("status".to_string()),
        Value::from(i64::from(status)),
    );
    Ok(ToolResult {
        value: Value::Dict(value),
        raw: Value::Dict(raw),
        stdout: None,
        stderr: None,
        exit_code: None,
        ok,
        stdout_parsed: false,
        tokens_sent: None,
        tokens_received: None,
        tokens_cached_in: None,
        tokens_cache_write: None,
        cost_usd: None,
    })
}

/// `plugins/http.py::HttpPlugin.execute`'s own `save_to` branch of its
/// final `ToolResult` -- `value: {path, bytes, sha256, content_type,
/// status}`, `raw` without a `body` key, and nothing written for a
/// non-2xx or refused response.
fn finalize_response_save(
    parsed: &ParsedRequest,
    original_url: &str,
    response: http_client::RawResponse,
    final_reason: Option<String>,
    refused: bool,
    save: &SaveTarget,
    scrub: Option<&dyn Fn(&str) -> String>,
) -> Result<ToolResult, ToolError> {
    let status = response.status;
    let ok = !(parsed.fail_on_error
        && (status >= 400
            || refused
            || (parsed.redirects == RedirectMode::Never && (300..400).contains(&status))));
    let content_type = find_header(&response.headers, "content-type").map(str::to_string);
    let mut stderr = None;
    let saved = if (200..300).contains(&status) && !refused {
        Some(save_response_body(&response.body, save)?)
    } else {
        let text = String::from_utf8_lossy(&response.body).into_owned();
        stderr = final_reason.map(|reason| {
            let base = format!("HTTP {status}: {reason}");
            // Issue #725: scrubbed *before* the cut, so no part of a
            // secret survives it (`plugins/http.py::HttpPlugin.execute`).
            let scrubbed = scrub.map(|f| f(&text)).unwrap_or(text);
            let excerpt = http_error_excerpt(&scrubbed, ERROR_EXCERPT_MAX_CHARS, scrub);
            if excerpt.is_empty() {
                base
            } else {
                format!("{base}: {excerpt}")
            }
        });
        None
    };
    let mut headers_dict = Dict::new();
    for (k, v) in &response.headers {
        headers_dict.insert(Value::Str(k.clone()), Value::Str(v.clone()));
    }
    let mut raw = Dict::new();
    raw.insert(
        Value::Str("status".to_string()),
        Value::from(i64::from(status)),
    );
    raw.insert(Value::Str("headers".to_string()), Value::Dict(headers_dict));
    raw.insert(
        Value::Str("url".to_string()),
        Value::Str(original_url.to_string()),
    );
    let mut value = Dict::new();
    value.insert(
        Value::Str("path".to_string()),
        saved
            .as_ref()
            .map(|s| Value::Str(s.path.display().to_string()))
            .unwrap_or(Value::None),
    );
    value.insert(
        Value::Str("bytes".to_string()),
        Value::from(saved.as_ref().map(|s| s.bytes as i64).unwrap_or(0)),
    );
    value.insert(
        Value::Str("sha256".to_string()),
        saved
            .as_ref()
            .map(|s| Value::Str(s.sha256.clone()))
            .unwrap_or(Value::None),
    );
    value.insert(
        Value::Str("content_type".to_string()),
        content_type.map(Value::Str).unwrap_or(Value::None),
    );
    value.insert(
        Value::Str("status".to_string()),
        Value::from(i64::from(status)),
    );
    Ok(ToolResult {
        value: Value::Dict(value),
        raw: Value::Dict(raw),
        stdout: None,
        stderr,
        exit_code: None,
        ok,
        stdout_parsed: false,
        tokens_sent: None,
        tokens_received: None,
        tokens_cached_in: None,
        tokens_cache_write: None,
        cost_usd: None,
    })
}

/// The one call [`HttpPlugin::execute`]'s own dispatch loop makes at
/// every return point, [`finalize_response_save`] once `params.save_to`
/// named a target, [`finalize_response`] otherwise.
fn build_result(
    parsed: &ParsedRequest,
    original_url: &str,
    response: http_client::RawResponse,
    final_reason: Option<String>,
    refused: bool,
    scrub: Option<&dyn Fn(&str) -> String>,
) -> Result<ToolResult, ToolError> {
    match &parsed.save {
        Some(save) => finalize_response_save(
            parsed,
            original_url,
            response,
            final_reason,
            refused,
            save,
            scrub,
        ),
        None => finalize_response(parsed, original_url, response, final_reason, refused, scrub),
    }
}

/// Builds the final [`ToolResult`] from a received response --
/// *final_reason* is `None` for a 2xx (`stderr: None`), `Some(reason)`
/// for anything else (4xx/5xx, or an unfollowed/disallowed/looping
/// 3xx): `stderr = f"HTTP {status}: {reason}"`, with
/// [`http_error_excerpt`] appended when non-empty. `raw.url` is always
/// *original_url* (`plugins/http.py`'s own `url` local, never the
/// final redirected target). `ok = not (fail_on_error and (status >=
/// 400 or refused or (redirects: never and a 3xx)))` -- *refused* is a
/// redirect `params.redirects` would not follow (issue #598).
fn finalize_response(
    parsed: &ParsedRequest,
    original_url: &str,
    response: http_client::RawResponse,
    final_reason: Option<String>,
    refused: bool,
    scrub: Option<&dyn Fn(&str) -> String>,
) -> Result<ToolResult, ToolError> {
    let status = response.status;
    let text = String::from_utf8_lossy(&response.body).into_owned();
    let value = parse_body(&parsed.parse_mode, &response.headers, &text)?;
    let stderr = final_reason.map(|reason| {
        let base = format!("HTTP {status}: {reason}");
        // Issue #725: scrubbed *before* the cut, so no part of a secret
        // survives it (`plugins/http.py::HttpPlugin.execute`).
        let scrubbed = scrub.map(|f| f(&text)).unwrap_or_else(|| text.clone());
        let excerpt = http_error_excerpt(&scrubbed, ERROR_EXCERPT_MAX_CHARS, scrub);
        if excerpt.is_empty() {
            base
        } else {
            format!("{base}: {excerpt}")
        }
    });
    let mut headers_dict = Dict::new();
    for (k, v) in &response.headers {
        headers_dict.insert(Value::Str(k.clone()), Value::Str(v.clone()));
    }
    let mut raw = Dict::new();
    raw.insert(
        Value::Str("status".to_string()),
        Value::from(i64::from(status)),
    );
    raw.insert(Value::Str("headers".to_string()), Value::Dict(headers_dict));
    raw.insert(
        Value::Str("url".to_string()),
        Value::Str(original_url.to_string()),
    );
    raw.insert(Value::Str("body".to_string()), Value::Str(text));
    let ok = !(parsed.fail_on_error
        && (status >= 400
            || refused
            || (parsed.redirects == RedirectMode::Never && (300..400).contains(&status))));
    Ok(ToolResult {
        value,
        raw: Value::Dict(raw),
        stdout: None,
        stderr,
        exit_code: None,
        ok,
        stdout_parsed: false,
        tokens_sent: None,
        tokens_received: None,
        tokens_cached_in: None,
        tokens_cache_write: None,
        cost_usd: None,
    })
}

fn to_string_pairs(pairs: &[(Value, Value)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.py_str(), v.py_str()))
        .collect()
}

/// `req.host`, exactly as the current hop's own URL writes it --
/// userinfo stripped (same as [`split_netloc`] already strips), but
/// the port kept or omitted exactly as written, never filled in with
/// the scheme's own default -- the `Host` header's own shape. The
/// proxy rule's own `no_proxy` matching (issue #778) uses
/// [`split_netloc`]'s hostname alone instead, never this function: a synthetic default
/// port always appended would make a `no_proxy` entry with no port of
/// its own -- the common case -- never match a request whose URL also
/// never wrote one.
pub(crate) fn netloc_host_as_written(netloc: &str) -> &str {
    match netloc.rfind('@') {
        Some(idx) => &netloc[idx + 1..],
        None => netloc,
    }
}

/// [`parse_proxy`]'s own result -- *user*/*password* are still
/// percent-encoded; [`resolve_proxy_target`] unquotes them before
/// building the `Basic` credentials (`ProxyHandler.proxy_open`'s own
/// job, not `_parse_proxy`'s).
struct ParsedProxy {
    user: Option<String>,
    password: Option<String>,
    hostport: String,
}

/// `urllib.request._parse_proxy` -- a proxy environment value is
/// either a full URL (`http://user:pass@host:port`) or bare authority
/// (`host:port`, or `user:pass@host:port` with no scheme at all;
/// [`splittype_with_rest`]'s own scheme match only counts when what
/// follows actually starts with `/`, exactly like CPython's own
/// `r_scheme.startswith("/")` check) -- the authority-form case
/// (issue #485) is the common one: an `http_proxy`
/// value is almost always written as `host:port` with no `http://`
/// prefix at all.
fn parse_proxy(proxy: &str) -> ParsedProxy {
    let (_scheme, r_scheme) = splittype_with_rest(proxy);
    let authority = if !r_scheme.starts_with('/') {
        proxy.to_string()
    } else if let Some(rest) = r_scheme.strip_prefix("//") {
        // The authority ends at the first `/`, `?` or `#` -- the same
        // boundary `urlsplit` (and `http_conventions::proxy_authority`,
        // which already normalized *proxy* before this ever runs) use,
        // so a value like `http://proxy.corp:3128?x` (its own query
        // string, no userinfo) still reads the hostport as
        // `proxy.corp:3128`, never the whole remainder (issue #778).
        match rest.find(['/', '?', '#']) {
            Some(end) => rest[..end].to_string(),
            None => rest.to_string(),
        }
    } else {
        String::new()
    };
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(idx) => (
            Some(authority[..idx].to_string()),
            authority[idx + 1..].to_string(),
        ),
        None => (None, authority),
    };
    let (user, password) = match userinfo {
        Some(u) => match u.split_once(':') {
            Some((user, pass)) => (Some(user.to_string()), Some(pass.to_string())),
            None => (Some(u), None),
        },
        None => (None, None),
    };
    ParsedProxy {
        user,
        password,
        hostport,
    }
}

/// [`http_client::ProxyTarget`] for the current hop, the proxy rule
/// end to end (issue #778): [`http_client::resolve_proxy_checked`]'s own
/// environment lookup/`no_proxy` bypass -- by hostname alone, port
/// stripped, matching [`split_netloc`]'s own target-host comparison --
/// then [`parse_proxy`] for the proxy value itself, unquoting both the
/// hostport and, whenever a user is present (an empty password
/// included), the `Basic` credentials. `Err` when the proxy
/// value itself names an unsupported scheme or a strictly invalid port
/// -- refused before any I/O, the same as the request URL's own port
/// ([`request_host_port`]), *never* silently falling through to a
/// direct connection (issue #778): every caller
/// of this function, on every surface (the `http` tool itself,
/// `electricity-adapters::cyberdiner`, `systemone`'s own webhook
/// probe), propagates this `Err` outright. `pub` (not `pub(crate)`):
/// `electricity-adapters::cyberdiner` reuses this directly for its own
/// proxy resolution (issue #515), rather than re-deriving the
/// proxy-URL/auth parsing a second time.
pub fn resolve_proxy_target(
    scheme: &str,
    netloc: &str,
) -> Result<Option<http_client::ProxyTarget>, String> {
    let env: Vec<(String, String)> = std::env::vars().collect();
    resolve_proxy_target_from(scheme, netloc, &env)
}

/// [`resolve_proxy_target`], over an explicit environment snapshot --
/// the pure, test-driven form (same split as [`http_client::
/// resolve_proxy_checked`]'s own).
fn resolve_proxy_target_from(
    scheme: &str,
    netloc: &str,
    env: &[(String, String)],
) -> Result<Option<http_client::ProxyTarget>, String> {
    let (host_for_bypass, port_for_bypass) = split_netloc(netloc);
    let Some(proxy_value) = http_client::resolve_proxy_checked_with_port(
        scheme,
        &host_for_bypass,
        port_for_bypass,
        env,
    )?
    else {
        return Ok(None);
    };
    let parsed = parse_proxy(&proxy_value);
    let hostport = http_url::unquote(&parsed.hostport);
    let (proxy_host, proxy_port) = split_netloc(&hostport);
    // `http_client::resolve_proxy_checked` already writes `:80` into a
    // portless proxy value explicitly, so this never actually
    // triggers in practice -- kept as a defensive fallback with the
    // proxy rule's own default (the *proxy*'s scheme, always `http`),
    // never `default_port(scheme)`'s own target-scheme-dependent guess
    // (443 for an https target), which is urllib's own quirk, not the
    // proxy rule's (issue #778).
    let proxy_port = proxy_port.unwrap_or(80);
    // `Basic` auth applies whenever a user is present, even with no
    // password at all (`user@proxy`) -- not only when both pieces are
    // non-empty (issue #778).
    let basic_auth = parsed.user.as_ref().map(|user| {
        let user = http_url::unquote(user);
        let password = http_url::unquote(parsed.password.as_deref().unwrap_or(""));
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    });
    Ok(Some(http_client::ProxyTarget {
        host: proxy_host,
        port: proxy_port,
        basic_auth,
    }))
}

// ---------------------------------------------------------------------
// `HttpPlugin`.
// ---------------------------------------------------------------------

pub struct HttpPlugin;

/// `HTTPRedirectHandler.max_repeats`/`max_redirections` -- 4 visits to
/// the same (already-quoted, already-resolved) URL, 10 distinct URLs
/// visited in total.
const MAX_REPEATS: u32 = 4;
const MAX_REDIRECTIONS: usize = 10;

#[async_trait(?Send)]
impl ToolPlugin for HttpPlugin {
    fn name(&self) -> &str {
        "http"
    }

    async fn execute(&self, params: Value, call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        // The scheme/port check, before any I/O and before touching
        // any local file (`save_to`'s own filesystem checks included):
        // checked on the raw URL, before `params['params']` is
        // appended (issue #778).
        let raw_url = extract_url(&params)?;
        check_url(&raw_url)?;
        let parsed = parse_params(raw_url, &params, call.secret_headers)?;
        let original_url = parsed.url.clone();

        let client = http_client::build_client().map_err(runtime_error)?;
        let timeout =
            TimeoutModel::PerOperation(Duration::from_secs(u64::from(call.timeout_seconds.max(1))));

        let mut current_url = parsed.url.clone();
        let mut current_method = parsed.method.clone();
        let mut current_body = parsed.body.clone();
        let mut current_headers = add_headers(&to_string_pairs(&parsed.header_pairs));
        let mut visited: IndexMap<String, u32> = IndexMap::new();

        loop {
            let split = http_url::urlsplit(&current_url, "");
            // The only schemes [`http_client`] ever speaks -- checked
            // on every hop, not just the first, so a redirect to a scheme
            // other than http/https is refused the same way the initial
            // URL is (`cof run` now refuses it too, before ever following
            // such a redirect).
            if !matches!(split.scheme.as_str(), "http" | "https") {
                return Err(scheme_refused(&split.scheme, &current_url));
            }
            let scheme_enum = if split.scheme == "https" {
                Scheme::Https
            } else {
                Scheme::Http
            };
            let (host, port) = request_host_port(&split.netloc, &split.scheme, &current_url)?;
            let proxy = resolve_proxy_target(&split.scheme, &split.netloc).map_err(|message| {
                ToolError {
                    py_class: PyExc::ValueError,
                    message,
                    cause: ErrorCause::None,
                }
            })?;

            let mut target = split.path.clone();
            if target.is_empty() {
                target = "/".to_string();
            }
            if !split.query.is_empty() {
                target.push('?');
                target.push_str(&split.query);
            }
            if let Some(found) = first_disallowed_target_char(&target) {
                return Err(ToolError {
                    py_class: PyExc::InvalidURL,
                    message: invalid_url_text(&target, found),
                    cause: ErrorCause::None,
                });
            }

            let mut send_headers = current_headers.clone();
            if !send_headers.contains_key("Host") {
                // `req.host` verbatim -- the netloc exactly as written
                // in the URL, userinfo stripped, brackets kept for an
                // IPv6 literal. urllib never adds or omits a port
                // here: an explicit `:80`/`:443` that happens to match
                // the scheme's own default still appears in the
                // header; an absent port stays absent (issue #485 --
                // confirmed directly against
                // CPython, not `host_header_value`'s own default-port
                // omission, which is `http.client`'s own *connection*
                // target, a different thing).
                send_headers.insert(
                    "Host".to_string(),
                    netloc_host_as_written(&split.netloc).to_string(),
                );
            }
            if !send_headers.contains_key("Accept-encoding") {
                send_headers.insert("Accept-encoding".to_string(), "identity".to_string());
            }
            // urllib's own default `addheaders` entry (`AbstractHTTPHandler.
            // do_request_`'s own `self.parent.addheaders`, `[('User-agent',
            // client_version)]`) -- `Python-urllib/3.11` there, `electricity/
            // <version>` here (issue #485's own settled question, a
            // documented divergence: some APIs reject a request with no
            // `User-Agent` at all, and the mock harness's own request
            // comparison already excludes this one header by name).
            if !send_headers.contains_key("User-agent") {
                send_headers.insert(
                    "User-agent".to_string(),
                    format!("electricity/{}", env!("CARGO_PKG_VERSION")),
                );
            }
            // A plain `http://` request through a proxy has no
            // `CONNECT` tunnel at all -- the absolute-form request
            // below *is* the one sent to the proxy, so (unlike the
            // tunneled `https://` case, where [`http_client::send`]
            // puts this on the `CONNECT` line only) `Proxy-
            // authorization` belongs in the regular header set here
            // (`AbstractHTTPHandler.do_open`'s own `del headers[
            // proxy_auth_hdr]` only ever fires when `req._tunnel_host`
            // is set).
            if scheme_enum == Scheme::Http {
                if let Some(creds) = proxy.as_ref().and_then(|p| p.basic_auth.as_deref()) {
                    send_headers
                        .insert("Proxy-authorization".to_string(), format!("Basic {creds}"));
                }
            }
            // `AbstractHTTPHandler.do_open` always sets
            // `headers["Connection"] = "close"` itself, overriding
            // any value the caller's own `headers:` supplied -- unlike
            // `Host`/`Accept-encoding` above, which only ever fill in
            // a *missing* header.
            send_headers.insert("Connection".to_string(), "close".to_string());
            if let Some(body) = &current_body {
                if !send_headers.contains_key("Content-type") {
                    send_headers.insert(
                        "Content-type".to_string(),
                        "application/x-www-form-urlencoded".to_string(),
                    );
                }
                if !send_headers.contains_key("Content-length") {
                    send_headers.insert("Content-length".to_string(), body.len().to_string());
                }
            }

            for (_, v) in &send_headers {
                if let Some((pos, ch)) = first_non_latin1_char(v) {
                    return Err(ToolError {
                        py_class: PyExc::UnicodeEncodeError,
                        message: unicode_encode_error_text(pos, ch),
                        cause: ErrorCause::None,
                    });
                }
            }

            let connect_target = ConnectTarget {
                scheme: scheme_enum,
                host: host.clone(),
                port,
                proxy: proxy.clone(),
            };
            let wire_target = match &proxy {
                Some(_) if scheme_enum == Scheme::Http => {
                    format!(
                        "{}://{}{}",
                        split.scheme,
                        netloc_host_as_written(&split.netloc),
                        target
                    )
                }
                _ => target.clone(),
            };

            let header_vec: Vec<(String, String)> = send_headers.into_iter().collect();
            let raw_request = RawRequest {
                method: current_method.clone(),
                target: wire_target,
                headers: header_vec,
                body: current_body.clone(),
            };

            let send_result = send_hop(
                &client,
                &connect_target,
                &raw_request,
                timeout,
                parsed.save.as_ref(),
                &original_url,
            )
            .await;
            if call.token.is_set() {
                return Err(ToolError::message("cancelled".to_string()));
            }

            let (mut response, streamed) = match send_result {
                Err(err) => return Err(err),
                Ok(pair) => pair,
            };

            let status = response.status;
            if (200..300).contains(&status) {
                // `with urlopen(...) as resp: ... resp.read()` -- a
                // 2xx has no `except HTTPError` handler of its own to
                // swallow a body-read failure into, unlike every
                // other status below (issue #485).
                if let Some(body_error) = response.body_error.clone() {
                    return Err(send_error_to_tool_error(body_error, &original_url));
                }
                if let Some(streamed) = streamed {
                    return finalize_streamed_save(
                        &parsed,
                        &original_url,
                        response,
                        parsed
                            .save
                            .as_ref()
                            .expect("send_hop only returns Some(StreamedSave) when parsed.save is"),
                        streamed,
                    );
                }
                return build_result(&parsed, &original_url, response, None, false, call.scrub);
            }
            // Not a 2xx success: whatever `send_hop` streamed to a temp
            // file (if `params.save_to` is set) is read back once here
            // (bounded already by `save.max_bytes`) so the existing
            // error-excerpt/redirect logic below can keep reading
            // `response.body` exactly as it always has; `streamed`'s own
            // `TempFileGuard` deletes the file at the end of this
            // iteration regardless of which of the branches below is
            // taken next (`finalize_streamed_save` above is the only
            // place that ever keeps one).
            if let Some(streamed) = &streamed {
                if let Some(path) = streamed.guard.path() {
                    response.body = std::fs::read(path).unwrap_or_default();
                }
            }

            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                if let Some(location) = find_header(&response.headers, "location")
                    .or_else(|| find_header(&response.headers, "uri"))
                    .map(str::to_string)
                {
                    let resolved = http_url::urljoin(&current_url, &location);
                    let quoted = http_url::quote(&resolved, http_url::STRING_PUNCTUATION);
                    let target_scheme = http_url::urlsplit(&quoted, "").scheme;
                    // `plugins/http.py::_RedirectPolicy.redirect_request`
                    // (issue #778): the never/strip_auth decision runs
                    // first, on its own terms -- a target this mode
                    // would refuse anyway is refused that way, whatever
                    // its scheme, never the shared scheme refusal's.
                    if parsed.redirects == RedirectMode::Never
                        || (parsed.redirects == RedirectMode::StripAuth
                            && target_scheme.to_lowercase() != "https")
                    {
                        let reason =
                            format!("{}: {}", response.reason, parsed.redirects.refused_note());
                        return build_result(
                            &parsed,
                            &original_url,
                            response,
                            Some(reason),
                            true,
                            call.scrub,
                        );
                    }
                    if let Some(redirect) = http_client::redirect_policy(&current_method, status) {
                        let repeat_count = visited.get(&quoted).copied().unwrap_or(0);
                        if repeat_count >= MAX_REPEATS || visited.len() >= MAX_REDIRECTIONS {
                            let reason = redirect_loop_reason(&response.reason);
                            return build_result(
                                &parsed,
                                &original_url,
                                response,
                                Some(reason),
                                false,
                                call.scrub,
                            );
                        }
                        // Checked only once this hop is actually about
                        // to be followed -- any scheme other than
                        // http/https gets the shared refusal text,
                        // whatever it is (`file:`, `data:`, ... -- not
                        // only `ftp`).
                        crate::http_conventions::check_scheme(
                            &target_scheme,
                            &quoted,
                            &["http", "https"],
                        )
                        .map_err(value_error)?;
                        *visited.entry(quoted.clone()).or_insert(0) += 1;
                        if parsed.redirects == RedirectMode::StripAuth {
                            let origin = redirect_origin(&current_url);
                            if origin.is_none() || origin != redirect_origin(&quoted) {
                                // Issue #725: a header that came from a
                                // secret is dropped alongside `Authorization`
                                // on a cross-origin redirect, the same as
                                // `plugins/http.py::_RedirectPolicy`'s own
                                // `frozenset({"authorization", *secret_headers})`.
                                current_headers.retain(|name, _| {
                                    !name.eq_ignore_ascii_case("authorization")
                                        && !call
                                            .secret_headers
                                            .iter()
                                            .any(|h| name.eq_ignore_ascii_case(h))
                                });
                            }
                        }
                        current_url = quoted;
                        current_method = redirect.method;
                        if redirect.drop_body {
                            current_body = None;
                            current_headers.shift_remove("Content-length");
                            current_headers.shift_remove("Content-type");
                        }
                        continue;
                    }
                    let reason = response.reason.clone();
                    return build_result(
                        &parsed,
                        &original_url,
                        response,
                        Some(reason),
                        false,
                        call.scrub,
                    );
                }
                let reason = response.reason.clone();
                return build_result(
                    &parsed,
                    &original_url,
                    response,
                    Some(reason),
                    false,
                    call.scrub,
                );
            }

            let reason = response.reason.clone();
            return build_result(
                &parsed,
                &original_url,
                response,
                Some(reason),
                false,
                call.scrub,
            );
        }
    }

    fn check(&self) -> CheckResult {
        CheckResult {
            ok: true,
            missing: Vec::new(),
            message: None,
        }
    }
}

#[cfg(test)]
mod http_url_tests {
    use super::http_url::*;

    #[test]
    fn urlsplit_separates_every_component() {
        let s = urlsplit("http://user:pass@host:81/a/b?q=1#frag", "");
        assert_eq!(s.scheme, "http");
        assert_eq!(s.netloc, "user:pass@host:81");
        assert_eq!(s.path, "/a/b");
        assert_eq!(s.query, "q=1");
        assert_eq!(s.fragment, "frag");
    }

    #[test]
    fn urlsplit_no_scheme_at_all() {
        let s = urlsplit("not a url", "");
        assert_eq!(s.scheme, "");
        assert_eq!(s.path, "not a url");
    }

    #[test]
    fn urljoin_resolves_an_absolute_location() {
        assert_eq!(
            urljoin("http://a.example/x", "http://b.example/y"),
            "http://b.example/y"
        );
    }

    #[test]
    fn urljoin_resolves_a_path_absolute_location() {
        assert_eq!(urljoin("http://a.example/x/y", "/z"), "http://a.example/z");
    }

    #[test]
    fn urljoin_resolves_a_relative_location() {
        assert_eq!(urljoin("http://a.example/x/y", "z"), "http://a.example/x/z");
    }

    #[test]
    fn urljoin_resolves_dot_segments() {
        assert_eq!(
            urljoin("http://a.example/x/y/", "../z"),
            "http://a.example/x/z"
        );
    }

    #[test]
    fn urljoin_keeps_an_unrelated_scheme_untouched() {
        assert_eq!(
            urljoin("http://a.example/x", "javascript:alert(1)"),
            "javascript:alert(1)"
        );
    }

    #[test]
    fn quote_keeps_punctuation_safe_and_encodes_space() {
        assert_eq!(quote("http://a/b c", STRING_PUNCTUATION), "http://a/b%20c");
    }

    #[test]
    fn unquote_decodes_percent_encoded_credentials() {
        assert_eq!(unquote("user%40corp"), "user@corp");
        assert_eq!(unquote("plain"), "plain");
    }
}

#[cfg(test)]
mod proxy_parse_tests {
    use super::*;

    #[test]
    fn authority_form_has_no_scheme_and_keeps_the_whole_string_as_hostport() {
        // `urllib.request._parse_proxy("proxy.corp:3128")` -- the
        // common case: no `http://` prefix at all. `_splittype`
        // matches scheme="proxy.corp", rest="3128", which doesn't
        // start with `/`, so the *whole* original string is the
        // authority (issue #485's own worked
        // example).
        let parsed = parse_proxy("proxy.corp:3128");
        assert_eq!(parsed.hostport, "proxy.corp:3128");
        assert!(parsed.user.is_none());
    }

    #[test]
    fn url_form_strips_the_scheme_and_trailing_path() {
        let parsed = parse_proxy("http://proxy.corp:3128/");
        assert_eq!(parsed.hostport, "proxy.corp:3128");
    }

    #[test]
    fn url_form_strips_the_scheme_and_a_trailing_query_or_fragment() {
        // The authority ends at the first `/`, `?` or `#`, not only
        // `/` -- matching cof's own `urlsplit`-backed `split_proxy_url`,
        // which already reads this the same way (issue #778).
        assert_eq!(
            parse_proxy("http://proxy.corp:3128?x").hostport,
            "proxy.corp:3128"
        );
        assert_eq!(
            parse_proxy("http://proxy.corp:3128#x").hostport,
            "proxy.corp:3128"
        );
    }

    #[test]
    fn authority_form_with_credentials_splits_user_and_password() {
        let parsed = parse_proxy("user:pass@proxy.corp:3128");
        assert_eq!(parsed.hostport, "proxy.corp:3128");
        assert_eq!(parsed.user.as_deref(), Some("user"));
        assert_eq!(parsed.password.as_deref(), Some("pass"));
    }

    #[test]
    fn url_form_with_credentials_splits_user_and_password() {
        let parsed = parse_proxy("http://user:pass@proxy.corp:3128");
        assert_eq!(parsed.hostport, "proxy.corp:3128");
        assert_eq!(parsed.user.as_deref(), Some("user"));
        assert_eq!(parsed.password.as_deref(), Some("pass"));
    }

    #[test]
    fn a_bare_user_with_no_password_splits_to_no_password() {
        let parsed = parse_proxy("user@proxy.corp:3128");
        assert_eq!(parsed.user.as_deref(), Some("user"));
        assert!(parsed.password.is_none());
    }

    #[test]
    fn netloc_host_as_written_strips_userinfo_but_keeps_the_port_as_is() {
        assert_eq!(netloc_host_as_written("example.com"), "example.com");
        assert_eq!(
            netloc_host_as_written("user:pass@example.com:8080"),
            "example.com:8080"
        );
    }

    #[test]
    fn netloc_host_as_written_never_drops_an_explicit_default_port() {
        // `req.host` (`AbstractHTTPHandler.do_request_`'s own `Host`
        // header) is the netloc exactly as written -- urllib never
        // omits a port that happens to match the scheme's own default
        // (confirmed directly against CPython: `http://h:80/` sends
        // `Host: h:80`, issue #485).
        assert_eq!(netloc_host_as_written("h:80"), "h:80");
        assert_eq!(netloc_host_as_written("h"), "h");
    }

    #[test]
    fn netloc_host_as_written_keeps_ipv6_brackets() {
        assert_eq!(netloc_host_as_written("[::1]:8080"), "[::1]:8080");
        assert_eq!(netloc_host_as_written("[::1]"), "[::1]");
    }
}

#[cfg(test)]
mod port_tests {
    use super::*;

    #[test]
    fn redirect_origin_follows_plugins_http_origin() {
        let origin = |url: &str| redirect_origin(url);
        assert_eq!(
            origin("https://API.example.com/a?b"),
            Some((
                "https".to_string(),
                "api.example.com".to_string(),
                Some(443)
            ))
        );
        assert_eq!(origin("https://h:443/"), origin("https://h/"));
        assert_eq!(origin("http://h:/"), origin("http://h/"));
        assert_eq!(origin("http://u:p@h:81/"), origin("http://h:81/"));
        assert_ne!(origin("https://h/"), origin("http://h/"));
        assert_ne!(origin("https://h/"), origin("https://h:8443/"));
        assert_ne!(origin("https://h/"), origin("https://other/"));
        assert_eq!(
            origin("http://[::1]:5/"),
            Some(("http".to_string(), "::1".to_string(), Some(5)))
        );
        for unreadable in [
            "http://h:+9/",
            "http://h:0_9/",
            "http://h:99999/",
            "http:///x",
        ] {
            assert_eq!(origin(unreadable), None, "{unreadable}");
        }
    }

    #[test]
    fn split_hostport_follows_get_hostport() {
        assert_eq!(split_hostport("h"), ("h", PortText::Absent));
        assert_eq!(split_hostport("h:"), ("h", PortText::Empty));
        assert_eq!(split_hostport("h:80"), ("h", PortText::Given("80")));
        assert_eq!(split_hostport("u:p@h:abc"), ("h", PortText::Given("abc")));
        assert_eq!(split_hostport("[::1]"), ("::1", PortText::Absent));
        assert_eq!(split_hostport("[::1]:"), ("::1", PortText::Empty));
        assert_eq!(split_hostport("[::1]:abc"), ("::1", PortText::Given("abc")));
    }

    #[test]
    fn request_host_port_follows_the_strict_port_rule() {
        let url = "http://x/";
        assert_eq!(
            request_host_port("h", "http", url).unwrap(),
            ("h".to_string(), 80)
        );
        assert_eq!(
            request_host_port("h:", "https", url).unwrap(),
            ("h".to_string(), 443)
        );
        assert_eq!(
            request_host_port("h:80", "http", url).unwrap(),
            ("h".to_string(), 80)
        );
        assert_eq!(
            request_host_port("[::1]:8080", "http", url).unwrap(),
            ("::1".to_string(), 8080)
        );
        for (netloc, port_text) in [
            ("h:abc", "abc"),
            ("h:+9", "+9"),
            ("h:0_9", "0_9"),
            ("h:0", "0"),
            ("h:99999", "99999"),
            ("h:-1", "-1"),
        ] {
            let err = request_host_port(netloc, "http", url).unwrap_err();
            assert_eq!(err.py_class, PyExc::ValueError);
            assert_eq!(
                err.message,
                format!("invalid port in URL '{url}': '{port_text}'")
            );
            assert_eq!(err.cause, ErrorCause::None);
        }
    }

    #[test]
    fn resolve_proxy_target_refuses_an_invalid_proxy_port_before_any_io() {
        let env = vec![("http_proxy".to_string(), "http://proxy:0".to_string())];
        let err = resolve_proxy_target_from("http", "example.invalid", &env).unwrap_err();
        assert_eq!(err, "invalid port in URL 'http://proxy:0': '0'");
    }

    #[test]
    fn resolve_proxy_target_resolves_a_valid_proxy() {
        let env = vec![("http_proxy".to_string(), "http://proxy:3128".to_string())];
        let target = resolve_proxy_target_from("http", "example.invalid", &env)
            .unwrap()
            .unwrap();
        assert_eq!(target.host, "proxy");
        assert_eq!(target.port, 3128);
    }

    #[test]
    fn resolve_proxy_target_is_none_with_no_proxy_configured() {
        assert!(
            resolve_proxy_target_from("http", "example.invalid", &[])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn resolve_proxy_target_sends_basic_auth_for_a_bare_user_with_no_password() {
        // `Basic` auth applies whenever a user is present, even with
        // an empty password (issue #778).
        let env = vec![(
            "http_proxy".to_string(),
            "http://user@proxy.corp:3128".to_string(),
        )];
        let target = resolve_proxy_target_from("http", "example.invalid", &env)
            .unwrap()
            .unwrap();
        use base64::Engine;
        let expected = base64::engine::general_purpose::STANDARD.encode("user:");
        assert_eq!(target.basic_auth, Some(expected));
    }
}

#[cfg(test)]
mod headers_param_tests {
    use super::*;

    #[test]
    fn a_falsy_headers_value_coerces_to_no_headers_at_all() {
        for value in [
            Value::Str(String::new()),
            Value::Int(electricity_value::IntValue::from(0i64)),
            Value::Float(0.0),
            Value::Bool(false),
        ] {
            assert_eq!(
                coerce_headers_param(Some(&value)).unwrap(),
                Vec::new(),
                "value: {value:?}"
            );
        }
    }

    #[test]
    fn a_truthy_non_mapping_headers_value_still_raises() {
        let value = Value::Str("ab".to_string());
        assert!(coerce_headers_param(Some(&value)).is_err());
    }
}

#[cfg(test)]
mod error_excerpt_scrub_tests {
    use super::*;

    #[test]
    fn scrub_runs_before_the_character_cut_not_after() {
        // Mirrors Python's test_http_error_excerpt_scrubs_a_message_crossing_the_character_cut
        // (tests/plugins/test_base.py): a decoded `error` field long enough that the secret
        // straddles `max_chars` must come out with no 6-character window of the secret left,
        // which only holds if `scrub` runs on the whole field before the cut, not after.
        let secret = "fake-tok3n-straddling-the-cut-xyz";
        let message = format!("{}{}", "x".repeat(490), secret);
        let body = format!(r#"{{"error": "{message}"}}"#);
        let scrub: &dyn Fn(&str) -> String = &|s: &str| s.replace(secret, "[secret:api]");

        let excerpt = http_error_excerpt(&body, ERROR_EXCERPT_MAX_CHARS, Some(scrub));

        for start in 0..secret.len().saturating_sub(5) {
            assert!(
                !excerpt.contains(&secret[start..start + 6]),
                "excerpt still contains a window of the secret: {excerpt:?}"
            );
        }
    }
}
