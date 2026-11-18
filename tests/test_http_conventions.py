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
    redact_request_url,
    redact_url,
    resolve_proxy,
    split_proxy_url,
    strip_request_url_userinfo,
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


@pytest.mark.parametrize("char", ("/", "?", "#"))
def test_check_strict_port_never_echoes_a_password_disguised_as_a_port(char: str) -> None:
    """``https://user:pa/ss@api.example.com/v1``'s authority ends at the
    `/`, so the parsed netloc is ``user:pa`` -- no userinfo of its own
    to strip, as far as a plain authority-only cut is concerned -- yet
    the URL still carries an `@` further along, which means the real
    host (``api.example.com``) and the rest of the password are hidden
    past it. Refused with the shared text, cut to the last ``@`` and the
    port text omitted entirely, never echoing the fragment (``pa``)
    that would otherwise look like an ordinary invalid port (issue
    #778).
    """
    url = f"https://user:pa{char}ss@api.example.com/v1"
    with pytest.raises(ValueError) as exc_info:
        check_strict_port(url)
    message = str(exc_info.value)
    assert message == "invalid port in URL 'https://api.example.com/v1'"
    assert "user" not in message
    assert "pa" not in message
    assert "ss" not in message


def test_check_strict_port_never_echoes_a_password_disguised_as_a_port_with_an_email_username() -> (
    None
):
    """``https://me@corp.com:pa/ss@api.example.com/v1``'s authority
    (``me@corp.com:pa``) already has userinfo of its own -- an email
    address used as a username -- so a gate on "netloc has no ``@``"
    would wrongly treat ``pa`` as an ordinary invalid port and echo the
    rest of the password (``pa/ss``) in the refusal (issue #778).
    """
    url = "https://me@corp.com:pa/ss@api.example.com/v1"
    with pytest.raises(ValueError) as exc_info:
        check_strict_port(url)
    message = str(exc_info.value)
    assert message == "invalid port in URL 'https://api.example.com/v1'"
    assert "me" not in message
    assert "corp.com" not in message
    assert "pa" not in message
    assert "ss" not in message


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


@pytest.mark.parametrize("char", ("/", "?", "#"))
def test_check_url_scheme_refusal_never_echoes_a_password_hidden_past_the_authority(
    char: str,
) -> None:
    """Issue #778: the parsed netloc for this is ``user:pa`` (the
    authority ends at ``char``), with no ``@`` of its own -- a plain
    authority-only cut leaves the rest of the password (``ss``)
    untouched.
    """
    url = f"ftp://user:pa{char}ss@host/x"
    with pytest.raises(ValueError) as exc_info:
        check_url(url)
    message = str(exc_info.value)
    assert message == (
        "unsupported URL scheme 'ftp' in 'ftp://host/x': only http or https is supported"
    )
    assert "user" not in message
    assert "pa" not in message
    assert "ss" not in message


@pytest.mark.parametrize("char", ("/", "?", "#"))
def test_check_url_control_character_refusal_never_echoes_a_password_hidden_past_the_authority(
    char: str,
) -> None:
    """Same hidden-password shape as the scheme refusal above, but for
    the control-character/whitespace check, which runs first (issue
    #778).
    """
    url = f"http://user:qq{char}rr@host\0/x"
    with pytest.raises(ValueError) as exc_info:
        check_url(url)
    message = str(exc_info.value)
    assert message.startswith("URL contains a control character or whitespace: ")
    assert "user" not in message
    assert "qq" not in message
    assert "rr" not in message


# ---------------------------------------------------------------------------
# strip_request_url_userinfo
# ---------------------------------------------------------------------------


def test_strip_request_url_userinfo_strips_userinfo_with_no_port() -> None:
    """Issue #778: CPython's own ``http.client`` raises a bare,
    credential-bearing ``InvalidURL("nonnumeric port: 'pass@host'")``
    when a plain ``urllib.request.Request`` is built from a URL like
    this -- the ``http`` tool and cyberdiner build their
    ``Request`` from this function's own result instead, so the
    userinfo is gone before urllib ever sees it.
    """
    assert strip_request_url_userinfo("http://user:pass@host/x") == "http://host/x"


