//! electricity-redaction: credential-shaped-value redaction
//! (`cli/redaction.py::redact`, ported word for word), and the
//! `meta.raw` size cap (issue #431's Scope section).
//!
//! This is defense in depth, not a guarantee -- the same caveat
//! `cli/redaction.py`'s own module docs carry: it targets the common
//! shapes of an accidental leak (an `api_key`-named field, a `base_url`
//! with userinfo, a JWT/API-key-shaped string) in whatever electricity
//! writes to disk or prints (`--out`, `--live-state`, `--events`,
//! `runtime.effective_settings`, a tool's `meta.raw`/
//! `meta.params_rendered`). It is never the recommended way to pass a
//! real credential into an orchestration -- an environment variable is.

use electricity_value::{Dict, Value};
use regex::Regex;
use std::sync::OnceLock;

/// `cli/redaction.py::REDACTED` -- the literal value a sensitive field
/// is replaced with.
pub const REDACTED: &str = "***REDACTED***";

/// `core/tool.py::_RAW_META_MAX_BYTES` -- the cap on a tool's `meta.raw`,
/// serialized, after redaction.
pub const RAW_META_MAX_BYTES: usize = 64 * 1024;

/// `cli/redaction.py::_SENSITIVE_KEY_RE` -- a dict key (case-insensitive,
/// matched against the whole key, same as Python's `re.match` anchored
/// by this pattern's own leading `^`/trailing `$`) whose *value* is
/// redacted outright, regardless of what that value looks like.
///
/// Python builds this without `re.MULTILINE`, so its own `$` matches
/// either at the very end of the string *or* just before one trailing
/// `\n` -- `re.match(r"a$", "a\n")` is truthy, `re.match(r"a$", "a\n\n")`
/// is not. The `regex` crate's `$` (no `(?m)` flag set here either) only
/// ever matches end-of-haystack, so every pattern below spells that same
/// one-trailing-newline allowance out explicitly as `(?:\n)?$`.
fn sensitive_key_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^(.*[_\-.])?(api[_-]?key|access[_-]?key|secret[_-]?key|auth[_-]?token|access[_-]?token|bearer[_-]?token|id[_-]?token|refresh[_-]?token|session[_-]?token|csrf[_-]?token|authorization|password|passphrase|client[_-]?secret|set[_-]?cookie|cookie|secret|token|credentials?)(?:\n)?$",
        )
        .expect("a fixed, hand-checked pattern")
    })
}

/// `cli/redaction.py::_JWT_RE` -- a standalone JWT (three base64url
/// segments separated by dots, header-prefixed `eyJ` so a dotted
/// hostname/path/version string never matches it by accident). See
/// [`sensitive_key_re`]'s own doc comment for why `(?:\n)?$` replaces a
/// bare trailing `$`.
fn jwt_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+(?:\n)?$")
            .expect("a fixed, hand-checked pattern")
    })
}

/// `cli/redaction.py::_KEYISH_RE` -- common API-key shapes (a vendor
/// prefix plus a long base64url-ish/hex run, or a `Bearer <token>`
/// string), case-sensitive like Python's own pattern (no `(?i)`). See
/// [`sensitive_key_re`]'s own doc comment for why `(?:\n)?$` replaces
/// every bare trailing `$`.
fn keyish_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?:^sk-[A-Za-z0-9_-]{20,}(?:\n)?$)|(?:^xox[abposr]-[A-Za-z0-9-]{20,}(?:\n)?$)|(?:^ghp_[A-Za-z0-9]{30,}(?:\n)?$)|(?:^Bearer\s+[A-Za-z0-9_.=-]{16,}(?:\n)?$)",
        )
        .expect("a fixed, hand-checked pattern")
    })
}

fn is_sensitive_key(key: &str) -> bool {
    sensitive_key_re().is_match(key)
}

