"""Tests for the shared plugin contract in circuitry.plugins.base."""

from __future__ import annotations

import json

import pytest

from circuitry.plugins.base import (
    ToolResult,
    _as_bool,
    http_error_excerpt,
    validate_tool_result,
)

# ---------------------------------------------------------------------------
# _as_bool — #262 part 2: bool("False") == True must not leak into params.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "value", ["false", "False", "FALSE", "0", "no", "No", "off", "Off", "n", "N", ""]
)
def test_as_bool_recognizes_false_strings(value: str) -> None:
    assert _as_bool(value) is False


@pytest.mark.parametrize("value", ["true", "True", "1", "yes", "anything-else"])
def test_as_bool_recognizes_true_strings(value: str) -> None:
    assert _as_bool(value) is True


def test_as_bool_none_returns_default() -> None:
    assert _as_bool(None) is False
    assert _as_bool(None, default=True) is True


def test_as_bool_passes_through_real_bool() -> None:
    assert _as_bool(True) is True
    assert _as_bool(False) is False


def test_as_bool_non_string_falls_back_to_python_bool() -> None:
    assert _as_bool(1) is True
    assert _as_bool(0) is False
    assert _as_bool([1]) is True
    assert _as_bool([]) is False


# ---------------------------------------------------------------------------
# ToolResult.ok
# ---------------------------------------------------------------------------


def test_tool_result_ok_defaults_to_true() -> None:
    assert ToolResult(value=None, raw={}).ok is True


def test_validate_tool_result_flags_non_bool_ok() -> None:
    bad = ToolResult(value=None, raw={})
    object.__setattr__(bad, "ok", "yes")  # bypass frozen-dataclass typing
    diagnostics = validate_tool_result(bad, plugin_name="fake")
    assert any("'ok'" in d for d in diagnostics)


# ---------------------------------------------------------------------------
# http_error_excerpt — #317: a bounded, redacted excerpt of an HTTP error
# body, without leaking a sibling credential-shaped field.
# ---------------------------------------------------------------------------


def test_http_error_excerpt_extracts_message_field() -> None:
    body = json.dumps({"message": "Validation failed: 'amount' is required"})
    assert http_error_excerpt(body) == "Validation failed: 'amount' is required"


def test_http_error_excerpt_no_reason_key_still_redacts_sibling_secret() -> None:
    """A GraphQL/validation-style body has no top-level error/message/detail
    field (the reason sits under its own key, e.g. `errors`), so the
    function falls back to the whole redacted body. Before the #324 fix it
    fell back to the *unredacted* raw text instead, because `redact()` on a
    bare string only matches when the whole string is itself a JWT/key/URL.
    """
    body = json.dumps(
        {
            "errors": [{"msg": "bad input"}],
            "api_key": "sk-canary-DO-NOT-LEAK-0123456789",
        }
    )
    excerpt = http_error_excerpt(body)
    assert "bad input" in excerpt
    assert "canary" not in excerpt
    assert "sk-canary-DO-NOT-LEAK-0123456789" not in excerpt


def test_http_error_excerpt_top_level_list_redacts_secret() -> None:
    body = json.dumps([{"msg": "bad input", "api_key": "sk-canary-DO-NOT-LEAK-0123456789"}])
    excerpt = http_error_excerpt(body)
    assert "bad input" in excerpt
    assert "canary" not in excerpt


def test_http_error_excerpt_non_json_body_falls_back_to_raw_text() -> None:
    assert http_error_excerpt("not found") == "not found"


def test_http_error_excerpt_empty_body_returns_empty_string() -> None:
    assert http_error_excerpt("") == ""
    assert http_error_excerpt("   ") == ""


def test_http_error_excerpt_scrub_secrets_runs_on_the_decoded_message() -> None:
    """#600 item 2: a raw-text scrub before this function can't catch a
    JSON-escaped secret; scrub_secrets runs after decoding too, when it is
    given."""
    body = '{"error": "bad token: sec\\/ret"}'

    def scrub(text: str) -> str:
        return text.replace("sec/ret", "[secret:api]")

    assert http_error_excerpt(body, scrub_secrets=scrub) == "bad token: [secret:api]"
    # Without it, the decoded message still carries the literal value.
    assert http_error_excerpt(body) == "bad token: sec/ret"


def test_http_error_excerpt_scrubs_a_message_crossing_the_character_cut() -> None:
    """#600 item 2: scrub_secrets runs on the whole decoded message before
    the 500-character cut, not after -- a secret starting just before the
    cut must not leave a fragment behind."""
    secret = "tok3n-straddling-the-cut-600-xyz"
    message = "x" * 490 + secret
    body = json.dumps({"error": message})

    def scrub(text: str) -> str:
        return text.replace(secret, "[secret:api]")

    excerpt = http_error_excerpt(body, scrub_secrets=scrub)
    # The placeholder itself can still be cut short by the 500-character
    # limit (it starts at character 490) -- what matters is that no part
    # of the raw secret survives, not that the whole placeholder does.
    assert "[secret:ap" in excerpt
    for start in range(len(secret) - 5):
        assert secret[start : start + 6] not in excerpt
