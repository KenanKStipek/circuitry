"""Unit tests for issue #778 step 7's shared HTTP conventions module: one
proxy rule, strict ports, and the http(s)-only scheme check.
"""

from __future__ import annotations

import pytest

from circuitry.http_conventions import (
    check_proxy_url,
    check_strict_port,
    check_url,
    resolve_proxy,
    split_proxy_url,
    strip_url_userinfo,
)

# ---------------------------------------------------------------------------
# Strict ports (H3)
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "url",
    [
        "http://host/x",
        "http://host:80/x",
        "http://host:1/x",
        "http://host:65535/x",
        "http://host:/x",
        "http://[::1]:8080/x",
        "http://[::1]/x",
        "http://user:pass@host:80/x",
    ],
)
def test_check_strict_port_accepts_valid_ports(url: str) -> None:
    check_strict_port(url)


@pytest.mark.parametrize(
    "url",
    [
        "http://host:99999/x",
        "http://host:abc/x",
        "http://host:+9/x",
        "http://host:0_9/x",
        "http://host:0/x",
        "http://host:65536/x",
        "http://host: 9/x",
    ],
)
def test_check_strict_port_rejects_invalid_ports(url: str) -> None:
    with pytest.raises(ValueError, match="invalid port"):
        check_strict_port(url)


def test_check_strict_port_message_strips_userinfo() -> None:
    with pytest.raises(ValueError) as exc_info:
        check_strict_port("http://user:secret@host:99999/x")
    assert "secret" not in str(exc_info.value)
    assert "user" not in str(exc_info.value)


# ---------------------------------------------------------------------------
# Scheme refusal (H4)
# ---------------------------------------------------------------------------


def test_check_url_accepts_http_and_https() -> None:
    check_url("http://host/x")
    check_url("https://host/x")


@pytest.mark.parametrize("url", ["file:///etc/passwd", "ftp://host/x", "data:text/plain,hi"])
def test_check_url_rejects_other_schemes(url: str) -> None:
    with pytest.raises(ValueError, match="unsupported URL scheme"):
        check_url(url)


def test_check_url_also_checks_the_port() -> None:
    with pytest.raises(ValueError, match="invalid port"):
        check_url("http://host:99999/x")


# ---------------------------------------------------------------------------
# strip_url_userinfo
# ---------------------------------------------------------------------------


def test_strip_url_userinfo_removes_credentials() -> None:
    assert strip_url_userinfo("http://user:pass@host/x") == "http://host/x"


def test_strip_url_userinfo_leaves_a_plain_url_unchanged() -> None:
    assert strip_url_userinfo("http://host/x") == "http://host/x"


# ---------------------------------------------------------------------------
# The proxy rule (H1)
# ---------------------------------------------------------------------------


def test_lowercase_http_proxy_wins_over_uppercase() -> None:
    env = {"http_proxy": "http://lower:3128", "HTTP_PROXY": "http://upper:3128"}
    assert resolve_proxy("http", "example.com", env) == "http://lower:3128"


def test_uppercase_http_proxy_is_never_read() -> None:
    env = {"HTTP_PROXY": "http://upper:3128"}
    assert resolve_proxy("http", "example.com", env) is None


def test_https_falls_back_from_lowercase_to_uppercase() -> None:
    env = {"HTTPS_PROXY": "http://upper:3128"}
    assert resolve_proxy("https", "example.com", env) == "http://upper:3128"


def test_both_schemes_fall_back_to_all_proxy_then_uppercase() -> None:
    assert resolve_proxy("http", "example.com", {"all_proxy": "http://p:1"}) == "http://p:1"
    assert resolve_proxy("https", "example.com", {"ALL_PROXY": "http://p:1"}) == "http://p:1"


def test_a_schemeless_proxy_value_defaults_to_http() -> None:
    env = {"http_proxy": "proxy.corp:3128"}
    assert resolve_proxy("http", "example.com", env) == "http://proxy.corp:3128"


def test_no_proxy_star_bypasses_everything() -> None:
    env = {"http_proxy": "http://p:1", "no_proxy": "*"}
    assert resolve_proxy("http", "example.com", env) is None


def test_no_proxy_matches_host_exactly_and_by_suffix() -> None:
    env = {"http_proxy": "http://p:1", "no_proxy": "example.com"}
    assert resolve_proxy("http", "example.com", env) is None
    assert resolve_proxy("http", "api.example.com", env) is None
    assert resolve_proxy("http", "notexample.com", env) == "http://p:1"


def test_no_proxy_leading_dot_is_optional() -> None:
    env = {"http_proxy": "http://p:1", "no_proxy": ".example.com"}
    assert resolve_proxy("http", "api.example.com", env) is None


def test_no_proxy_uppercase_fallback() -> None:
    env = {"http_proxy": "http://p:1", "NO_PROXY": "example.com"}
    assert resolve_proxy("http", "example.com", env) is None


def test_no_proxy_entries_are_comma_separated() -> None:
    env = {"http_proxy": "http://p:1", "no_proxy": "a.com, example.com ,b.com"}
    assert resolve_proxy("http", "example.com", env) is None


def test_resolve_proxy_returns_none_with_no_matching_env_var() -> None:
    assert resolve_proxy("http", "example.com", {}) is None
    assert resolve_proxy("http", "example.com", {"https_proxy": "http://p:1"}) is None


def test_check_proxy_url_rejects_non_http_scheme() -> None:
    with pytest.raises(ValueError, match="unsupported proxy scheme"):
        check_proxy_url("socks5://host:1080")


def test_check_proxy_url_rejects_an_invalid_port() -> None:
    with pytest.raises(ValueError, match="invalid port"):
        check_proxy_url("http://host:99999")


def test_split_proxy_url_without_userinfo() -> None:
    result = split_proxy_url("http://proxy.corp:3128")
    assert result.hostport == "proxy.corp:3128"
    assert result.basic_auth is None


def test_split_proxy_url_with_userinfo_builds_basic_auth() -> None:
    import base64

    result = split_proxy_url("http://alice:s3cr3t@proxy.corp:3128")
    assert result.hostport == "proxy.corp:3128"
    assert result.basic_auth == base64.b64encode(b"alice:s3cr3t").decode("ascii")