/// `http_conventions.py::_CREDENTIAL_QUERY_KEY_RE` -- a case-insensitive
/// *search* (not a full match) for any of these words anywhere in a
/// query key.
fn looks_like_credential_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    ["key", "token", "secret", "auth", "password", "pwd"]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// `cli/redaction.py::_redact_url` -- `main`'s own, unmodified
/// redaction of an ordinary string: strips `user[:pass]@` userinfo
/// from a URL's netloc, keeping the scheme, host:port, path, query and
/// fragment untouched. Only ever reached from [`redact_string`]'s own
/// `"://" in value && "@" in value` gate, so it only has to recognize
/// the one shape that gate already promises. Ports the subset of
/// CPython 3.11's `urllib.parse.urlsplit`/`urlunsplit` that
/// `_redact_url` itself depends on (leading-control/space stripping,
/// embedded tab/CR/LF removal, lowercase scheme, and `urlunsplit`'s
/// own empty-`?`/`#` rules) rather than a full `url`-crate split.
///
/// Two of `urlsplit`'s own checks are deliberately not ported:
/// `_checknetloc`'s Unicode-NFKC homograph check, and
/// `_check_bracketed_netloc`'s deeper IPv6-literal validation (both are
/// rare, and both only *narrow* what Python redacts -- a netloc that
/// fails either raises `ValueError` in Python, which `_redact_url`
/// catches by returning *value* unchanged, so skipping them here can
/// only make this redact a URL Python would have left alone, never the
/// reverse). The bracket-*presence* check (`'[' in netloc` XOR `']' in
/// netloc`) is ported, since getting that wrong could under-redact.
fn redact_embedded_url(value: &str) -> String {
    let lstripped = value.trim_start_matches(|c: char| (c as u32) <= 0x20);
    let cleaned: String = lstripped
        .chars()
        .filter(|&c| c != '\u{09}' && c != '\u{0d}' && c != '\u{0a}')
        .collect();

    let Some(colon) = cleaned.find(':') else {
        return value.to_string();
    };
    let starts_alpha = cleaned
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic());
    if colon == 0 || !starts_alpha {
        return value.to_string();
    }
    let candidate = &cleaned[..colon];
    let scheme_is_valid = candidate
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.');
    if !scheme_is_valid {
        return value.to_string();
    }
    let scheme = candidate.to_lowercase();
    let rest = &cleaned[colon + 1..];

    let Some(after_slashes) = rest.strip_prefix("//") else {
        return value.to_string();
    };
    let netloc_end = after_slashes
        .find(['/', '?', '#'])
        .unwrap_or(after_slashes.len());
    let netloc = &after_slashes[..netloc_end];
    let mut url = &after_slashes[netloc_end..];

    if netloc.contains('[') != netloc.contains(']') {
        return value.to_string();
    }

    if netloc.is_empty() {
        return value.to_string();
    }
    let Some(at) = netloc.rfind('@') else {
        return value.to_string();
    };
    let host = &netloc[at + 1..];

    let mut fragment = "";
    if let Some(idx) = url.find('#') {
        fragment = &url[idx + 1..];
        url = &url[..idx];
    }
    let mut query = "";
    if let Some(idx) = url.find('?') {
        query = &url[idx + 1..];
        url = &url[..idx];
    }

    let mut out = if url.is_empty() {
        String::new()
    } else if url.starts_with('/') {
        url.to_string()
    } else {
        format!("/{url}")
    };
    out = format!("//{REDACTED}@{host}{out}");
    out = format!("{scheme}:{out}");
    if !query.is_empty() {
        out = format!("{out}?{query}");
    }
    if !fragment.is_empty() {
        out = format!("{out}#{fragment}");
    }
    out
}

/// `cli/redaction.py::_redact_string`'s own JWT/key-shape check --
/// `main`'s own ordinary-string rule applied first, even to a string
/// under a URL key ([`is_url_key`]): a token-shaped value is redacted
/// wholesale regardless of the key it's under, same as it always was
/// on `main` (issue #819 follow-up -- a URL key's own value is never
/// *only* sent through [`redact_url`]).
fn is_token_shaped(value: &str) -> bool {
    jwt_re().is_match(value) || keyish_re().is_match(value)
}

/// `cli/redaction.py::_redact_string` -- `main`'s own, unmodified
/// redaction of an ordinary string: a JWT or API-key-shaped string
/// redacts wholesale; a URL carrying both `://` and `@` goes through
/// [`redact_embedded_url`]; anything else passes through unchanged.
/// The URL rule in [`redact_url`] never applies here -- only to a
/// string under a URL key ([`is_url_key`]), handled by [`redact`]
/// itself.
fn redact_string(value: &str) -> String {
    if is_token_shaped(value) {
        return REDACTED.to_string();
    }
    if value.contains("://") && value.contains('@') {
        return redact_embedded_url(value);
    }
    value.to_string()
}

/// Circuitry's own literal URL key names (`http_conventions.py::
/// _URL_KEY_NAMES`), plus any key ending in `_url` or `_proxy` (which
/// already covers the proxy environment variables: `http_proxy`,
/// `https_proxy`, `all_proxy`, `no_proxy`, either case). Matched
/// case-insensitively.
const URL_KEY_NAMES: &[&str] = &[
    "url", "base_url", "api_base", "endpoint", "expo_url", "proxy",
];

/// `cli/redaction.py::_is_url_key` -- whether *key* names a value
/// [`redact_url`] applies to, regardless of its content.
fn is_url_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    URL_KEY_NAMES.contains(&lower.as_str()) || lower.ends_with("_url") || lower.ends_with("_proxy")
}

/// A minimal `%XX`-hex URI-percent-decoder, used only to match a
/// query key against [`looks_like_credential_key`] in its decoded form
/// (issue #819 follow-up: `?tok%65n=...` must still be recognized as
/// `token`) -- never to transform the value text that is actually
/// written back out, which stays exactly as given. Every byte that
/// isn't a well-formed `%` escape passes through unchanged.
fn percent_decode_key(key: &str) -> String {
    fn hex_val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = key.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| key.to_string())
}

/// Whether `rest[start..end]` contains a `:` that can't be a port: the
/// text after it, up to the next `/`, `?`, `#` or `@`, has a character
/// that isn't an ASCII digit (issue #819 follow-up). Scans every `:`
/// in the span, not just the first -- the span never contains an `@`
/// of its own, since *end* is always the position of the next one.
fn has_non_port_colon(rest: &str, start: usize, end: usize) -> bool {
    let mut pos = start;
    loop {
        let Some(colon) = rest[pos..end].find(':').map(|idx| pos + idx) else {
            return false;
        };
        let mut sep_end = rest.len();
        for sep in ['/', '?', '#', '@'] {
            if let Some(idx) = rest[colon + 1..].find(sep) {
                sep_end = sep_end.min(colon + 1 + idx);
            }
        }
        let port_text = &rest[colon + 1..sep_end];
        if port_text.is_empty() || !port_text.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
        pos = colon + 1;
    }
}

