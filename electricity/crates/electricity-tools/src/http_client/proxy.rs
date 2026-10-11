//! `urllib.request.getproxies_environment`/`proxy_bypass_environment`,
//! ported (lane E, issue #485) -- the one proxy convention this lane
//! implements (#448 decision 1: the `http` tool follows urllib's own
//! rule; curl's own convention, for lane F2's adapters, is a separate,
//! later extension of this module). Pure functions over an explicit
//! environment snapshot, never `std::env` directly, so a test (and the
//! generator's own differential corpus replay) can drive them with a
//! synthetic environment instead of the process's real one -- the
//! lane's own environment-isolation rule (every test that resolves a
//! proxy sets or clears every `*_proxy`/`*_PROXY` variable explicitly).

/// `getproxies_environment()`'s own two-pass precedence rule: *env* is
/// `(name, value)` pairs in the process's own declaration order (the
/// order `os.environ.items()`/`std::env::vars()` both iterate in, read
/// straight from the C `environ` array on every platform this crate
/// targets) -- first *any* case-insensitive `<scheme>_proxy` sets
/// `proxies[scheme]`, last one in iteration order winning; then, if
/// `REQUEST_METHOD` is present (CPython's own CGI guard, CVE-2016-
/// 1000110), the `http` entry set by that first pass is discarded
/// outright; then a second pass considers only a variable whose own,
/// *unlowered* name already ends in `_proxy` (so `HTTP_PROXY` never
/// qualifies here, only `http_proxy` does) -- a non-empty value
/// overwrites `proxies[scheme]` regardless of what the first pass
/// found (restoring an `http` entry the CGI guard just removed, if a
/// lowercase `http_proxy` exists), and an *empty* value instead
/// removes that scheme's entry outright, even one the first pass set
/// from an uppercase variable.
pub fn getproxies_environment(env: &[(String, String)]) -> Vec<(String, String)> {
    let mut proxies: Vec<(String, String)> = Vec::new();
    let set = |proxies: &mut Vec<(String, String)>, scheme: String, value: String| {
        if let Some(entry) = proxies.iter_mut().find(|(k, _)| *k == scheme) {
            entry.1 = value;
        } else {
            proxies.push((scheme, value));
        }
    };
    let remove = |proxies: &mut Vec<(String, String)>, scheme: &str| {
        proxies.retain(|(k, _)| k != scheme);
    };

    for (name, value) in env {
        let lower = name.to_lowercase();
        if !value.is_empty() && lower.ends_with("_proxy") {
            let scheme = lower[..lower.len() - "_proxy".len()].to_string();
            set(&mut proxies, scheme, value.clone());
        }
    }
    if env.iter().any(|(name, _)| name == "REQUEST_METHOD") {
        remove(&mut proxies, "http");
    }
    for (name, value) in env {
        if !name.ends_with("_proxy") {
            continue;
        }
        let scheme = name[..name.len() - "_proxy".len()].to_lowercase();
        if value.is_empty() {
            remove(&mut proxies, &scheme);
        } else {
            set(&mut proxies, scheme, value.clone());
        }
    }
    proxies
}

/// `proxy_bypass_environment(host)`: *proxies* is [`getproxies_environment`]'s
/// own result; *host* is the already-split `hostname[:port]` (urllib's
/// own `req.host`, which already strips userinfo). `no_proxy == "*"`
/// bypasses every host; otherwise each comma-separated entry (leading/
/// trailing whitespace, and a leading dot, both ignored) matches a
/// bare hostname exactly, a subdomain by suffix (`hostonly == name` or
/// `hostonly` ends with `"." + name`), or `host:port` verbatim against
/// an entry that itself carries a port. No special-case for
/// `localhost`/loopback -- issue #485's own ruling: unlike curl/most
/// browsers, urllib's own rule exempts nothing unless `no_proxy` says
/// so.
pub fn proxy_bypass_environment(proxies: &[(String, String)], host: &str) -> bool {
    let Some((_, no_proxy)) = proxies.iter().find(|(k, _)| k == "no") else {
        return false;
    };
    if no_proxy == "*" {
        return true;
    }
    let host_lower = host.to_lowercase();
    let (hostonly, port) = split_host_port(&host_lower);
    for raw_entry in no_proxy.split(',') {
        let entry = raw_entry.trim();
        if entry.is_empty() {
            continue;
        }
        let name = entry.trim_start_matches('.').to_lowercase();
        if name.is_empty() {
            continue;
        }
        if hostonly == name || hostonly.ends_with(&format!(".{name}")) {
            return true;
        }
        if let Some(port) = port {
            if format!("{hostonly}:{port}") == name {
                return true;
            }
        }
    }
    false
}

