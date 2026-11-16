"""Unit tests for issue #778 step 7's shared HTTP conventions module: one
proxy rule, strict ports, and the http(s)-only scheme check.
"""

from __future__ import annotations

import pytest

from circuitry.http_conventions import (
    check_proxy_url,
    check_strict_port,
    check_url,
    redact_proxy_value,
    redact_url,
    resolve_proxy,
    split_proxy_url,
    strip_url_userinfo,
)

# ---------------------------------------------------------------------------
# Strict ports
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


def test_check_strict_port_rejects_junk_after_an_ipv6_literal_with_the_shared_text() -> None:
    """``urllib.parse.urlsplit`` itself raises its own bare "Invalid
    IPv6 URL" for this on CPython -- refused here with the same text
    electricity gives, instead (issue #778).
    """
    with pytest.raises(ValueError) as exc_info:
        check_strict_port("http://[::1]x/x")
    assert str(exc_info.value) == "invalid port in URL 'http://[::1]x/x': 'x'"


# ---------------------------------------------------------------------------
# Scheme refusal
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "url",
    [
        "http://host\0/x",
        "http://ho st/x",
        "http://host\t/x",
        "http://host\n/x",
        "http://host/\x7fx",
    ],
)
def test_check_url_rejects_a_control_character_or_whitespace(url: str) -> None:
    """Issue #778."""
    with pytest.raises(ValueError, match="control character or whitespace"):
        check_url(url)


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


def test_strip_url_userinfo_strips_to_the_last_at_sign_in_the_authority() -> None:
    """Issue #778: a password containing ``@`` (and ``:``) must be
    removed in full -- stopping at the *first* ``@`` would leave a
    fragment of it (``ss@proxy:1080``) in the redacted text.
    """
    redacted = strip_url_userinfo("socks5://alice:p@ss@proxy:1080/x")
    assert redacted == "socks5://proxy:1080/x"
    assert "alice" not in redacted
    assert "p@ss" not in redacted
    assert "ss" not in redacted


# ---------------------------------------------------------------------------
# Hostile passwords (issue #778): no piece of one may survive a refusal,
# on either a request URL (``redact_url``) or a proxy value
# (``redact_proxy_value``/``check_proxy_url``).
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("char", (" ", "\r"))
def test_redact_url_removes_a_password_containing_whitespace_or_a_control_char(
    char: str,
) -> None:
    password = f"pa{char}ss"
    redacted = redact_url(f"http://alice:{password}@host:1234/x")
    assert redacted == "http://host:1234/x"
    assert "alice" not in redacted
    assert password not in redacted


@pytest.mark.parametrize("char", (" ", "\r"))
def test_check_url_never_leaks_a_password_containing_whitespace_or_a_control_char(
    char: str,
) -> None:
    password = f"pa{char}ss"
    with pytest.raises(ValueError, match="control character or whitespace") as exc_info:
        check_url(f"http://alice:{password}@host:1234/x")
    message = str(exc_info.value)
    assert "alice" not in message
    assert password not in message


@pytest.mark.parametrize("char", ("/", "?", "#", "@", ":"))
def test_redact_proxy_value_removes_a_password_containing_any_hostile_char(char: str) -> None:
    password = f"pa{char}ss"
    redacted = redact_proxy_value(f"http://alice:{password}@proxy:3128")
    assert redacted == "http://proxy:3128"
    assert "alice" not in redacted
    assert password not in redacted


@pytest.mark.parametrize("char", ("/", "?", "#", "@", ":"))
def test_check_proxy_url_accepts_a_password_containing_a_hostile_non_control_char(
    char: str,
) -> None:
    """None of these end a proxy value's authority the way they would a
    request URL's -- a proxy value has no meaningful path, so the port
    check must still find the real port after the password, not treat
    part of the password itself as an invalid port.
    """
    password = f"pa{char}ss"
    assert check_proxy_url(f"http://alice:{password}@proxy:3128") == (
        f"http://alice:{password}@proxy:3128"
    )