def test_strip_request_url_userinfo_leaves_a_plain_url_unchanged() -> None:
    assert strip_request_url_userinfo("http://host/x") == "http://host/x"


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
    redacted = redact_url(f"http://alice:{password}@host:1234/x", "")
    assert redacted == "http://host:1234/x"
    assert "alice" not in redacted
    assert password not in redacted


def test_redact_request_url_masks_a_credential_query_param_behind_a_hidden_at_sign() -> None:
    """Issue #778: once the authority's own boundary hides a second
    ``@`` behind a ``:`` that can't be a port (so the whole value is
    cut at the LAST one, through :func:`redact_proxy_value`, rather
    than just the authority's own userinfo), the query string still
    carries its own credential-like key and must still be masked --
    the cut runs first, since masking first would re-encode the query
    string's own ``@`` as ``%40`` and the cut would no longer find it.
    """
    redacted = redact_request_url("https://alice:longsecret/x@api.example.com/v1?api_key=sk")
    assert redacted == "https://api.example.com/v1?api_key=%2A%2A%2A"
    assert "alice" not in redacted
    assert "sk" not in redacted


def test_redact_request_url_leaves_an_at_sign_past_a_real_host_port_alone() -> None:
    """Issue #819 follow-up: ``443`` is a valid port, so ``alice:443``
    reads as a real host:port, not a hidden credential -- the later
    ``@`` is left untouched, the same as ``http://localhost:8080/@alice``.
    The query's own credential-like key is still masked either way.
    """
    redacted = redact_request_url("https://alice:443/x@api.example.com/v1?api_key=sk")
    assert redacted == "https://alice:443/x@api.example.com/v1?api_key=%2A%2A%2A"


def test_redact_request_url_cuts_authority_userinfo_even_with_a_later_at_sign() -> None:
    """Issue #819 follow-up: the authority's own userinfo is cut first,
    whether or not another ``@`` also appears later in the value (a
    scoped-package-style path here).
    """
    url = "https://npmTOKEN123@registry.example.test/@acme/widget"
    assert redact_url(url, "***REDACTED***") == (
        "https://***REDACTED***@registry.example.test/@acme/widget"
    )
    assert redact_request_url(url) == "https://registry.example.test/@acme/widget"


def test_redact_request_url_cuts_at_the_last_at_sign_with_no_scheme_at_all() -> None:
    """Issue #778: a URL with no ``://`` prefix at all (the same shape
    as a schemeless proxy value) has no netloc for ``urlsplit`` to find
    any userinfo in, so :func:`_at_follows_netloc` is never true for
    it either -- it must still be cut the way
    :func:`redact_proxy_value` already cuts a schemeless proxy value.
    """
    assert redact_request_url("alice:pw@host/x") == "host/x"
    assert redact_request_url("alice:pw@localhost:11434") == "localhost:11434"


def test_redact_url_leaves_a_credential_free_url_byte_for_byte() -> None:
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
    ]:
        assert redact_url(url, "***REDACTED***") == url
        assert redact_request_url(url) == url


def test_redact_url_cuts_a_password_hidden_behind_a_slash_but_not_a_bare_at_sign() -> None:
    url = "http://alice:longsecret/x@127.0.0.1"
    assert redact_url(url, "***REDACTED***") == "http://***REDACTED***@127.0.0.1"
    assert redact_request_url(url) == "http://127.0.0.1"


def test_redact_url_leaves_a_real_port_before_a_later_at_sign_alone() -> None:
    """Issue #819 follow-up: ``8080`` is a valid port, so the later
    ``@`` (a scoped-path-style ``/@alice``) is left exactly as it is.
    """
    url = "http://localhost:8080/@alice"
    assert redact_url(url, "***REDACTED***") == url
    assert redact_request_url(url) == url