/// `http_conventions.py::redact_url` -- the one function behind every
/// URL redaction on this engine (issue #819 follow-up). Every other
/// caller that names a request URL or a proxy value in a refusal,
/// warning, event, state or hint calls this with `marker=""` (drop the
/// credential, with its `@`, exactly as `main` always has). The
/// effective-settings snapshot redactor ([`redact`] itself) calls it
/// with `marker=REDACTED` for a value under a URL key, to show a
/// credential was there rather than silently removing all trace of it.
///
/// Nothing else about *url* changes: no percent-decoding, no
/// re-encoding, no case change, no separator normalization -- a
/// credential-free URL, an unmasked query value, an empty fragment,
/// `HTTPS://`, non-ASCII text and percent-encoding all come back byte
/// for byte.
///
/// The rule:
///
/// - **Userinfo in the authority.** The authority is the text right
///   after `scheme://`, up to the first `/`, `?` or `#` (or, with no
///   `scheme://` at all, *url* itself). When it contains an `@`,
///   everything before its last `@` is replaced -- whether or not
///   another `@` also appears later in *url*
///   (`https://tok@host/@acme/widget`).
/// - **A `:` before a later `@`, when it can't be a port.** When an
///   `@` comes after the authority (in the path, query or fragment)
///   and, between the start of the host (right after any authority
///   userinfo just cut) and that later `@`, a `:` appears whose
///   following text (up to the next `/`, `?`, `#` or `@`) isn't all
///   ASCII digits ([`has_non_port_colon`]), the whole span up to that
///   `@` is replaced instead -- this catches a password hidden behind
///   a `/`. A `:` whose text could be a real port
///   (`http://localhost:8080/@alice`), or no `:` at all, leaves the
///   `@` untouched.
/// - **No `://` at all.** A schemeless value with an `@` is cut before
///   its last `@` unconditionally -- no `:` required, and whitespace
///   is no exception: every caller here has already committed to
///   treating *url* as a URL (a URL-key value, or a request/proxy
///   value), never prose, so a bare `token@host` is itself a
///   credential.
/// - Either cut replaces the matched span with `marker` followed by
///   `@` when *marker* is non-empty, or with nothing at all (dropping
///   the `@` too) when it is empty.
/// - **Query.** Every `&`-separated `key=value` pair (raw text,
///   nothing decoded or re-encoded) whose KEY, percent-decoded
///   ([`percent_decode_key`]), looks like a credential
///   ([`looks_like_credential_key`]) has its VALUE's raw text replaced
///   -- with *marker* when non-empty, or the fixed `%2A%2A%2A` (`main`'s
///   own percent-encoded `***`) when empty, so a value without a
///   marker still fails `has_redacted_secrets` for the right reason. A
///   `?` that comes after a `#` is fragment text, never a query
///   delimiter.
pub fn redact_url(url: &str, marker: &str) -> String {
    // A scheme boundary is the FIRST ':' in the value, immediately
    // followed by "//" -- not just the first occurrence of the
    // literal text "://" anywhere in it. "alice:p://w@host/x" has no
    // scheme of its own by this rule (its first ':' is followed by
    // 'p', not "//"), so it falls through to the schemeless branch
    // below, same as a real `urlsplit` would (its own first ':' ends
    // a candidate scheme only when a "//" authority immediately
    // follows it).
    let scheme_end = url
        .find(':')
        .filter(|&idx| url[idx + 1..].starts_with("//"));
    let (prefix, rest) = match scheme_end {
        Some(idx) => (&url[..idx + 3], &url[idx + 3..]),
        None => ("", url),
    };

    let userinfo_prefix: String = if marker.is_empty() {
        String::new()
    } else {
        format!("{marker}@")
    };

    let owned_rest: String;
    let rest: &str = match scheme_end {
        None => {
            // Schemeless: every caller here has already committed to
            // treating *url* as a URL (a URL-key value, or a request/
            // proxy value), so a bare `token@host` -- no ':' at all,
            // and whitespace no exception -- is itself a credential,
            // not just the user:pass@host shape.
            match rest.rfind('@') {
                Some(last_at) => {
                    owned_rest = format!("{userinfo_prefix}{}", &rest[last_at + 1..]);
                    &owned_rest
                }
                None => rest,
            }
        }
        Some(_) => {
            let mut authority_end = ['/', '?', '#']
                .iter()
                .filter_map(|sep| rest.find(*sep))
                .min()
                .unwrap_or(rest.len());

            let mut host_start = 0usize;
            let mut cut_rest: String = rest.to_string();
            if let Some(authority_at) = rest[..authority_end].rfind('@') {
                cut_rest = format!("{userinfo_prefix}{}", &rest[authority_at + 1..]);
                authority_end = authority_end + userinfo_prefix.len() - (authority_at + 1);
                host_start = userinfo_prefix.len();
            }

            match cut_rest.rfind('@') {
                Some(last_at)
                    if last_at >= authority_end
                        && has_non_port_colon(&cut_rest, host_start, last_at) =>
                {
                    owned_rest = format!(
                        "{}{userinfo_prefix}{}",
                        &cut_rest[..host_start],
                        &cut_rest[last_at + 1..]
                    );
                    &owned_rest
                }
                _ => {
                    owned_rest = cut_rest;
                    &owned_rest
                }
            }
        }
    };

    let (body, fragment) = match rest.find('#') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    let Some(query_idx) = body.find('?') else {
        return format!("{prefix}{body}{fragment}");
    };
    let query_mask = if marker.is_empty() {
        "%2A%2A%2A"
    } else {
        marker
    };
    let masked_query: Vec<String> = body[query_idx + 1..]
        .split('&')
        .map(|segment| match segment.split_once('=') {
            Some((key, _)) if looks_like_credential_key(&percent_decode_key(key)) => {
                format!("{key}={query_mask}")
            }
            _ => segment.to_string(),
        })
        .collect();
    format!(
        "{prefix}{}?{}{fragment}",
        &body[..query_idx],
        masked_query.join("&")
    )
}