/// `_splitport`-equivalent: the last `:` (if any) that isn't part of an
/// IPv6 literal's own brackets splits the host from the port --
/// `host[:port]` only ever carries one `:` outside brackets in
/// practice here (a bracketed IPv6 literal's own colons are never
/// split), so a plain `rsplit_once` after stripping brackets suffices.
fn split_host_port(host: &str) -> (String, Option<&str>) {
    if let Some(rest) = host.strip_prefix('[') {
        if let Some((addr, after)) = rest.split_once(']') {
            let port = after.strip_prefix(':').filter(|p| !p.is_empty());
            return (format!("[{addr}]"), port);
        }
    }
    match host.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_string(), Some(p))
        }
        _ => (host.to_string(), None),
    }
}

/// The `http`/`https` proxy URL for *scheme*/*host* under urllib's own
/// convention, `None` when there's no proxy configured for *scheme* or
/// `no_proxy` bypasses *host* -- [`crate::http::HttpPlugin`]'s own
/// caller; *env* is the same explicit snapshot [`getproxies_environment`]
/// takes (never read from the real process environment inside this
/// function either, for the same test-isolation reason).
pub fn resolve_proxy(scheme: &str, host: &str, env: &[(String, String)]) -> Option<String> {
    let proxies = getproxies_environment(env);
    if proxy_bypass_environment(&proxies, host) {
        return None;
    }
    proxies
        .iter()
        .find(|(k, _)| k == scheme)
        .map(|(_, v)| v.clone())
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
    fn lowercase_wins_over_uppercase_regardless_of_order() {
        let e = env(&[("HTTP_PROXY", "http://upper:1"), ("http_proxy", "http://lower:2")]);
        let proxies = getproxies_environment(&e);
        assert_eq!(
            proxies.iter().find(|(k, _)| k == "http").map(|(_, v)| v.as_str()),
            Some("http://lower:2")
        );
    }

    #[test]
    fn uppercase_http_proxy_is_ignored_under_cgi() {
        let e = env(&[("HTTP_PROXY", "http://upper:1"), ("REQUEST_METHOD", "GET")]);
        let proxies = getproxies_environment(&e);
        assert!(proxies.iter().find(|(k, _)| k == "http").is_none());
    }

    #[test]
    fn lowercase_http_proxy_survives_cgi_even_after_the_uppercase_guard() {
        let e = env(&[
            ("HTTP_PROXY", "http://upper:1"),
            ("http_proxy", "http://lower:2"),
            ("REQUEST_METHOD", "GET"),
        ]);
        let proxies = getproxies_environment(&e);
        assert_eq!(
            proxies.iter().find(|(k, _)| k == "http").map(|(_, v)| v.as_str()),
            Some("http://lower:2")
        );
    }

    #[test]
    fn an_empty_lowercase_value_removes_an_uppercase_hit() {
        let e = env(&[("HTTPS_PROXY", "http://upper:1"), ("https_proxy", "")]);
        let proxies = getproxies_environment(&e);
        assert!(proxies.iter().find(|(k, _)| k == "https").is_none());
    }

    #[test]
    fn no_proxy_star_bypasses_everything() {
        let proxies = env(&[("no", "*")]);
        assert!(proxy_bypass_environment(&proxies, "example.com"));
    }

    #[test]
    fn no_proxy_matches_a_suffix_with_a_leading_dot() {
        let proxies = env(&[("no", ".example.com")]);
        assert!(proxy_bypass_environment(&proxies, "api.example.com"));
        assert!(proxy_bypass_environment(&proxies, "example.com"));
        assert!(!proxy_bypass_environment(&proxies, "notexample.com"));
    }

    #[test]
    fn no_proxy_matches_a_bare_hostname_exactly() {
        let proxies = env(&[("no", "example.com")]);
        assert!(proxy_bypass_environment(&proxies, "example.com"));
        assert!(!proxy_bypass_environment(&proxies, "other.com"));
    }

    #[test]
    fn no_proxy_is_port_aware() {
        let proxies = env(&[("no", "example.com:8080")]);
        assert!(proxy_bypass_environment(&proxies, "example.com:8080"));
        assert!(!proxy_bypass_environment(&proxies, "example.com:9090"));
        assert!(!proxy_bypass_environment(&proxies, "example.com"));
    }

    #[test]
    fn loopback_is_not_exempt_unless_listed() {
        let proxies = env(&[("http_proxy", "http://proxy:3128")]);
        assert_eq!(
            resolve_proxy("http", "127.0.0.1:8080", &proxies),
            Some("http://proxy:3128".to_string())
        );
    }

    #[test]
    fn resolve_proxy_returns_none_when_bypassed() {
        let e = env(&[("http_proxy", "http://proxy:3128"), ("no_proxy", "example.com")]);
        assert_eq!(resolve_proxy("http", "example.com", &e), None);
    }

    #[test]
    fn resolve_proxy_returns_none_when_no_scheme_entry() {
        let e = env(&[("https_proxy", "http://proxy:3128")]);
        assert_eq!(resolve_proxy("http", "example.com", &e), None);
    }
}