def test_redact_url_masks_a_query_key_matched_in_its_decoded_form() -> None:
    """Issue #819 follow-up: the key is matched percent-decoded
    (``token``), but only the value's raw text is replaced."""
    url = "https://host/x?tok%65n=abcd1234"
    assert redact_url(url, "***REDACTED***") == "https://host/x?tok%65n=***REDACTED***"


def test_redact_url_cuts_a_schemeless_userinfo_value() -> None:
    assert redact_url("alice:longsecret@host:1234", "***REDACTED***") == (
        "***REDACTED***@host:1234"
    )


def test_redact_url_cuts_a_schemeless_token_with_no_colon_at_all() -> None:
    """A bare token@host with no `:` is still a credential once *url* is
    already known to be a URL/proxy value -- only whitespace, never the
    `:` shape, gates the schemeless cut."""
    url = "mytoken@host.example.test/api"
    assert redact_url(url, "***REDACTED***") == "***REDACTED***@host.example.test/api"
    assert redact_url(url, "") == "host.example.test/api"


def test_redact_url_cuts_a_schemeless_value_even_with_trailing_whitespace() -> None:
    """Issue #819 follow-up: every caller already knows *url* is a URL
    -- whitespace is never an exception to the schemeless cut."""
    assert redact_url("alice:s3cret@proxy:3128 ", "") == "proxy:3128 "


def test_redact_url_masks_a_credential_query_key_among_other_keys() -> None:
    url = "https://host/x?foo=1&api_key=sk-test&bar=2"
    assert redact_url(url, "***REDACTED***") == (
        "https://host/x?foo=1&api_key=***REDACTED***&bar=2"
    )
    assert redact_request_url(url) == "https://host/x?foo=1&api_key=%2A%2A%2A&bar=2"


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


@pytest.mark.parametrize("char", ("@", ":"))
def test_check_proxy_url_accepts_a_password_containing_a_hostile_non_control_char(
    char: str,
) -> None:
    """Neither of these ends a proxy value's authority -- a proxy value
    has no meaningful path, so the port check must still find the real
    port after the password, not treat part of the password itself as
    an invalid port.
    """
    password = f"pa{char}ss"
    assert check_proxy_url(f"http://alice:{password}@proxy:3128") == (
        f"http://alice:{password}@proxy:3128"
    )


@pytest.mark.parametrize("char", ("/", "?", "#"))
def test_check_proxy_url_refuses_an_at_sign_after_the_authoritys_own_boundary(
    char: str,
) -> None:
    """Unlike ``@``/``:``, each of these *does* end a proxy value's
    authority (issue #778) -- a password containing one, followed later
    by an ``@``, would otherwise hide a second, real host past it
    (``http://alice:pa/ss@proxy:3128`` reads as authority ``alice:pa``,
    tail ``/ss@proxy:3128``), so this is refused outright, named with
    :func:`redact_proxy_value` and no port text.
    """
    password = f"pa{char}ss"
    with pytest.raises(ValueError, match="invalid proxy URL") as exc_info:
        check_proxy_url(f"http://alice:{password}@proxy:3128")
    message = str(exc_info.value)
    assert "alice" not in message
    assert password not in message


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
        # A common form (issue #778): a trailing `/` after an
        # explicit port still works like main, and a portless value
        # with one still gets this module's own default, the `/`
        # itself preserved rather than dropped.
        ("http://proxy.corp:3128/", "http://proxy.corp:3128/"),
        ("http://proxy.corp/", "http://proxy.corp:80/"),
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


def test_split_proxy_url_stops_the_authority_at_a_query_string_or_fragment() -> None:
    """The authority ends at the first ``/``, ``?`` or ``#``, not only
    ``/`` -- ``urlsplit``'s own netloc already reads it this way, the
    same boundary electricity's ``http.rs::parse_proxy`` now uses too
    (issue #778).
    """
    assert split_proxy_url("http://proxy.corp:3128?x").hostport == "proxy.corp:3128"
    assert split_proxy_url("http://proxy.corp:3128#x").hostport == "proxy.corp:3128"
