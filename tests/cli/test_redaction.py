"""Unit tests for circuitry.cli.redaction.redact."""

from __future__ import annotations

from circuitry.cli.redaction import REDACTED, redact


def test_redacts_api_key_field() -> None:
    assert redact({"api_key": "sk-abcdefghijklmnopqrstuvwxyz0123"}) == {
        "api_key": REDACTED
    }


def test_redacts_authorization_header() -> None:
    assert redact({"Authorization": "Bearer sometoken"}) == {"Authorization": REDACTED}


def test_redacts_set_cookie_header() -> None:
    """#238/#262 part 1 raw: headers echoed into meta.raw must be redacted
    by key, not just by value shape \u2014 Set-Cookie carries a session token
    that doesn't necessarily look like an API key or JWT."""
    assert redact({"Set-Cookie": "sessionid=abc123; HttpOnly"}) == {
        "Set-Cookie": REDACTED
    }
    assert redact({"set-cookie": "sessionid=abc123"}) == {"set-cookie": REDACTED}
    assert redact({"Cookie": "sessionid=abc123"}) == {"Cookie": REDACTED}


def test_does_not_redact_unrelated_keys() -> None:
    assert redact({"url": "https://example.test/x", "count": 3}) == {
        "url": "https://example.test/x",
        "count": 3,
    }


def test_redacts_userinfo_in_url_strings() -> None:
    out = redact({"base_url": "https://user:pass@example.test/x"})
    assert "pass" not in out["base_url"]
    assert out["base_url"].startswith(f"https://{REDACTED}@")


def test_walks_nested_dicts_and_lists() -> None:
    out = redact({"headers": [{"Authorization": "secretvalue"}]})
    assert out["headers"][0]["Authorization"] == REDACTED


def test_redacts_jwt_shaped_string_value_regardless_of_key() -> None:
    jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dQw4w9WgXcQ"
    assert redact({"note": jwt}) == {"note": REDACTED}


def test_does_not_redact_plain_dotted_names_shaped_like_three_segments() -> None:
    """A three-segment dotted string that doesn't start with a JWT header's
    eyJ prefix is a plain name (hostname, filename, version), not a JWT."""
    assert redact({"note": "www.example.com"}) == {"note": "www.example.com"}
    assert redact({"note": "a.tar.gz"}) == {"note": "a.tar.gz"}
    assert redact({"note": "1.2.3"}) == {"note": "1.2.3"}


def test_redacts_a_schemeless_base_url_without_any_double_slash() -> None:
    """#819: a base_url with no :// at all still has its credential cut."""
    out = redact({"base_url": "alice:hunter2hunter2@host:1234"})
    assert "hunter2" not in out["base_url"]
    assert out["base_url"] == f"{REDACTED}@host:1234"


def test_redacts_a_schemeless_token_used_as_a_username_with_no_colon_at_all() -> None:
    """A bare token@host (no `:` at all) is still a credential once the
    value is under a URL key."""
    out = redact({"base_url": "mytoken@host.example.test/api"})
    assert "mytoken" not in out["base_url"]
    assert out["base_url"] == f"{REDACTED}@host.example.test/api"


def test_redacts_a_credential_hidden_behind_the_path() -> None:
    """#819: an @ past the parsed authority's own boundary still hides a
    password — the whole value is cut at the LAST @, not just the netloc's
    own userinfo."""
    out = redact({"base_url": "https://alice:443/x@api.example.test/v1"})
    assert "alice" not in out["base_url"]
    assert out["base_url"] == f"https://{REDACTED}@api.example.test/v1"


def test_redacts_a_credential_like_query_param_with_no_userinfo_at_all() -> None:
    """#819: a base_url with a credential-like query key but no @ still
    leaked its secret before this redactor masked query params too."""
    out = redact({"base_url": "https://api.example.test/search?api_key=hunter2hunter2"})
    assert "hunter2" not in out["base_url"]
    assert out["base_url"] == f"https://api.example.test/search?api_key={REDACTED}"


def test_does_not_mangle_an_email_address() -> None:
    """An @ with no colon before it (an email address, not a
    user:pass@host credential) is left alone."""
    out = redact({"to": ["alice@example.test"]})
    assert out == {"to": ["alice@example.test"]}


def test_does_not_mangle_a_sentence_containing_a_question_mark_link() -> None:
    """Prose that merely contains a URL (with whitespace around it) is
    never treated as a single URL value: it must not be re-encoded as
    a query string."""
    value = '<a href="https://x.test/?a=1">x</a> more'
    assert redact({"note": value}) == {"note": value}


def test_does_not_mangle_a_multi_line_body_containing_a_question_mark() -> None:
    value = "line one\nis this a question?\nline three"
    assert redact({"note": value}) == {"note": value}


def test_does_not_mangle_a_vertex_style_model_id() -> None:
    """A model id with an @ and no colon before it (e.g. a pinned Vertex
    model version) is not a user:pass@host credential."""
    value = "claude-3-5-sonnet@20240620"
    assert redact({"model": value}) == {"model": value}


def test_does_not_mangle_a_list_of_email_addresses() -> None:
    value = ["alice@example.test", "bob@example.test"]
    assert redact({"to": value}) == {"to": value}


def test_a_question_mark_inside_the_fragment_is_never_treated_as_a_query() -> None:
    """`urlsplit` finds the fragment (the first #) before it looks for a
    query -- a ? that comes after it is just fragment text, never a
    query delimiter, so it must never be re-encoded as a masked query."""
    value = "https://host/path#frag?a=1"
    assert redact({"url": value}) == {"url": value}
