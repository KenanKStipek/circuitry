//! `circuitry.http_conventions`, ported -- issue #778 step 7: strict port
//! parsing and the http(s)-only scheme check, shared by the `http` tool,
//! every adapter's `base_url` ([`crate::http_client::proxy`], and
//! `electricity-adapters::transport`, which depends on this crate), and a
//! proxy URL. Both engines refuse the same input with the same text, so
//! this is never third-party text -- it is Circuitry's own, compared
//! exactly.

/// `curl_support.py::_URL_USERINFO_RE` -- `://user[:pass]@` becomes
/// `://`, wherever it occurs in *text* (hint/prose text included, not
/// just a bare URL), for a refusal message -- never the raw value,
/// which can carry a credential. Shared with `electricity-adapters::
/// curl_text`, which re-exports this rather than keeping a second copy.
pub fn strip_url_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(scheme_idx) = rest.find("://") else {
            out.push_str(rest);
            break;
        };
        let after_scheme = &rest[scheme_idx + 3..];
        // The authority ends at the first `/` or whitespace; a password
        // inside it can itself contain `@`, so the userinfo is everything
        // up to the *last* `@` in that span, not the first one (issue
        // #778) -- stopping at the first would
        // leave a fragment of the password (e.g. `ss@proxy` out of
        // `alice:p@ss@proxy`) in the stripped text.
        let boundary = after_scheme
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(after_scheme.len());
        let authority = &after_scheme[..boundary];
        out.push_str(&rest[..scheme_idx + 3]);
        match authority.rfind('@') {
            Some(last_at) => rest = &after_scheme[last_at + 1..],
            None => rest = after_scheme,
        }
    }
    out
}

/// `curl_support.py::_CREDENTIAL_QUERY_KEY_RE` -- a case-insensitive
/// *search* (not a full match) for any of these words anywhere in a
/// query key.
fn looks_like_credential_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    ["key", "token", "secret", "auth", "password", "pwd"]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// `urllib.parse.parse_qsl(keep_blank_values=True)` -- `&`-separated
/// `name=value` pairs (a bare `name` with no `=` keeps an empty
/// value), each percent-decoded with `+` read as a space (the
/// `application/x-www-form-urlencoded` convention `parse_qsl` uses by
/// default, `plus` left `True`).
fn parse_qsl(query: &str) -> Vec<(String, String)> {
    if query.is_empty() {
        return Vec::new();
    }
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (percent_decode_plus(k), percent_decode_plus(v)),
            None => (percent_decode_plus(pair), String::new()),
        })
        .collect()
}

