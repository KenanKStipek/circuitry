//! electricity-redaction: credential-shaped-value redaction
//! (`cli/redaction.py::redact`, ported word for word), and the
//! `meta.raw` size cap (issue #431's Scope section, lane C).
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

/// `cli/redaction.py::_redact_url` -- strips `user[:pass]@` userinfo
/// from a URL's netloc, keeping the scheme, host:port, path, query and
/// fragment untouched. Ports the subset of CPython 3.11's
/// `urllib.parse.urlsplit`/`urlunsplit` that `_redact_url` itself
/// depends on (leading-control/space stripping, embedded `\t`/`\r`/`\n`
/// removal, lowercase scheme, and `urlunsplit`'s own empty-`?`/`#`
/// rules) rather than a full `url`-crate split: this is only ever
/// reached from [`redact_string`]'s own `"://" in value and "@" in
/// value` gate, so it only has to recognize the one shape that gate
/// already promises.
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
fn redact_url(value: &str) -> String {
    // `url.lstrip(_WHATWG_C0_CONTROL_OR_SPACE)` -- C0 controls (0x00-0x1f)
    // and space (0x20).
    let lstripped = value.trim_start_matches(|c: char| (c as u32) <= 0x20);
    // `for b in ("\t", "\r", "\n"): url = url.replace(b, "")` -- applied
    // to the whole (already-lstripped) string, not just its prefix.
    let cleaned: String = lstripped
        .chars()
        .filter(|&c| c != '\t' && c != '\r' && c != '\n')
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
    // `scheme, url = url[:i].lower(), url[i+1:]`
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

    // `('[' in netloc and ']' not in netloc) or (']' in netloc and '[' not
    // in netloc)` -- `urlsplit` raises `ValueError` here, which
    // `_redact_url` catches by returning *value* unchanged.
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

    // `urlunsplit`: the redacted netloc is always non-empty, so the path
    // always gets a leading `//<netloc>`; an empty path stays empty
    // (never gains a bare `/`); an empty query/fragment is dropped
    // entirely (`https://u:p@h/p?` -> `.../p`, not `.../p?`).
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

/// `cli/redaction.py::_redact_string` -- a JWT or API-key-shaped string
/// redacts wholesale; a URL carrying userinfo redacts just that part
/// ([`redact_url`]); anything else passes through unchanged.
fn redact_string(value: &str) -> String {
    if jwt_re().is_match(value) {
        return REDACTED.to_string();
    }
    if keyish_re().is_match(value) {
        return REDACTED.to_string();
    }
    if value.contains("://") && value.contains('@') {
        return redact_url(value);
    }
    value.to_string()
}

/// `cli/redaction.py::redact` -- recursively redacts credential-like
/// fields: a dict value is wholesale-redacted when its own key matches
/// [`is_sensitive_key`]; a string is redacted (wholesale, or just its
/// URL userinfo) when it looks like a JWT, an API key, or a URL with
/// userinfo; lists and nested dicts are walked; every other scalar
/// passes through unchanged. Returns a new [`Value`]; never mutates in
/// place (there is nothing to mutate -- `Value` has no shared interior
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
            for (key, inner) in taken {
                let is_sensitive = matches!(&key, Value::Str(s) if is_sensitive_key(s));
                let redacted_inner = if is_sensitive {
                    Value::Str(REDACTED.to_string())
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