/// `cli/redaction.py::_redact_adapter_env` -- a `runtime.adapters` block
/// with every `<adapter>.env` value replaced by [`REDACTED`] (the
/// variable names stay; an `env` that is not a mapping is replaced whole), and the rest redacted as usual.
fn redact_adapter_env(mut adapters: Value) -> Value {
    let Value::Dict(dict) = &mut adapters else {
        return redact(adapters);
    };
    let taken = std::mem::take(dict);
    let mut out = Dict::with_capacity(taken.len());
    for (name, mut entry) in taken {
        if let Value::Dict(entry_dict) = &mut entry {
            let env_key = Value::Str("env".to_string());
            match entry_dict.get_mut(&env_key) {
                Some(Value::Dict(env)) => {
                    for value in env.values_mut() {
                        *value = Value::Str(REDACTED.to_string());
                    }
                }
                // Not a mapping (a config error preflight rejects, but the
                // snapshot is written first): replaced whole.
                Some(other) => *other = Value::Str(REDACTED.to_string()),
                None => {}
            }
        }
        out.insert(name, redact(entry));
    }
    Value::Dict(out)
}

/// `cli/redaction.py::redact` -- recursively redacts credential-like
/// fields: a dict value is wholesale-redacted when its own key matches
/// [`is_sensitive_key`]; every value of an `env` mapping under an
/// `adapters` key is redacted ([`redact_adapter_env`]); a string value
/// under a URL key ([`is_url_key`]) redacts wholesale when it's
/// token-shaped ([`is_token_shaped`], `main`'s own ordinary-string
/// rule applied first), else gets the URL rule ([`redact_url`]); any
/// other string is redacted (wholesale, or just its URL userinfo) when
/// it looks like a JWT, an API key, or a URL carrying both `://` and
/// `@`; lists and nested dicts are walked; every other scalar passes
/// through unchanged. Returns a new [`Value`]; never mutates in place
/// (there is nothing to mutate -- `Value` has no shared interior
/// mutability of its own).
pub fn redact(mut value: Value) -> Value {
    // `Value::Dict(dict) = value` would move `dict`'s `IndexMap` out of
    // a type that implements `Drop` (electricity-value's iterative
    // `Drop` impl), which Rust never allows -- `mem::take` swaps the
    // container out instead, leaving `value` an (unused) empty
    // placeholder behind (`electricity-template/src/render.rs`'s own
    // identical comment, same root cause).
    match &mut value {
        Value::Dict(dict) => {
            let taken = std::mem::take(dict);
            let mut out = Dict::with_capacity(taken.len());
            for (key, mut inner) in taken {
                let is_sensitive = matches!(&key, Value::Str(s) if is_sensitive_key(s));
                let is_url = matches!(&key, Value::Str(s) if is_url_key(s));
                let redacted_inner = if is_sensitive {
                    Value::Str(REDACTED.to_string())
                } else if matches!(&key, Value::Str(s) if s == "adapters") {
                    redact_adapter_env(inner)
                } else if is_url {
                    if let Value::Str(s) = &mut inner {
                        if is_token_shaped(s) {
                            Value::Str(REDACTED.to_string())
                        } else {
                            Value::Str(redact_url(s, REDACTED))
                        }
                    } else {
                        redact(inner)
                    }
                } else {
                    redact(inner)
                };
                out.insert(key, redacted_inner);
            }
            Value::Dict(out)
        }
        Value::List(items) => Value::List(std::mem::take(items).into_iter().map(redact).collect()),
        Value::Str(s) => Value::Str(redact_string(std::mem::take(s).as_str())),
        _ => value,
    }
}

/// `cli/redaction.py::redact_env_pairs` -- a list of `KEY=VALUE` strings
/// (the `-e` flag shape, and `cli/last_run.py`'s own stashed `env_vars`):
/// the key is kept as given (so a replay can show what was supplied), the
/// value is replaced with [`REDACTED`] when the key matches
/// [`is_sensitive_key`]; under a URL key ([`is_url_key`]), a
/// token-shaped value redacts wholesale ([`is_token_shaped`], `main`'s
/// own ordinary-string rule applied first), else it gets the URL rule
/// ([`redact_url`]); any other value is redacted at the string level
/// ([`redact_string`]) the same way any other string value is -- so a
/// credential-shaped value under an innocuous key (`my_var=sk-...`) is
/// still caught. An entry with no `=` passes through unchanged, same as
/// the reference's own `"=" not in pair` branch.
pub fn redact_env_pairs(pairs: &[String]) -> Vec<String> {
    pairs
        .iter()
        .map(|pair| match pair.split_once('=') {
            None => pair.clone(),
            Some((key, value)) => {
                if is_sensitive_key(key) {
                    format!("{key}={REDACTED}")
                } else if is_url_key(key) {
                    if is_token_shaped(value) {
                        format!("{key}={REDACTED}")
                    } else {
                        format!("{key}={}", redact_url(value, REDACTED))
                    }
                } else {
                    format!("{key}={}", redact_string(value))
                }
            }
        })
        .collect()
}

/// `core/tool.py::_capped_raw`'s own truncation-marker shape: `{
/// "_truncated": true, "_original_bytes": N, "_preview": "..." }`,
/// substituted for `meta.raw` wholesale once its redacted, JSON-encoded
/// form exceeds [`RAW_META_MAX_BYTES`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCapMarker {
    pub original_bytes: usize,
    /// *encoded_redacted_raw*'s own first [`RAW_META_MAX_BYTES`] bytes,
    /// lossily decoded as UTF-8 (`bytes.decode("utf-8", errors="ignore")`
    /// -- a multi-byte codepoint split by the cut keeps only its valid
    /// prefix, dropping the rest, exactly as Python's `errors="ignore"`
    /// does for a trailing partial sequence).
    pub preview: String,
}

