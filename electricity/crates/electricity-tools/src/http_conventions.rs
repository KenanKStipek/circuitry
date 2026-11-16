//! `circuitry.http_conventions`, ported -- issue #778 step 7: strict port
//! parsing and the http(s)-only scheme check, shared by the `http` tool,
//! every adapter's `base_url` ([`crate::http_client::proxy`], and
//! `electricity-adapters::transport`, which depends on this crate), and a
//! proxy URL. Both engines refuse the same input with the same text, so
//! this is never third-party text -- it is Circuitry's own, compared
//! exactly.

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
fn mask_credential_query_params(url: &str) -> String {
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
/// `/`, `?`, `#` or whitespace boundary: this always receives one
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

/// `http_conventions._redact_url` -- the two masks every refusal
/// message that names a request URL (never a proxy value, see
/// [`redact_proxy_value`]) applies before naming it: userinfo
/// stripped, then any credential-like query parameter masked. Never
/// called directly outside this module ([`redact_request_url`] is the
/// one public redactor for a request URL); kept for the test that
/// exercises it in isolation.
#[cfg(test)]
fn redact_url(url: &str) -> String {
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

/// `http_conventions._proxy_authority` -- the authority text of
/// *normalized* (a proxy value that already has a `scheme://`):
/// bounded at the first `/`, `?` or `#` after it -- the same boundary
/// curl, `http.client` and a raw socket all use when they read the
/// value back out, so `http://proxy.corp:3128/` names the authority
/// `proxy.corp:3128`, never the whole remainder (issue #778). `Err`
/// when an `@` appears in the part *after* that boundary: a password
/// hidden behind a `/`, `?` or `#` to smuggle a second, *real* host
/// past whichever caller stops at the first one would otherwise reach
/// it unchecked.
pub(crate) fn proxy_authority(normalized: &str) -> Result<&str, String> {
    let after_scheme = normalized
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(normalized);
    let boundary = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let (authority, tail) = (&after_scheme[..boundary], &after_scheme[boundary..]);
    if tail.contains('@') {
        return Err(format!(
            "invalid proxy URL '{}'",
            redact_proxy_value(normalized)
        ));
    }
    Ok(authority)
}

/// `http_conventions._proxy_host_port` -- the host[:port] text of
/// *normalized* (a proxy value that already has a `scheme://`):
/// [`proxy_authority`], split at the LAST `@` -- a proxy value's
/// userinfo may itself contain one (issue #778), but never past the
/// authority's own boundary any more.
pub(crate) fn proxy_host_port(normalized: &str) -> Result<&str, String> {
    let authority = proxy_authority(normalized)?;
    Ok(match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    })
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

/// Whether an `@` appears anywhere in *url* after its parsed *netloc*
/// -- a password hidden behind a `/`, `?` or `#` can still smuggle a
/// second, real host past a boundary that stops at the first one
/// (issue #778). True regardless of whether *netloc* already carries
/// userinfo of its own: an email address used as a username
/// (`https://me@corp.com:pa/ss@api.example.com/v1`) still leaves a
/// credential fragment exposed if this were ever gated on *netloc*
/// having no `@` of its own.
fn at_follows_netloc(url: &str, netloc: &str) -> bool {
    let scheme_end = url.find("://");
    let rest = scheme_end
        .and_then(|idx| url.get(idx + 3 + netloc.len()..))
        .unwrap_or("");
    rest.contains('@')
}

/// *netloc*'s port, the strict port rule -- when an `@` appears
/// anywhere in *original_url* after its parsed authority
/// ([`at_follows_netloc`]) -- e.g. `https://user:pa/ss@api.example.
/// com/v1` (the authority ends at the first `/`, so *netloc* reads
/// `user:pa`, and `pa` looks like an ordinary invalid port, not half
/// of a password), or `https://me@corp.com:pa/ss@api.example.com/v1`
/// (the authority's own userinfo is itself an email address,
/// `me@corp.com`, and `pa` is still a port attempt, not half of a
/// password) -- the "port" text is really a credential fragment: the
/// refusal never echoes it, and names *original_url* with everything
/// up to the LAST `@` in the whole value cut away instead of the
/// usual userinfo-stripped form, whether or not the authority's own
/// userinfo already has an `@` of its own (issue #778).
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
    let redacted = redact_request_url(original_url);
    if at_follows_netloc(original_url, netloc) {
        return Err(format!("invalid port in URL '{redacted}'"));
    }
    Err(format!("invalid port in URL '{redacted}': '{port_text}'"))
}

/// Like [`strict_port`], but for a proxy value: its host[:port] text
/// comes from [`proxy_host_port`], not `urlsplit`'s own netloc, and any
/// refusal names *normalized* through [`redact_proxy_value`] (issue
/// #778).
pub(crate) fn strict_port_for_proxy(normalized: &str) -> Result<Option<u16>, String> {
    let host_port = proxy_host_port(normalized)?;
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

/// *url*, redacted for any refusal, warning, event, state, hint or
/// `host:` text that names a request URL -- the one function every
/// such caller uses, never `redact_url` (the private test helper) or
/// [`redact_proxy_value`] directly: the whole value is cut at the
/// LAST `@` (never just the authority's own userinfo) when *url*'s
/// parsed `netloc` is empty (a userinfo-bearing value with no scheme
/// at all, or one whose scheme has no `//` authority of its own even
/// though the literal text `://` appears somewhere later in it, e.g.
/// in a query value -- neither leaves [`crate::http::http_url::urlsplit`]
/// any authority for [`strip_request_url_userinfo`] to find anything
/// in) or [`at_follows_netloc`] is true (a password hidden behind a
/// `/`, `?` or `#` can still smuggle a second, real host past the
/// parsed authority); [`strip_request_url_userinfo`] otherwise. Either
/// way, the cut runs *before* any credential-like query parameter is
/// masked: masking first re-encodes a query string's own `@` as
/// `%40`, so a cut that ran afterward would no longer find it (issue
/// #778). Shared by every surface that names a request URL after
/// parsing it: [`check_scheme`], [`strict_port`],
/// `curl_text::redact_host_url`, `curl_text::curl_failure_message`,
/// and every adapter error that names a request URL after I/O.
pub fn redact_request_url(url: &str) -> String {
    let netloc = crate::http::http_url::urlsplit(url, "").netloc;
    let cut = if netloc.is_empty() {
        match url.rsplit_once('@') {
            Some((_, after)) => after.to_string(),
            None => url.to_string(),
        }
    } else if at_follows_netloc(url, &netloc) {
        redact_proxy_value(url)
    } else {
        strip_request_url_userinfo(url)
    };
    mask_credential_query_params(&cut)
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
/// checks at all. *scheme* itself is named through [`redact_request_url`]
/// too, not just *original_url*: every other caller's *scheme* comes
/// from a real [`crate::http::http_url::urlsplit`], but
/// `electricity-adapters::transport`'s own caller finds its scheme by
/// cutting at the first literal `://` -- for a malformed value like
/// `alice:pw@host/x?next=http://y`, that is almost the whole value,
/// userinfo included (issue #778).
pub fn check_scheme(scheme: &str, original_url: &str, allowed: &[&str]) -> Result<(), String> {
    check_no_control_or_whitespace(original_url, redact_request_url)?;
    if allowed
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(scheme))
    {
        return Ok(());
    }
    Err(format!(
        "unsupported URL scheme '{}' in '{}': only {} is supported",
        redact_request_url(&scheme.to_ascii_lowercase()),
        redact_request_url(original_url),
        allowed.join(" or ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn strict_port_never_echoes_a_password_disguised_as_a_port_with_an_email_username() {
        // `https://me@corp.com:pa/ss@api.example.com/v1`'s authority
        // (`me@corp.com:pa`) already has userinfo of its own -- an
        // email address used as a username -- so a gate on "netloc has
        // no `@`" would wrongly treat `pa` as an ordinary invalid
        // port and echo the rest of the password (`pa/ss`) in the
        // refusal (issue #778).
        let url = "https://me@corp.com:pa/ss@api.example.com/v1";
        let netloc = "me@corp.com:pa";
        let err = strict_port(netloc, url).unwrap_err();
        assert_eq!(err, "invalid port in URL 'https://api.example.com/v1'");
        assert!(!err.contains("me"));
        assert!(!err.contains("corp.com"));
        assert!(!err.contains("pa"));
        assert!(!err.contains("ss"));
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
    fn check_scheme_refusal_never_echoes_a_password_hidden_past_the_authority() {
        // The parsed netloc for each of these is `user:pa` (the
        // authority ends at the hidden-password separator), with no
        // `@` of its own -- a plain `redact_url` leaves the rest of
        // the password (`ss`) untouched (issue #778).
        for sep in ['/', '?', '#'] {
            let url = format!("ftp://user:pa{sep}ss@host/x");
            let err = check_scheme("ftp", &url, &["http", "https"]).unwrap_err();
            assert_eq!(
                err,
                "unsupported URL scheme 'ftp' in 'ftp://host/x': only http or https is supported"
            );
            assert!(!err.contains("user"));
            assert!(!err.contains("pa"));
            assert!(!err.contains("ss"));
        }
    }

    #[test]
    fn check_scheme_control_character_refusal_never_echoes_a_password_hidden_past_the_authority() {
        // Same hidden-password shape as the scheme refusal above, but
        // for the control-character/whitespace check, which runs
        // first (issue #778).
        for sep in ['/', '?', '#'] {
            let url = format!("http://user:qq{sep}rr@host\0/x");
            let err = check_scheme("http", &url, &["http", "https"]).unwrap_err();
            assert!(err.starts_with("URL contains a control character or whitespace: "));
            assert!(!err.contains("user"));
            assert!(!err.contains("qq"));
            assert!(!err.contains("rr"));
        }
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
