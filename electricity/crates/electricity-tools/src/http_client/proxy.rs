//! `circuitry.http_conventions`'s proxy rule, ported (issue #778) --
//! one convention, shared by the `http` tool
//! ([`resolve_proxy_checked`] below) and every adapter
//! (`electricity-adapters::transport`, which calls this same function
//! rather than keeping its own, separate copy). Circuitry decides the
//! proxy itself: neither urllib's nor curl's own environment-reading
//! convention applies on either engine any more. Every caller gets the
//! checked `Result` form -- a proxy value that can't be used (an
//! unsupported scheme, an invalid port) is refused outright, never
//! silently treated as "no proxy" and sent direct.
//!
//! Pure functions over an explicit environment snapshot, never
//! `std::env` directly, so a test (and the generator's own differential
//! corpus replay) can drive them with a synthetic environment instead of
//! the process's real one -- every test that resolves a proxy sets or
//! clears every `*_proxy`/`*_PROXY` variable explicitly.

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

fn is_ip_address(text: &str) -> bool {
    text.parse::<std::net::IpAddr>().is_ok()
}

/// *host_lower* matches a bare (no-port) `no_proxy` *entry*: once
/// *host_lower* is itself an IP address, only an exact match counts --
/// never a domain suffix (issue #778 -- `0.0.1`
/// must never bypass `127.0.0.1` just because the text happens to end
/// the same way a real domain suffix would); a named host matches
/// *host_lower* exactly or as a domain suffix (a leading dot optional).
fn host_matches_entry(host_lower: &str, entry: &str) -> bool {
    if is_ip_address(host_lower) {
        return host_lower == entry;
    }
    host_lower == entry || host_lower.ends_with(&format!(".{entry}"))
}

/// The scheme's own default port -- used only to decide whether a
/// `host:port` `no_proxy` entry matches a URL that named no port of
/// its own: `example.com:443` still matches a portless
/// `https://example.com/` (issue #778).
fn scheme_default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

/// The proxy rule's `no_proxy`/`NO_PROXY`: a comma-separated list; `*`
/// bypasses everything, and a named entry matches *host* exactly or as
/// a domain suffix (a leading dot is optional). An IP entry matches
/// only that exact IP (no suffix, no CIDR). A `host:port` entry (the
/// host part not itself an IP) bypasses only that host on that exact
/// *port* -- *port* `None` (the URL named none of its own) falls back
/// to *scheme*'s own default rather than never matching (issue #778).
fn no_proxy_bypasses(no_proxy: &str, scheme: &str, host: &str, port: Option<u16>) -> bool {
    if no_proxy.trim() == "*" {
        return true;
    }
    let effective_port = port.or_else(|| scheme_default_port(scheme));
    let host_lower = host.to_ascii_lowercase();
    for raw_entry in no_proxy.split(',') {
        let entry = raw_entry.trim().to_ascii_lowercase();
        if entry.is_empty() {
            continue;
        }
        if is_ip_address(&entry) || is_ip_address(&host_lower) {
            if host_lower == entry {
                return true;
            }
            continue;
        }
        if let Some((entry_host, entry_port_text)) = entry.rsplit_once(':') {
            if !entry_host.is_empty()
                && !entry_port_text.is_empty()
                && entry_port_text.chars().all(|c| c.is_ascii_digit())
            {
                if let (Some(port), Ok(entry_port)) =
                    (effective_port, entry_port_text.parse::<u16>())
                {
                    if entry_port == port
                        && host_matches_entry(&host_lower, entry_host.trim_start_matches('.'))
                    {
                        return true;
                    }
                }
                continue;
            }
        }
        if host_matches_entry(&host_lower, entry.trim_start_matches('.')) {
            return true;
        }
    }
    false
}

