//! `plugins/http.py::HttpPlugin`, ported -- a single HTTP request/
//! response over the shared client ([`crate::http_client`]), with
//! urllib's own redirect rule, proxy convention and error shapes
//! (issue #485, lane E). See [`http_url`] for the small `urllib.parse`
//! subset (`urlsplit`/`urljoin`/`quote`) this file's own redirect
//! handling needs.

use crate::http_client::{self, ConnectTarget, RawRequest, Scheme, SendError, TimeoutModel};
use crate::{CheckResult, ErrorCause, PyExc, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::pycompat::py_strip;
use electricity_value::{Dict, Value};
use indexmap::IndexMap;
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

/// A scheme `urlopen()` recognizes the syntax of but has no handler
/// for (`OpenerDirector.open`'s own dispatch, `getattr(handler, name +
/// "_open")` failing, `unknown_open` raising `URLError('unknown url
/// type: %s' % req.type)`) -- wrapped, inside the same `try/except
/// URLError` as a connect failure, and retried the same way
/// (`ErrorCause::NoReply`).
fn unknown_url_type_failure(original_url: &str, scheme: &str) -> ToolError {
    ToolError {
        py_class: PyExc::RuntimeError,
        message: format!("HTTP request to {original_url} failed: unknown url type: {scheme}"),
        cause: ErrorCause::NoReply,
    }
}

/// `ftp:`/`file:`/`data:` -- schemes urllib itself *does* have a
/// handler for (`FTPHandler`/`DataHandler`/`FileHandler`, installed by
/// `build_opener()`'s own defaults), so `cof run` speaks real FTP,
/// reads a local file, or decodes an inline `data:` URL. This crate
/// implements none of that (never read a local file, never speak FTP)
/// -- a documented divergence (DESIGN.md), refused before any I/O
/// with electricity's own text, never cof's own eventual `TypeError`
/// (`int(resp.status)` on a `None` status, for file/data) or a real
/// FTP round trip.
/// `HTTPRedirectHandler.http_error_302`'s own `"%s - Redirection to
/// url '%s' is not allowed" % (msg, newurl)` -- pinned against real
/// CPython in `generate_http_corpus.py`'s own `redirect_reason_cases`
/// (issue #485 review finding 6), since this file never actually
/// raises through a real `HTTPRedirectHandler` to get the text from
/// CPython itself.
pub fn disallowed_redirect_scheme_reason(msg: &str, newurl: &str) -> String {
    format!("{msg} - Redirection to url '{newurl}' is not allowed")
}

/// `HTTPRedirectHandler.http_error_302`'s own `inf_msg + msg` (the two
/// loop limits, `max_repeats`/`max_redirections`) -- same provenance
/// note as [`disallowed_redirect_scheme_reason`] above.
pub fn redirect_loop_reason(msg: &str) -> String {
    format!(
        "The HTTP server returned a redirect error that would lead to an infinite loop.\nThe last 30x error message was:\n{msg}"
    )
}

fn unsupported_in_electricity_failure(original_url: &str, scheme: &str) -> ToolError {
    ToolError {
        py_class: PyExc::RuntimeError,
        message: format!(
            "HTTP request to {original_url} failed: scheme '{scheme}' is not supported by electricity (http/https only)"
        ),
        cause: ErrorCause::NoReply,
    }
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
mod http_url {
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
pub fn http_error_excerpt(body: &str, max_chars: usize) -> String {
    let text = py_strip(body);
    if text.is_empty() {
        return String::new();
    }
    let slice_chars = |s: &str| -> String {
        match s.char_indices().nth(max_chars) {
            Some((byte_idx, _)) => s[..byte_idx].to_string(),
            None => s.to_string(),
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

/// `urllib.request._splittype` -- `re.compile('([^/:]+):(.*)',
/// re.DOTALL)`'s own `match` (anchored at position 0, so a leading `/`
/// or no `:` at all never matches): `None` when the regex doesn't
/// match -- `Request.__init__`'s own `ValueError: unknown url type:
/// %r`, raised unwrapped at parse time, before `urlopen` ever runs
/// (`unknown_scheme_or_not_http` below is the separate, *wrapped*
/// check for a scheme this regex *does* recognize but `urlopen` has no
/// handler for, e.g. `ftp-nope:` or a bare `127.0.0.1:8080/x`, the
/// scheme it finds here being an IPv4 address up to its own first
/// `:`). Deliberately looser than [`http_url::urlsplit`]'s own RFC
/// 3986 scheme grammar (which requires an alphabetic first character)
/// -- the two disagree on exactly this kind of input, and this is the
/// one urllib itself actually runs at parse time.
fn splittype(url: &str) -> Option<String> {
    splittype_with_rest(url).0
}

/// `urllib.request._splittype`'s own full result (scheme lowercased,
/// plus everything after the first matched `:`) -- [`splittype`]
/// above only ever needs the scheme; [`resolve_proxy_target`]'s own
/// `_parse_proxy` port needs the rest too, to tell a URL-form proxy
/// value (`http://user:pass@host:port`) apart from an authority-form
/// one (`host:port`, or `user:pass@host:port` with no scheme at all).
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

/// The port text, when *netloc* carries one that is syntactically
/// numeric but outside `u16`'s range (`0..=65535`) -- [`split_netloc`]
/// itself can't tell this apart from "no port at all" (its own
/// `parse::<u16>().ok()` returns `None` either way), which would
/// otherwise silently fall back to the scheme's own default port and
/// actually open a TCP connection to a *different* port than the one
/// written (probed directly: `http://127.0.0.1:99999/x` connecting to
/// port 80). CPython never gets this far either -- `getaddrinfo`
/// itself rejects the out-of-range numeric "service" before any
/// packet goes out (`[Errno 8] nodename nor servname provided, or not
/// known` on macOS) -- but that OS text isn't reproduced here (a
/// documented divergence, DESIGN.md): no socket of any kind is ever
/// opened for this case, which is the part that matters.
fn out_of_range_port(netloc: &str) -> Option<String> {
    let hostinfo = match netloc.rfind('@') {
        Some(idx) => &netloc[idx + 1..],
        None => netloc,
    };
    let port_str = if let Some(rest) = hostinfo.strip_prefix('[') {
        rest.split_once(']')
            .and_then(|(_, after)| after.strip_prefix(':'))
    } else {
        hostinfo.rsplit_once(':').map(|(_, p)| p)
    };
    port_str
        .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) && p.parse::<u16>().is_err())
        .map(str::to_string)
}

/// `http.client.HTTPConnection.putrequest`'s own `Host` header: the
/// port is omitted when it's the scheme's own default.
fn host_header_value(host: &str, port: u16, scheme: &str) -> String {
    if port == default_port(scheme) {
        host.to_string()
    } else {
        format!("{host}:{port}")
    }
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
fn coerce_headers_param(value: Option<&Value>) -> Result<Vec<(Value, Value)>, ToolError> {
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

fn parse_params(params: &Value) -> Result<ParsedRequest, ToolError> {
    let url_value = get(params, "url");
    let url = match url_value {
        Some(Value::Str(s)) if !py_strip(s).is_empty() => py_strip(s).to_string(),
        _ => {
            return Err(value_error(
                "HttpPlugin requires params['url'] as a non-empty string.",
            ));
        }
    };

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

    let json_body = present(get(params, "json"));
    let raw_body = present(get(params, "body"));
    if json_body.is_some() && raw_body.is_some() {
        return Err(value_error(
            "HttpPlugin: pass either params['json'] or params['body'], not both.",
        ));
    }

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

    Ok(ParsedRequest {
        url,
        method,
        header_pairs,
        body,
        parse_mode,
        fail_on_error,
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

/// Builds the final [`ToolResult`] from a received response --
/// *final_reason* is `None` for a 2xx (`stderr: None`), `Some(reason)`
/// for anything else (4xx/5xx, or an unfollowed/disallowed/looping
/// 3xx): `stderr = f"HTTP {status}: {reason}"`, with
/// [`http_error_excerpt`] appended when non-empty. `raw.url` is always
/// *original_url* (`plugins/http.py`'s own `url` local, never the
/// final redirected target). `ok = not (fail_on_error and status >= 400)`.
fn finalize_response(
    parsed: &ParsedRequest,
    original_url: &str,
    response: http_client::RawResponse,
    final_reason: Option<String>,
) -> Result<ToolResult, ToolError> {
    let status = response.status;
    let text = String::from_utf8_lossy(&response.body).into_owned();
    let value = parse_body(&parsed.parse_mode, &response.headers, &text)?;
    let stderr = final_reason.map(|reason| {
        let base = format!("HTTP {status}: {reason}");
        let excerpt = http_error_excerpt(&text, ERROR_EXCERPT_MAX_CHARS);
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
    let ok = !(parsed.fail_on_error && status >= 400);
    Ok(ToolResult {
        value,
        raw: Value::Dict(raw),
        stdout: None,
        stderr,
        exit_code: None,
        ok,
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
/// the scheme's own default. `proxy_bypass_environment`'s own *host*
/// argument (issue #485 review finding 7: a synthetic default port
/// always appended, as this function used to, makes a `no_proxy`
/// entry with no port of its own -- the common case -- never match a
/// request whose URL also never wrote one).
fn netloc_host_as_written(netloc: &str) -> &str {
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
/// (issue #485 review finding 7) is the common one: an `http_proxy`
/// value is almost always written as `host:port` with no `http://`
/// prefix at all.
fn parse_proxy(proxy: &str) -> ParsedProxy {
    let (_scheme, r_scheme) = splittype_with_rest(proxy);
    let authority = if !r_scheme.starts_with('/') {
        proxy.to_string()
    } else if let Some(rest) = r_scheme.strip_prefix("//") {
        match rest.find('/') {
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

/// [`http_client::ProxyTarget`] for the current hop, urllib's own
/// convention end to end: [`http_client::resolve_proxy`]'s own
/// environment lookup/`no_proxy` bypass, then [`parse_proxy`] for the
/// proxy value itself, unquoting both the hostport and (when both a
/// user and a password are present) the `Basic` credentials.
fn resolve_proxy_target(scheme: &str, netloc: &str) -> Option<http_client::ProxyTarget> {
    let host_for_bypass = netloc_host_as_written(netloc);
    let proxy_value = http_client::resolve_proxy(scheme, host_for_bypass)?;
    let parsed = parse_proxy(&proxy_value);
    let hostport = http_url::unquote(&parsed.hostport);
    let (proxy_host, proxy_port) = split_netloc(&hostport);
    let proxy_port = proxy_port.unwrap_or_else(|| default_port(scheme));
    let basic_auth = match (&parsed.user, &parsed.password) {
        (Some(user), Some(password)) if !user.is_empty() && !password.is_empty() => {
            let user = http_url::unquote(user);
            let password = http_url::unquote(password);
            use base64::Engine;
            Some(base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}")))
        }
        _ => None,
    };
    Some(http_client::ProxyTarget {
        host: proxy_host,
        port: proxy_port,
        basic_auth,
    })
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
        let parsed = parse_params(&params)?;
        let original_url = parsed.url.clone();

        let initial_scheme = match splittype(&parsed.url) {
            None => {
                return Err(value_error(format!(
                    "unknown url type: {}",
                    Value::Str(parsed.url.clone()).py_repr()
                )));
            }
            Some(scheme) => scheme,
        };
        if !matches!(initial_scheme.as_str(), "http" | "https") {
            return Err(if matches!(initial_scheme.as_str(), "ftp" | "file" | "data") {
                unsupported_in_electricity_failure(&original_url, &initial_scheme)
            } else {
                unknown_url_type_failure(&original_url, &initial_scheme)
            });
        }

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
            // The only schemes [`http_client`] ever speaks -- reached on
            // every hop, not just the first: a redirect to `ftp://` is
            // allowed by [`redirect_policy`]'s own method/status check
            // (urllib's own `HTTPRedirectHandler` would really follow
            // it and speak FTP for real, a real handler being
            // installed by `build_opener()`'s own defaults) but this
            // crate never implements FTP/file/data at all -- refused
            // before any I/O, same wrapped shape as an unrecognized
            // scheme on the initial URL, and recorded as a divergence
            // (DESIGN.md).
            if !matches!(split.scheme.as_str(), "http" | "https") {
                return Err(if matches!(split.scheme.as_str(), "ftp" | "file" | "data") {
                    unsupported_in_electricity_failure(&original_url, &split.scheme)
                } else {
                    unknown_url_type_failure(&original_url, &split.scheme)
                });
            }
            let scheme_enum = if split.scheme == "https" {
                Scheme::Https
            } else {
                Scheme::Http
            };
            if let Some(bad_port) = out_of_range_port(&split.netloc) {
                return Err(ToolError {
                    py_class: PyExc::RuntimeError,
                    message: format!(
                        "HTTP request to {original_url} failed: port out of range 0-65535: {bad_port}"
                    ),
                    cause: ErrorCause::NoReply,
                });
            }
            let (host, parsed_port) = split_netloc(&split.netloc);
            let port = parsed_port.unwrap_or_else(|| default_port(&split.scheme));
            let proxy = resolve_proxy_target(&split.scheme, &split.netloc);

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
                send_headers.insert(
                    "Host".to_string(),
                    host_header_value(&host, port, &split.scheme),
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
                    send_headers.insert(
                        "Proxy-authorization".to_string(),
                        format!("Basic {creds}"),
                    );
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
                        host_header_value(&host, port, &split.scheme),
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

            let send_result =
                http_client::send(&client, &connect_target, &raw_request, timeout).await;
            if call.token.is_set() {
                return Err(ToolError::message("cancelled".to_string()));
            }

            let response = match send_result {
                Err(SendError::Connect(reason)) => {
                    return Err(ToolError {
                        py_class: PyExc::RuntimeError,
                        message: format!("HTTP request to {original_url} failed: {reason}"),
                        cause: ErrorCause::NoReply,
                    });
                }
                Err(SendError::Timeout) => {
                    return Err(ToolError {
                        py_class: PyExc::TimeoutError,
                        message: "timed out".to_string(),
                        cause: ErrorCause::NoReply,
                    });
                }
                Err(SendError::RemoteDisconnected) => {
                    return Err(ToolError {
                        py_class: PyExc::RemoteDisconnected,
                        message: "Remote end closed connection without response".to_string(),
                        cause: ErrorCause::NoReply,
                    });
                }
                Err(SendError::IncompleteRead { read, expected }) => {
                    return Err(ToolError {
                        py_class: PyExc::IncompleteRead,
                        message: format!(
                            "IncompleteRead({read} bytes read, {expected} more expected)"
                        ),
                        cause: ErrorCause::None,
                    });
                }
                Err(SendError::Other(text)) => return Err(runtime_error(text)),
                Ok(response) => response,
            };

            let status = response.status;
            if (200..300).contains(&status) {
                return finalize_response(&parsed, &original_url, response, None);
            }

            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                if let Some(location) = find_header(&response.headers, "location")
                    .or_else(|| find_header(&response.headers, "uri"))
                    .map(str::to_string)
                {
                    let resolved = http_url::urljoin(&current_url, &location);
                    let quoted = http_url::quote(&resolved, http_url::STRING_PUNCTUATION);
                    let target_scheme = http_url::urlsplit(&quoted, "").scheme;
                    if !matches!(target_scheme.as_str(), "" | "http" | "https" | "ftp") {
                        let reason = disallowed_redirect_scheme_reason(&response.reason, &quoted);
                        return finalize_response(&parsed, &original_url, response, Some(reason));
                    }
                    if let Some(redirect) = http_client::redirect_policy(&current_method, status) {
                        let repeat_count = visited.get(&quoted).copied().unwrap_or(0);
                        if repeat_count >= MAX_REPEATS || visited.len() >= MAX_REDIRECTIONS {
                            let reason = redirect_loop_reason(&response.reason);
                            return finalize_response(
                                &parsed,
                                &original_url,
                                response,
                                Some(reason),
                            );
                        }
                        *visited.entry(quoted.clone()).or_insert(0) += 1;
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
                    return finalize_response(&parsed, &original_url, response, Some(reason));
                }
                let reason = response.reason.clone();
                return finalize_response(&parsed, &original_url, response, Some(reason));
            }

            let reason = response.reason.clone();
            return finalize_response(&parsed, &original_url, response, Some(reason));
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
        // authority (issue #485 review finding 7's own worked
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
    fn a_bare_user_with_no_password_builds_no_basic_auth() {
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
}