/// `len(encoded) <= _RAW_META_MAX_BYTES`'s own two-way branch: `None`
/// when *encoded_redacted_raw* (a tool's already-[`redact`]ed `raw`,
/// JSON-encoded -- `json.dumps(redacted, ensure_ascii=False,
/// default=str).encode("utf-8")`) already fits; `Some` with the
/// [`RawCapMarker`] a caller substitutes for the whole `meta.raw` value
/// otherwise.
pub fn cap_raw(encoded_redacted_raw: &[u8]) -> Option<RawCapMarker> {
    if encoded_redacted_raw.len() <= RAW_META_MAX_BYTES {
        return None;
    }
    let truncated = &encoded_redacted_raw[..RAW_META_MAX_BYTES];
    // `bytes.decode("utf-8", errors="ignore")` drops an invalid byte
    // sequence rather than substituting U+FFFD for it (what
    // `from_utf8_lossy` would do) -- *encoded_redacted_raw* is always
    // valid UTF-8 before this cut (`json.dumps`'s own output always is),
    // so the only invalid sequence a byte-offset cut can produce is one
    // partial multi-byte codepoint right at the very end; keeping only
    // the longest valid-UTF-8 prefix reproduces that exactly.
    let preview = match std::str::from_utf8(truncated) {
        Ok(s) => s.to_string(),
        Err(err) => std::str::from_utf8(&truncated[..err.valid_up_to()])
            .expect("valid_up_to() bounds a valid UTF-8 prefix")
            .to_string(),
    };
    Some(RawCapMarker {
        original_bytes: encoded_redacted_raw.len(),
        preview,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(pairs: Vec<(&str, Value)>) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(d)
    }

    #[test]
    fn redact_env_pairs_redacts_sensitive_keys_and_keyish_values() {
        let pairs = vec![
            "name=World".to_string(),
            "OPENAI_API_KEY=sk-test-1234567890abcdef1234567890abcdef".to_string(),
            "auth_token=abc.def.ghi".to_string(),
            "password=hunter2hunter2hunter2hunter2hunter2".to_string(),
            "topic=cats".to_string(),
            "MY_TOKEN=plain".to_string(),
            "shaped_value=sk-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ];
        let redacted = redact_env_pairs(&pairs);
        assert!(redacted.contains(&"name=World".to_string()));
        assert!(redacted.contains(&"topic=cats".to_string()));
        assert!(redacted.contains(&format!("OPENAI_API_KEY={REDACTED}")));
        assert!(redacted.contains(&format!("auth_token={REDACTED}")));
        assert!(redacted.contains(&format!("password={REDACTED}")));
        // Suffix-matched secret name.
        assert!(redacted.contains(&format!("MY_TOKEN={REDACTED}")));
        // Value shaped like an API key, redacted even though the key isn't sensitive.
        assert!(redacted.contains(&format!("shaped_value={REDACTED}")));
    }

    #[test]
    fn redact_env_pairs_passes_a_pair_with_no_equals_through() {
        assert_eq!(redact_env_pairs(&[]), Vec::<String>::new());
        assert_eq!(
            redact_env_pairs(&["badtext".to_string()]),
            vec!["badtext".to_string()]
        );
    }

    #[test]
    fn every_env_value_under_adapters_redacts_and_the_names_stay() {
        let env = dict(vec![
            ("CLAUDE_CONFIG_DIR", Value::from("~/.claude-alt")),
            ("OTHER", Value::from("plain")),
        ]);
        let entry = dict(vec![
            ("adapter", Value::from("claude_code")),
            ("env", env),
            ("binary", Value::from("claude")),
        ]);
        let input = dict(vec![("adapters", dict(vec![("claude_alt", entry)]))]);
        let expected_entry = dict(vec![
            ("adapter", Value::from("claude_code")),
            (
                "env",
                dict(vec![
                    ("CLAUDE_CONFIG_DIR", Value::from(REDACTED)),
                    ("OTHER", Value::from(REDACTED)),
                ]),
            ),
            ("binary", Value::from("claude")),
        ]);
        let expected = dict(vec![(
            "adapters",
            dict(vec![("claude_alt", expected_entry)]),
        )]);
        assert_eq!(redact(input), expected);
    }

    #[test]
    fn an_env_that_is_not_a_mapping_is_replaced_whole() {
        for bad in [
            Value::from("A=secret"),
            Value::List(vec![Value::from("A=secret")]),
            Value::from(5i64),
        ] {
            let input = dict(vec![(
                "adapters",
                dict(vec![(
                    "alt",
                    dict(vec![("adapter", Value::from("pi")), ("env", bad)]),
                )]),
            )]);
            let expected = dict(vec![(
                "adapters",
                dict(vec![(
                    "alt",
                    dict(vec![
                        ("adapter", Value::from("pi")),
                        ("env", Value::from(REDACTED)),
                    ]),
                )]),
            )]);
            assert_eq!(redact(input), expected);
        }
    }

    #[test]
    fn an_adapters_list_is_redacted_like_any_other_list() {
        let input = dict(vec![(
            "adapters",
            Value::List(vec![dict(vec![("api_key", Value::from("x"))])]),
        )]);
        let expected = dict(vec![(
            "adapters",
            Value::List(vec![dict(vec![("api_key", Value::from(REDACTED))])]),
        )]);
        assert_eq!(redact(input), expected);
    }

    #[test]
    fn a_sensitive_key_redacts_its_value_regardless_of_shape() {
        let input = dict(vec![("api_key", Value::from("anything-at-all"))]);
        let expected = dict(vec![("api_key", Value::from(REDACTED))]);
        assert_eq!(redact(input), expected);
    }

    #[test]
    fn a_sensitive_key_match_is_case_insensitive_and_suffix_anchored() {
        let input = dict(vec![("MY-API-KEY", Value::from("x"))]);
        let expected = dict(vec![("MY-API-KEY", Value::from(REDACTED))]);
        assert_eq!(redact(input), expected);
    }

    #[test]
    fn an_unrelated_key_is_left_alone() {
        let input = dict(vec![("name", Value::from("not a secret"))]);
        assert_eq!(redact(input.clone()), input);
    }

    #[test]
    fn a_jwt_shaped_string_redacts_wholesale() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        assert_eq!(redact(Value::from(jwt)), Value::from(REDACTED));
    }

    #[test]
    fn an_sk_shaped_string_redacts_wholesale() {
        let key = "sk-abcdefghijklmnopqrstuvwxyz123456";
        assert_eq!(redact(Value::from(key)), Value::from(REDACTED));
    }

    #[test]
    fn a_bearer_token_string_redacts_wholesale() {
        let header = "Bearer abcdefghijklmnopqrstuvwxyz";
        assert_eq!(redact(Value::from(header)), Value::from(REDACTED));
    }

    #[test]
    fn a_url_with_userinfo_redacts_only_the_userinfo() {
        let url = "https://user:pass@example.com:8080/path?q=1#frag";
        assert_eq!(
            redact(Value::from(url)),
            Value::from("https://***REDACTED***@example.com:8080/path?q=1#frag")
        );
    }

    #[test]
    fn a_trailing_newline_does_not_hide_an_sk_shaped_secret() {
        // `redact("sk-" + "a" * 20 + "\n")` -- Python's `$` (no
        // `re.MULTILINE`) matches just before a single trailing `\n`.
        let key = format!("sk-{}\n", "a".repeat(20));
        assert_eq!(redact(Value::from(key.as_str())), Value::from(REDACTED));
    }

    #[test]
    fn a_trailing_newline_does_not_hide_a_bearer_token() {
        let header = format!("Bearer {}\n", "a".repeat(16));
        assert_eq!(redact(Value::from(header.as_str())), Value::from(REDACTED));
    }

    #[test]
    fn a_trailing_newline_does_not_hide_a_jwt() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U\n";
        assert_eq!(redact(Value::from(jwt)), Value::from(REDACTED));
    }

    #[test]
    fn a_trailing_newline_does_not_hide_a_sensitive_key() {
        let input = dict(vec![("api_key\n", Value::from("x"))]);
        let expected = dict(vec![("api_key\n", Value::from(REDACTED))]);
        assert_eq!(redact(input), expected);
    }

    #[test]
    fn two_trailing_newlines_do_not_match_the_single_newline_allowance() {
        let key = format!("sk-{}\n\n", "a".repeat(20));
        assert_eq!(redact(Value::from(key.as_str())), Value::from(key.as_str()));
    }

    #[test]
    fn a_url_with_no_userinfo_passes_through() {
        let url = "https://example.com/path";
        assert_eq!(redact(Value::from(url)), Value::from(url));
    }

    #[test]
    fn a_schemeless_base_url_without_any_double_slash_still_redacts() {
        // #819: a base_url with no :// at all (the same shape as a
        // schemeless proxy value) still has its credential cut. Only
        // a string under a URL key gets this rule (issue #819 follow-up).
        let url = "alice:hunter2hunter2@host:1234";
        let input = dict(vec![("base_url", Value::from(url))]);
        let expected = dict(vec![(
            "base_url",
            Value::from(format!("{REDACTED}@host:1234")),
        )]);
        assert_eq!(redact(input), expected);
    }

    #[test]
    fn a_credential_hidden_behind_the_path_still_redacts() {
        // #819: an @ past the parsed authority's own boundary still
        // hides a password when the text before it couldn't be a
        // port -- the whole value is cut at the LAST @.
        let url = "https://alice:longpw/x@api.example.test/v1";
        let input = dict(vec![("base_url", Value::from(url))]);
        let expected = dict(vec![(
            "base_url",
            Value::from(format!("https://{REDACTED}@api.example.test/v1")),
        )]);
        assert_eq!(redact(input), expected);
    }

    #[test]
    fn an_at_sign_past_a_real_host_port_is_left_alone() {
        // #819 follow-up: "443" is a valid port, so "alice:443" reads
        // as a real host:port, not a hidden credential -- the later @
        // is left untouched, the same as `http://localhost:8080/@alice`.
        let url = "https://alice:443/x@api.example.test/v1";
        let input = dict(vec![("base_url", Value::from(url))]);
        let expected = dict(vec![("base_url", Value::from(url))]);
        assert_eq!(redact(input), expected);
    }

    #[test]
    fn a_credential_like_query_param_with_no_userinfo_is_masked() {
        // #819: a base_url with a credential-like query key but no @ at
        // all was never masked before. The data redactor's own marker is
        // used for a query value too, not the error-text `%2A%2A%2A`.
        let url = "https://api.example.test/search?api_key=hunter2hunter2";
        let input = dict(vec![("base_url", Value::from(url))]);
        let expected = dict(vec![(
            "base_url",
            Value::from(format!(
                "https://api.example.test/search?api_key={REDACTED}"
            )),
        )]);
        assert_eq!(redact(input), expected);
    }

    #[test]
    fn an_email_address_is_not_mangled() {
        // An @ with no colon before it (an email address, not a
        // user:pass@host credential) is left alone.
        let value = "alice@example.test";
        assert_eq!(redact(Value::from(value)), Value::from(value));
    }

    #[test]
    fn a_sentence_containing_a_question_mark_link_is_not_mangled() {
        // Prose that merely contains a URL (with whitespace around it)
        // is never treated as a single URL value: it must not be
        // re-encoded as a query string.
        let value = "<a href=\"https://x.test/?a=1\">x</a> more";
        assert_eq!(redact(Value::from(value)), Value::from(value));
    }

    #[test]
    fn a_multi_line_body_containing_a_question_mark_is_not_mangled() {
        let value = "line one\nis this a question?\nline three";
        assert_eq!(redact(Value::from(value)), Value::from(value));
    }

    #[test]
    fn a_vertex_style_model_id_is_not_mangled() {
        // A model id with an @ and no colon before it (e.g. a pinned
        // Vertex model version) is not a user:pass@host credential.
        let value = "claude-3-5-sonnet@20240620";
        assert_eq!(redact(Value::from(value)), Value::from(value));
    }

    #[test]
    fn a_list_of_email_addresses_is_not_mangled() {
        let input = dict(vec![(
            "to",
            Value::List(vec![
                Value::from("alice@example.test"),
                Value::from("bob@example.test"),
            ]),
        )]);
        assert_eq!(redact(input.clone()), input);
    }

    #[test]
    fn a_question_mark_inside_the_fragment_is_never_treated_as_a_query() {
        // `urlsplit` finds the fragment (the first #) before it looks
        // for a query -- a ? that comes after it is just fragment
        // text, never a query delimiter, so it must never be
        // re-encoded as a masked query.
        let value = "https://host/path#frag?a=1";
        assert_eq!(redact(Value::from(value)), Value::from(value));
    }

    #[test]
    fn redact_url_strips_leading_whitespace_like_urlsplit() {
        let url = " https://u:p@h";
        assert_eq!(
            redact(Value::from(url)),
            Value::from("https://***REDACTED***@h")
        );
    }

    #[test]
    fn redact_url_drops_an_embedded_tab_like_urlsplit() {
        let url = "ht\ttps://u:p@h";
        assert_eq!(
            redact(Value::from(url)),
            Value::from("https://***REDACTED***@h")
        );
    }

    #[test]
    fn redact_url_lowercases_the_scheme_like_urlsplit() {
        let url = "HTTPS://u:p@h";
        assert_eq!(
            redact(Value::from(url)),
            Value::from("https://***REDACTED***@h")
        );
    }

    #[test]
    fn redact_url_drops_an_empty_query_like_urlunsplit() {
        let url = "https://u:p@h/p?";
        assert_eq!(
            redact(Value::from(url)),
            Value::from("https://***REDACTED***@h/p")
        );
    }

    #[test]
    fn redact_url_keeps_an_empty_query_dropped_but_fragment_present() {
        let url = "https://u:p@h/p?#f";
        assert_eq!(
            redact(Value::from(url)),
            Value::from("https://***REDACTED***@h/p#f")
        );
    }

    #[test]
    fn redact_url_leaves_an_unbalanced_bracket_unchanged() {
        // `urlsplit` raises `ValueError` on a netloc with an unmatched
        // `[`/`]` ("Invalid IPv6 URL"); `_redact_url` catches that and
        // returns the value unchanged.
        let url = "https://u:p@h[invalid";
        assert_eq!(redact(Value::from(url)), Value::from(url));
    }

    #[test]
    fn a_plain_dotted_string_is_not_mistaken_for_a_jwt() {
        let value = "www.example.com";
        assert_eq!(redact(Value::from(value)), Value::from(value));
    }

    #[test]
    fn nested_dicts_and_lists_are_walked() {
        let input = dict(vec![(
            "outer",
            Value::List(vec![dict(vec![("password", Value::from("x"))])]),
        )]);
        let expected = dict(vec![(
            "outer",
            Value::List(vec![dict(vec![("password", Value::from(REDACTED))])]),
        )]);
        assert_eq!(redact(input), expected);
    }

    #[test]
    fn a_non_string_key_is_never_treated_as_sensitive() {
        let mut input = Dict::new();
        input.insert(
            Value::Bool(true),
            Value::from("sk-abcdefghijklmnopqrstuvwxyz123456"),
        );
        let mut expected = Dict::new();
        expected.insert(Value::Bool(true), Value::from(REDACTED));
        assert_eq!(redact(Value::Dict(input)), Value::Dict(expected));
    }

    #[test]
    fn redact_url_leaves_a_credential_free_url_byte_for_byte() {
        for url in [
            "https://medium.com/@alice/post",
            "https://x.test/?q=a@b",
            "https://host/p?q=a/b",
            "https://host/p?q=a%20b",
            "https://host/p?flag",
            "https://host/p?%7e",
            "HTTPS://host/p",
            "https://host/p?",
            "https://host/p?&",
            "https://host/path#frag?a=1",
        ] {
            assert_eq!(redact_url(url, REDACTED), url);
            assert_eq!(redact_url(url, ""), url);
        }
    }

    #[test]
    fn redact_url_cuts_a_password_hidden_behind_a_slash_but_not_a_bare_at_sign() {
        let url = "http://alice:longsecret/x@127.0.0.1";
        assert_eq!(
            redact_url(url, REDACTED),
            format!("http://{REDACTED}@127.0.0.1")
        );
        assert_eq!(redact_url(url, ""), "http://127.0.0.1");
    }

    #[test]
    fn redact_url_cuts_a_schemeless_userinfo_value() {
        assert_eq!(
            redact_url("alice:longsecret@host:1234", REDACTED),
            format!("{REDACTED}@host:1234")
        );
    }

    #[test]
    fn redact_url_cuts_a_schemeless_token_with_no_colon_at_all() {
        // A bare token@host with no `:` is still a credential once
        // *url* is already known to be a URL/proxy value -- only
        // whitespace, never the `:` shape, gates the schemeless cut.
        let url = "mytoken@host.example.test/api";
        assert_eq!(
            redact_url(url, REDACTED),
            format!("{REDACTED}@host.example.test/api")
        );
        assert_eq!(redact_url(url, ""), "host.example.test/api");
    }

    #[test]
    fn redact_url_cuts_a_schemeless_value_even_with_trailing_whitespace() {
        // #819 follow-up: every caller already knows *url* is a URL --
        // whitespace is never an exception to the schemeless cut.
        assert_eq!(redact_url("alice:s3cret@proxy:3128 ", ""), "proxy:3128 ");
    }

    #[test]
    fn redact_url_masks_a_credential_query_key_among_other_keys() {
        let url = "https://host/x?foo=1&api_key=sk-test&bar=2";
        assert_eq!(
            redact_url(url, REDACTED),
            format!("https://host/x?foo=1&api_key={REDACTED}&bar=2")
        );
        assert_eq!(
            redact_url(url, ""),
            "https://host/x?foo=1&api_key=%2A%2A%2A&bar=2"
        );
    }

    #[test]
    fn redact_url_cuts_authority_userinfo_even_when_a_later_at_sign_has_no_colon() {
        // #819 follow-up: the authority's own userinfo is cut first,
        // whether or not another @ also appears later in the value
        // (a scoped-package path here).
        let url = "https://npmTOKEN123@registry.example.test/@acme/widget";
        assert_eq!(
            redact_url(url, REDACTED),
            format!("https://{REDACTED}@registry.example.test/@acme/widget")
        );
        assert_eq!(
            redact_url(url, ""),
            "https://registry.example.test/@acme/widget"
        );
    }

    #[test]
    fn redact_url_masks_a_credential_query_key_matched_in_decoded_form() {
        // #819 follow-up: the key is matched percent-decoded
        // ("token"), but only the value's raw text is replaced.
        let url = "https://host/x?tok%65n=abcd1234";
        assert_eq!(
            redact_url(url, REDACTED),
            format!("https://host/x?tok%65n={REDACTED}")
        );
    }

    #[test]
    fn a_token_shaped_value_under_a_url_key_redacts_whole() {
        // #819 follow-up: `main`'s own ordinary-string rule (the JWT/
        // key-shape patterns) applies before the URL rule, so a
        // token-shaped value with no @ and no ? under a URL key still
        // redacts wholesale, not byte for byte.
        let key = format!("sk-{}", "a".repeat(20));
        let input = dict(vec![("endpoint", Value::from(key.as_str()))]);
        let expected = dict(vec![("endpoint", Value::from(REDACTED))]);
        assert_eq!(redact(input), expected);
    }

    #[test]
    fn redact_env_pairs_applies_the_token_shape_check_before_the_url_rule() {
        let key = format!("xoxb-{}", "a".repeat(20));
        let pairs = vec![format!("webhook_url={key}")];
        assert_eq!(
            redact_env_pairs(&pairs),
            vec![format!("webhook_url={REDACTED}")]
        );
    }

    #[test]
    fn scalars_other_than_strings_pass_through_unchanged() {
        assert_eq!(redact(Value::from(1i64)), Value::from(1i64));
        assert_eq!(redact(Value::Bool(true)), Value::Bool(true));
        assert_eq!(redact(Value::None), Value::None);
    }

    #[test]
    fn cap_raw_is_none_at_or_under_the_limit() {
        let encoded = vec![b'a'; RAW_META_MAX_BYTES];
        assert_eq!(cap_raw(&encoded), None);
    }

    #[test]
    fn cap_raw_substitutes_a_marker_once_over_the_limit() {
        let encoded = vec![b'a'; RAW_META_MAX_BYTES + 1];
        let marker = cap_raw(&encoded).unwrap();
        assert_eq!(marker.original_bytes, RAW_META_MAX_BYTES + 1);
        assert_eq!(marker.preview.len(), RAW_META_MAX_BYTES);
    }

    #[test]
    fn cap_raw_drops_a_codepoint_split_by_the_cut_instead_of_substituting_u_fffd() {
        // `_capped_raw({"k": "\u00e9" * 40000})["_preview"][-3:]` has no
        // `\ufffd` -- Python's `errors="ignore"` drops a trailing partial
        // multi-byte sequence rather than substituting the replacement
        // character. Here: `RAW_META_MAX_BYTES` bytes of all-`a` except
        // the very last one is the lead byte of a 2-byte UTF-8 sequence
        // (`\u00e9`) whose second byte falls just past the cut.
        let mut encoded = vec![b'a'; RAW_META_MAX_BYTES + 1];
        encoded[RAW_META_MAX_BYTES - 1] = 0xC3;
        encoded[RAW_META_MAX_BYTES] = 0xA9;
        let marker = cap_raw(&encoded).unwrap();
        assert!(!marker.preview.contains('\u{fffd}'));
        assert_eq!(marker.preview.len(), RAW_META_MAX_BYTES - 1);
    }
}