/// The proxy rule's scheme-specific lookup, before the
/// `all_proxy`/`ALL_PROXY` fallback: an `http` URL uses `http_proxy`, lowercase only -- uppercase
/// `HTTP_PROXY` is never read; an `https` URL uses `https_proxy`, then
/// `HTTPS_PROXY`.
fn proxy_value(
    scheme: &str,
    host: &str,
    port: Option<u16>,
    env: &[(String, String)],
) -> Option<String> {
    let no_proxy = env_first(env, &["no_proxy", "NO_PROXY"]).unwrap_or_default();
    if no_proxy_bypasses(&no_proxy, scheme, host, port) {
        return None;
    }
    let scheme_specific = match scheme {
        "http" => env_first(env, &["http_proxy"]),
        "https" => env_first(env, &["https_proxy", "HTTPS_PROXY"]),
        _ => None,
    };
    scheme_specific.or_else(|| env_first(env, &["all_proxy", "ALL_PROXY"]))
}

/// [`resolve_proxy_checked_with_port`] with no port of its own to
/// name -- a `host:port` `no_proxy` entry still matches against
/// *scheme*'s own default port then, never "no such entry can ever
/// match" (issue #778) -- the plain form every existing test (and
/// `electricity-adapters::transport`'s own two real call sites'
/// *predecessor*) uses.
pub fn resolve_proxy_checked(
    scheme: &str,
    host: &str,
    env: &[(String, String)],
) -> Result<Option<String>, String> {
    resolve_proxy_checked_with_port(scheme, host, None, env)
}

/// The proxy rule for *scheme*/*host*(:*port*), normalized and checked
/// -- *env* an explicit snapshot, never read from the real process
/// environment by this function. `Ok(None)` when no proxy applies
/// (unset, or `no_proxy` bypasses *host*); `Ok(Some(url))` with a
/// `http://`-scheme URL (a value with no `scheme://` prefix defaults to
/// that); `Err` with Circuitry's own refusal text when the value names
/// an unsupported proxy scheme, or its port is strictly invalid.
/// *port* only ever matters for a `host:port` `no_proxy` entry (issue
/// #778); `None` (the caller's own URL named no port of its own)
/// still matches such an entry against *scheme*'s own default port,
/// never "can never match" -- see [`no_proxy_bypasses`]'s own
/// `effective_port`.
pub fn resolve_proxy_checked_with_port(
    scheme: &str,
    host: &str,
    port: Option<u16>,
    env: &[(String, String)],
) -> Result<Option<String>, String> {
    let Some(value) = proxy_value(scheme, host, port, env) else {
        return Ok(None);
    };
    let normalized = if value.contains("://") {
        value
    } else {
        format!("http://{value}")
    };
    let proxy_scheme = normalized
        .split_once("://")
        .map(|(s, _)| s.to_ascii_lowercase())
        .unwrap_or_default();
    // A proxy value has no meaningful path, so its own control-
    // character/scheme/port checks never use `check_scheme`/
    // `strict_port`'s own `urlsplit`-bounded authority -- a password
    // containing `/`, `?`, `#` or whitespace must still be removed in
    // full from any refusal this raises (issue #778).
    crate::http_conventions::check_no_control_or_whitespace(
        &normalized,
        crate::http_conventions::redact_proxy_value,
    )?;
    if !proxy_scheme.eq_ignore_ascii_case("http") {
        return Err(format!(
            "unsupported proxy scheme '{proxy_scheme}' in '{}': only http:// proxies are \
             supported",
            crate::http_conventions::redact_proxy_value(&normalized)
        ));
    }
    crate::http_conventions::strict_port_for_proxy(&normalized)?;
    Ok(Some(with_default_port(&normalized, 80)?))
}