fn percent_decode_plus(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `curl_support.py::_mask_credential_query_params` -- masks the
/// value of any query-string key matching [`looks_like_credential_key`],
/// then re-encodes the **whole** query with
/// [`electricity_value::pycompat::urlencode`] even when nothing
/// matched (so `%2A%2A%2A` for the mask, a space becomes `+`, exactly
/// mirrors `urllib.parse.urlencode`'s own default `quote_plus`
/// encoding).
pub fn mask_credential_query_params(url: &str) -> String {
    let Some(query_start) = url.find('?') else {
        return url.to_string();
    };
    let (before_query, rest) = url.split_at(query_start);
    let query_and_fragment = &rest[1..];
    let (query, fragment) = match query_and_fragment.find('#') {
        Some(idx) => (&query_and_fragment[..idx], Some(&query_and_fragment[idx..])),
        None => (query_and_fragment, None),
    };
    let pairs = parse_qsl(query);
    let masked: Vec<(String, electricity_value::Value)> = pairs
        .into_iter()
        .map(|(k, v)| {
            let value = if looks_like_credential_key(&k) {
                "***".to_string()
            } else {
                v
            };
            (k, electricity_value::Value::Str(value))
        })
        .collect();
    let new_query = electricity_value::pycompat::urlencode(&masked, false).unwrap_or_default();
    let mut result = format!("{before_query}?{new_query}");
    if let Some(fragment) = fragment {
        result.push_str(fragment);
    }
    result
}

/// `http_conventions._strip_request_url_userinfo` -- the LAST `@`
/// inside [`crate::http::http_url::urlsplit`]'s own `netloc`, not a
/// `/`, `?`, `#` or whitespace boundary the way [`strip_url_userinfo`]
/// (a free-text scrub) needs to stay inside: this always receives one
/// whole URL, never prose with one embedded somewhere, so building the
/// redaction from the parsed authority directly -- instead of a regex-
/// style heuristic -- means a password containing any ASCII control
/// character or whitespace (none of which end `urlsplit`'s own
/// `netloc` the way `/` does) still comes out fully removed (issue
/// #778).
fn strip_request_url_userinfo(url: &str) -> String {
    let parsed = crate::http::http_url::urlsplit(url, "");
    let Some(at) = parsed.netloc.rfind('@') else {
        return url.to_string();
    };
    let netloc = parsed.netloc[at + 1..].to_string();
    crate::http::http_url::urlunsplit(&crate::http::http_url::Split { netloc, ..parsed })
}

/// `curl_support.py::redact_host_url`/`http_conventions.redact_url` --
/// the two masks every refusal message that names a request URL
/// (never a proxy value, see [`redact_proxy_value`]) applies before
/// naming it: userinfo stripped, then any credential-like query
/// parameter masked.
pub fn redact_url(url: &str) -> String {
    strip_request_url_userinfo(&mask_credential_query_params(url))
}

/// `http_conventions.redact_proxy_value` -- a proxy value has no
/// meaningful path, so (unlike [`redact_url`]) this never stops at a
/// `/`, `?`, `#` or whitespace boundary: only the LAST `@` in the
/// *whole* value (after its `scheme://`, when it has one) counts, so a
/// password containing any of those still comes out fully removed
/// (issue #778).
pub fn redact_proxy_value(value: &str) -> String {
    let (prefix, tail) = match value.find("://") {
        Some(idx) => (&value[..idx + 3], &value[idx + 3..]),
        None => ("", value),
    };
    match tail.rfind('@') {
        Some(at) => format!("{prefix}{}", &tail[at + 1..]),
        None => value.to_string(),
    }
}

/// `http_conventions._proxy_host_port` -- the host[:port] text of
/// *normalized* (a proxy value that already has a `scheme://`): the
/// whole remainder after it, split at the LAST `@` -- never bounded by
/// a `/`, `?` or `#` the way [`strict_port`]'s own `urlsplit`-based
/// netloc is, since a proxy value has no meaningful path and its
/// password may itself contain any of those (issue #778). The
/// returned text is always a suffix of *normalized* itself.
pub(crate) fn proxy_host_port(normalized: &str) -> &str {
    let after_scheme = normalized
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(normalized);
    match after_scheme.rfind('@') {
        Some(at) => &after_scheme[at + 1..],
        None => after_scheme,
    }
}

/// The raw port text in *host_port* (userinfo already stripped by the
/// caller): `None` with no `:` separator at all (or none after an IPv6
/// literal's own `]`), otherwise the (possibly empty) text after it.
pub(crate) fn host_port_text(host_port: &str) -> Option<&str> {
    let rest = if let Some(after_bracket) = host_port.strip_prefix('[') {
        match after_bracket.find(']') {
            Some(end) => &after_bracket[end + 1..],
            None => "",
        }
    } else {
        let colon = host_port.find(':')?;
        &host_port[colon..]
    };
    if rest.is_empty() {
        return None;
    }
    // After an IPv6 literal's `]` comes `:` and a port, or nothing at all
    // -- anything else (`[::1]x`) is *not* "no port", it is a port
    // text that will never pass the digit check below, so it still ends
    // up refused, just with the usual "invalid port" text rather than a
    // distinct message of its own (issue #778).
    Some(rest.strip_prefix(':').unwrap_or(rest))
}

/// The strict port in *netloc* (an authority string, userinfo
/// included) -- `Ok(None)` for an absent or empty port (the scheme's own
/// default applies); `Err` with Circuitry's own refusal text (naming
/// *original_url* with any userinfo removed) for anything else: after the
/// host (or after `]` for IPv6) comes `:` and one or more ASCII digits,
/// 1-65535.
fn port_in_range(port_text: &str) -> Option<u16> {
    if !port_text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let value: u32 = port_text.parse().ok()?;
    (1..=65535).contains(&value).then_some(value as u16)
}

pub fn strict_port(netloc: &str, original_url: &str) -> Result<Option<u16>, String> {
    let host_port = match netloc.rfind('@') {
        Some(at) => &netloc[at + 1..],
        None => netloc,
    };
    let Some(port_text) = host_port_text(host_port) else {
        return Ok(None);
    };
    if port_text.is_empty() {
        return Ok(None);
    }
    if let Some(value) = port_in_range(port_text) {
        return Ok(Some(value));
    }
    Err(format!(
        "invalid port in URL '{}': '{}'",
        redact_url(original_url),
        port_text
    ))
}

/// Like [`strict_port`], but for a proxy value: its host[:port] text
/// comes from [`proxy_host_port`], not `urlsplit`'s own netloc, and any
/// refusal names *normalized* through [`redact_proxy_value`] (issue
/// #778).
pub(crate) fn strict_port_for_proxy(normalized: &str) -> Result<Option<u16>, String> {
    let host_port = proxy_host_port(normalized);
    let Some(port_text) = host_port_text(host_port) else {
        return Ok(None);
    };
    if port_text.is_empty() {
        return Ok(None);
    }
    if let Some(value) = port_in_range(port_text) {
        return Ok(Some(value));
    }
    Err(format!(
        "invalid port in URL '{}': '{}'",
        redact_proxy_value(normalized),
        port_text
    ))
}

/// Any ASCII control character (0x00-0x1F, 0x7F) or ASCII whitespace
/// (space is 0x20, already inside that first range) -- a URL or proxy
/// value containing one is refused before any I/O, on both engines,
/// with one shared message (issue #778).
pub(crate) fn check_no_control_or_whitespace(
    original_url: &str,
    redact: fn(&str) -> String,
) -> Result<(), String> {
    if original_url
        .chars()
        .any(|c| matches!(c, '\u{0}'..='\u{20}' | '\u{7f}'))
    {
        return Err(format!(
            "URL contains a control character or whitespace: '{}'",
            redact(original_url)
        ));
    }
    Ok(())
}

/// *scheme* must be one of *allowed* (case-insensitively), or
/// *original_url* is refused with Circuitry's own text, userinfo
/// removed. Checked first (issue #778, [`check_no_control_or_whitespace`]): every caller of this
/// function reaches it before [`strict_port`], so this is the one place
/// that check needs to live to cover every URL/proxy value either engine
/// checks at all.
pub fn check_scheme(scheme: &str, original_url: &str, allowed: &[&str]) -> Result<(), String> {
    check_no_control_or_whitespace(original_url, redact_url)?;
    if allowed
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(scheme))
    {
        return Ok(());
    }
    Err(format!(
        "unsupported URL scheme '{}' in '{}': only {} is supported",
        scheme.to_ascii_lowercase(),
        redact_url(original_url),
        allowed.join(" or ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_url_userinfo_removes_credentials() {
        assert_eq!(
            strip_url_userinfo("http://user:pass@host/x"),
            "http://host/x"
        );
    }

    #[test]
    fn strip_url_userinfo_leaves_a_plain_url_unchanged() {
        assert_eq!(strip_url_userinfo("http://host/x"), "http://host/x");
    }

    #[test]
    fn strip_url_userinfo_strips_to_the_last_at_sign_in_the_authority() {
        let redacted = strip_url_userinfo("socks5://alice:p@ss@proxy:1080/x");
        assert_eq!(redacted, "socks5://proxy:1080/x");
        assert!(!redacted.contains("alice"));
        assert!(!redacted.contains("p@ss"));
    }

    #[test]
    fn strict_port_accepts_an_absent_or_empty_port() {
        assert_eq!(strict_port("host", "http://host/x").unwrap(), None);
        assert_eq!(strict_port("host:", "http://host:/x").unwrap(), None);
    }

    #[test]
    fn strict_port_accepts_1_to_65535() {
        assert_eq!(strict_port("host:1", "http://host:1/x").unwrap(), Some(1));
        assert_eq!(
            strict_port("host:65535", "http://host:65535/x").unwrap(),
            Some(65535)
        );
    }

    #[test]
    fn strict_port_accepts_an_ipv6_host() {
        assert_eq!(
            strict_port("[::1]:8080", "http://[::1]:8080/x").unwrap(),
            Some(8080)
        );
        assert_eq!(strict_port("[::1]", "http://[::1]/x").unwrap(), None);
    }

    #[test]
    fn strict_port_rejects_text_after_an_ipv6_literal_thats_not_a_port_separator() {
        let url = "http://[::1]x/foo";
        let err = strict_port("[::1]x", url).unwrap_err();
        assert_eq!(err, format!("invalid port in URL '{url}': 'x'"));
    }

    #[test]
    fn strict_port_rejects_zero_out_of_range_and_non_digit_forms() {
        for (netloc, port_text) in [
            ("host:0", "0"),
            ("host:65536", "65536"),
            ("host:abc", "abc"),
            ("host:+9", "+9"),
            ("host:0_9", "0_9"),
            ("host: 9", " 9"),
        ] {
            let url = format!("http://{netloc}/x");
            let err = strict_port(netloc, &url).unwrap_err();
            assert_eq!(err, format!("invalid port in URL '{url}': '{port_text}'"));
        }
    }

    #[test]
    fn strict_port_message_strips_userinfo() {
        let err =
            strict_port("user:secret@host:99999", "http://user:secret@host:99999/x").unwrap_err();
        assert!(!err.contains("secret"));
        assert!(!err.contains("user"));
        assert_eq!(err, "invalid port in URL 'http://host:99999/x': '99999'");
    }

    #[test]
    fn check_scheme_accepts_an_allowed_scheme() {
        check_scheme("http", "http://host/x", &["http", "https"]).unwrap();
        check_scheme("https", "https://host/x", &["http", "https"]).unwrap();
    }

    #[test]
    fn check_scheme_rejects_a_control_character_or_whitespace() {
        for url in [
            "http://host\0/x",
            "http://ho st/x",
            "http://host\t/x",
            "http://host\n/x",
            "http://host/\u{7f}x",
        ] {
            let err = check_scheme("http", url, &["http", "https"]).unwrap_err();
            assert!(
                err.starts_with("URL contains a control character or whitespace: "),
                "url: {url:?}, err: {err:?}"
            );
        }
    }

    #[test]
    fn check_scheme_rejects_anything_else() {
        let err = check_scheme("ftp", "ftp://host/x", &["http", "https"]).unwrap_err();
        assert_eq!(
            err,
            "unsupported URL scheme 'ftp' in 'ftp://host/x': only http or https is supported"
        );
    }

    #[test]
    fn mask_credential_query_params_masks_a_matching_key_only() {
        assert_eq!(
            mask_credential_query_params("https://host/x?api_key=sk-test&q=widgets"),
            "https://host/x?api_key=%2A%2A%2A&q=widgets"
        );
    }

    #[test]
    fn mask_credential_query_params_leaves_a_url_with_no_query_unchanged() {
        assert_eq!(
            mask_credential_query_params("https://host/x"),
            "https://host/x"
        );
    }

    #[test]
    fn redact_url_strips_userinfo_and_masks_a_credential_query_param() {
        assert_eq!(
            redact_url("https://user:s3cr3t@host/x?api_key=sk-test"),
            "https://host/x?api_key=%2A%2A%2A"
        );
    }

    #[test]
    fn check_scheme_refusal_masks_a_credential_query_param_too() {
        let err = check_scheme(
            "gopher",
            "gopher://user:s3cr3t@host/path?api_key=sk-test",
            &["http", "https"],
        )
        .unwrap_err();
        assert!(!err.contains("s3cr3t"));
        assert!(!err.contains("sk-test"));
        assert!(err.contains("unsupported URL scheme 'gopher'"));
    }
}