@pytest.mark.parametrize("char", (" ", "\r"))
def test_check_proxy_url_never_leaks_a_password_containing_whitespace_or_a_control_char(
    char: str,
) -> None:
    password = f"pa{char}ss"
    with pytest.raises(ValueError, match="control character or whitespace") as exc_info:
        check_proxy_url(f"http://alice:{password}@proxy:3128")
    message = str(exc_info.value)
    assert "alice" not in message
    assert password not in message


def test_check_proxy_url_never_leaks_a_schemeless_value_with_trailing_whitespace() -> None:
    """A schemeless proxy value has no ``://`` of its own for the old
    redactor to find -- normalizing before the control-character check
    (rather than after) is what lets this one redact at all.
    """
    with pytest.raises(ValueError, match="control character or whitespace") as exc_info:
        check_proxy_url("alice:s3cret@proxy:3128 ")
    message = str(exc_info.value)
    assert "alice" not in message
    assert "s3cret" not in message


# ---------------------------------------------------------------------------
# The proxy rule
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


def test_no_proxy_ip_entry_matches_only_that_exact_ip() -> None:
    """Issue #778: an IP entry is never a domain
    suffix -- ``0.0.1`` must not bypass ``127.0.0.1``.
    """
    env = {"http_proxy": "http://p:1", "no_proxy": "0.0.1"}
    assert resolve_proxy("http", "127.0.0.1", env) == "http://p:1"
    assert resolve_proxy("http", "0.0.1", env) is None


def test_no_proxy_host_port_entry_matches_only_that_host_on_that_port() -> None:
    """Issue #778."""
    env = {"http_proxy": "http://p:1", "no_proxy": "example.com:8080"}
    assert resolve_proxy("http", "example.com", env, port=8080) is None
    assert resolve_proxy("http", "example.com", env, port=80) == "http://p:1"
    assert resolve_proxy("http", "example.com", env) == "http://p:1"


def test_no_proxy_host_port_entry_matches_a_urls_implicit_default_port() -> None:
    """Issue #778: ``example.com:443`` still matches a portless
    ``https://example.com/`` -- the URL's own implicit port is 443,
    the https default, not "no port to compare against".
    """
    env = {"https_proxy": "http://p:1", "no_proxy": "example.com:443"}
    assert resolve_proxy("https", "example.com", env) is None
    assert resolve_proxy("https", "example.com", env, port=8443) == "http://p:1"


def test_no_proxy_host_port_entry_rejects_a_non_ascii_digit_port() -> None:
    """``isdigit()`` alone matches any Unicode decimal digit, which
    ``int()`` then rejects -- never a match either way, just a plain
    one instead of a crash (issue #778).
    """
    env = {"http_proxy": "http://p:1", "no_proxy": "example.com:²"}
    assert resolve_proxy("http", "example.com", env) == "http://p:1"


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


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        ("proxy.corp", "http://proxy.corp:80"),
        ("http://proxy.corp", "http://proxy.corp:80"),
        ("http://proxy.corp:", "http://proxy.corp:80"),
        ("http://user:pass@proxy.corp", "http://user:pass@proxy.corp:80"),
        ("http://[::1]", "http://[::1]:80"),
        ("http://proxy.corp:3128", "http://proxy.corp:3128"),
    ],
)
def test_check_proxy_url_writes_the_default_port_80_explicitly(value: str, expected: str) -> None:
    """Issue #778: curl's own default for a
    schemeless/portless proxy is 1080 (its historic SOCKS convention),
    never the proxy rule's own 80 -- the default is written into the
    normalized proxy URL explicitly so curl's own guess never applies.
    """
    assert check_proxy_url(value) == expected


def test_split_proxy_url_without_userinfo() -> None:
    result = split_proxy_url("http://proxy.corp:3128")
    assert result.hostport == "proxy.corp:3128"
    assert result.basic_auth is None


def test_split_proxy_url_with_userinfo_builds_basic_auth() -> None:
    import base64

    result = split_proxy_url("http://alice:s3cr3t@proxy.corp:3128")
    assert result.hostport == "proxy.corp:3128"
    assert result.basic_auth == base64.b64encode(b"alice:s3cr3t").decode("ascii")