/// *url* with `:<default_port>` written right after its authority
/// (`http_conventions::proxy_authority`, never `urlsplit`'s own
/// `/`/`?`/`#`-bounded netloc) when it names no port of its own, not
/// even an empty one -- curl's own default for a schemeless/portless
/// proxy is 1080, its historic SOCKS convention, never this module's
/// own 80, so the default is written in explicitly rather than left for curl (or
/// `http.rs`'s own port-defaulting, which would otherwise use the
/// *target* URL's scheme) to guess (issue #778). The authority is no
/// longer always a suffix of *url* -- a trailing `/`, `?` or `#` (and
/// whatever follows it) can come after it -- so the default port is
/// spliced in right after the authority ends, never appended to the
/// end of *url* itself.
fn with_default_port(url: &str, default_port: u16) -> Result<String, String> {
    let authority = crate::http_conventions::proxy_authority(url)?;
    let host_port = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };
    if crate::http_conventions::host_port_text(host_port).is_some_and(|text| !text.is_empty()) {
        return Ok(url.to_string());
    }
    let authority_end = url.find("://").map(|i| i + 3).unwrap_or(0) + authority.len();
    let separator = if host_port.ends_with(':') { "" } else { ":" };
    Ok(format!(
        "{}{separator}{default_port}{}",
        &url[..authority_end],
        &url[authority_end..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn lowercase_http_proxy_wins_over_uppercase() {
        let e = env(&[
            ("http_proxy", "http://lower:2"),
            ("HTTP_PROXY", "http://upper:1"),
        ]);
        assert_eq!(
            resolve_proxy_checked("http", "example.com", &e).unwrap(),
            Some("http://lower:2".to_string())
        );
    }

    #[test]
    fn uppercase_http_proxy_is_never_read() {
        let e = env(&[("HTTP_PROXY", "http://upper:1")]);
        assert_eq!(
            resolve_proxy_checked("http", "example.com", &e).unwrap(),
            None
        );
    }

    #[test]
    fn https_falls_back_from_lowercase_to_uppercase() {
        let e = env(&[("HTTPS_PROXY", "http://upper:1")]);
        assert_eq!(
            resolve_proxy_checked("https", "example.com", &e).unwrap(),
            Some("http://upper:1".to_string())
        );
    }

    #[test]
    fn both_schemes_fall_back_to_all_proxy_then_uppercase() {
        let e = env(&[("all_proxy", "http://p:1")]);
        assert_eq!(
            resolve_proxy_checked("http", "example.com", &e).unwrap(),
            Some("http://p:1".to_string())
        );
        let e = env(&[("ALL_PROXY", "http://p:1")]);
        assert_eq!(
            resolve_proxy_checked("https", "example.com", &e).unwrap(),
            Some("http://p:1".to_string())
        );
    }

    #[test]
    fn a_schemeless_proxy_value_defaults_to_http() {
        let e = env(&[("http_proxy", "proxy.corp:3128")]);
        assert_eq!(
            resolve_proxy_checked("http", "example.com", &e).unwrap(),
            Some("http://proxy.corp:3128".to_string())
        );
    }

    #[test]
    fn no_proxy_star_bypasses_everything() {
        let e = env(&[("http_proxy", "http://p:1"), ("no_proxy", "*")]);
        assert_eq!(
            resolve_proxy_checked("http", "example.com", &e).unwrap(),
            None
        );
    }

    #[test]
    fn no_proxy_matches_host_exactly_and_by_suffix_with_leading_dot_optional() {
        let e = env(&[("http_proxy", "http://p:1"), ("no_proxy", ".example.com")]);
        assert_eq!(
            resolve_proxy_checked("http", "example.com", &e).unwrap(),
            None
        );
        assert_eq!(
            resolve_proxy_checked("http", "api.example.com", &e).unwrap(),
            None
        );
        assert_eq!(
            resolve_proxy_checked("http", "notexample.com", &e).unwrap(),
            Some("http://p:1".to_string())
        );
    }

    #[test]
    fn no_proxy_ip_entry_matches_only_that_exact_ip() {
        // Issue #778: `0.0.1` must not bypass `127.0.0.1` by suffix.
        let e = env(&[("http_proxy", "http://p:1"), ("no_proxy", "0.0.1")]);
        assert_eq!(
            resolve_proxy_checked("http", "127.0.0.1", &e).unwrap(),
            Some("http://p:1".to_string())
        );
        assert_eq!(resolve_proxy_checked("http", "0.0.1", &e).unwrap(), None);
    }

    #[test]
    fn no_proxy_host_port_entry_matches_only_that_host_on_that_port() {
        let e = env(&[
            ("http_proxy", "http://p:1"),
            ("no_proxy", "example.com:8080"),
        ]);
        assert_eq!(
            resolve_proxy_checked_with_port("http", "example.com", Some(8080), &e).unwrap(),
            None
        );
        assert_eq!(
            resolve_proxy_checked_with_port("http", "example.com", Some(80), &e).unwrap(),
            Some("http://p:1".to_string())
        );
        assert_eq!(
            resolve_proxy_checked("http", "example.com", &e).unwrap(),
            Some("http://p:1".to_string())
        );
    }

    #[test]
    fn no_proxy_host_port_entry_matches_a_urls_implicit_default_port() {
        // Issue #778: `example.com:443` still matches a portless
        // `https://example.com/` -- the URL's own implicit port is
        // 443, the https default, not "no port to compare against".
        let e = env(&[
            ("https_proxy", "http://p:1"),
            ("no_proxy", "example.com:443"),
        ]);
        assert_eq!(
            resolve_proxy_checked("https", "example.com", &e).unwrap(),
            None
        );
        // An explicit, non-default port on the same host still
        // doesn't match the entry.
        assert_eq!(
            resolve_proxy_checked_with_port("https", "example.com", Some(8443), &e).unwrap(),
            Some("http://p:1".to_string())
        );
    }

    #[test]
    fn no_proxy_uppercase_fallback() {
        let e = env(&[("http_proxy", "http://p:1"), ("NO_PROXY", "example.com")]);
        assert_eq!(
            resolve_proxy_checked("http", "example.com", &e).unwrap(),
            None
        );
    }

    #[test]
    fn resolve_proxy_checked_rejects_a_non_http_proxy_scheme() {
        let e = env(&[("http_proxy", "socks5://host:1080")]);
        let err = resolve_proxy_checked("http", "example.com", &e).unwrap_err();
        assert_eq!(
            err,
            "unsupported proxy scheme 'socks5' in 'socks5://host:1080': only http:// proxies \
             are supported"
        );
    }

    #[test]
    fn resolve_proxy_checked_never_leaks_a_colonless_credential_hidden_behind_a_slash() {
        for suffix in ["", "?q=1", "#frag"] {
            let e = env(&[("http_proxy", &format!("http://abc/def@proxy:3128{suffix}"))]);
            let err = resolve_proxy_checked("http", "example.com", &e).unwrap_err();
            assert!(!err.contains("abc"), "suffix {suffix:?}: {err}");
            assert!(!err.contains("def"), "suffix {suffix:?}: {err}");
        }
    }

    #[test]
    fn resolve_proxy_checked_never_leaks_a_colonless_credential_in_an_unsupported_scheme() {
        let e = env(&[("http_proxy", "socks5://abc/def@proxy:1080")]);
        let err = resolve_proxy_checked("http", "example.com", &e).unwrap_err();
        assert!(!err.contains("abc"), "{err}");
        assert!(!err.contains("def"), "{err}");
    }

    #[test]
    fn resolve_proxy_checked_rejects_an_invalid_proxy_port() {
        let e = env(&[("http_proxy", "http://host:99999")]);
        assert!(resolve_proxy_checked("http", "example.com", &e).is_err());
    }

    #[test]
    fn resolve_proxy_checked_writes_the_default_port_80_explicitly() {
        // Issue #778: never left for curl's own 1080 default, or for
        // `http.rs`'s own target-scheme-dependent fallback, to decide.
        for (value, expected) in [
            ("proxy.corp", "http://proxy.corp:80"),
            ("http://proxy.corp", "http://proxy.corp:80"),
            ("http://proxy.corp:", "http://proxy.corp:80"),
            (
                "http://user:pass@proxy.corp",
                "http://user:pass@proxy.corp:80",
            ),
            ("http://proxy.corp:3128", "http://proxy.corp:3128"),
        ] {
            let e = env(&[("http_proxy", value)]);
            assert_eq!(
                resolve_proxy_checked("http", "example.com", &e).unwrap(),
                Some(expected.to_string()),
                "value: {value}"
            );
        }
    }
}
