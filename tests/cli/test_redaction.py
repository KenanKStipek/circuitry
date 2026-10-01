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
