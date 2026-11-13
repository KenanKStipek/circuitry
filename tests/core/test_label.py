"""`clean_label`/`render_document_label` (#752) — the cleaning rules and
the render-failure-warns-and-continues contract both engines share."""

from __future__ import annotations

import pytest

from circuitry.core.label import clean_label, render_document_label

# One table, shared in spirit with electricity's own Rust unit tests for
# `clean_label` — same inputs, same expected outputs, ported byte for byte.
_CLEAN_CASES = [
    ("plain", "hello world", "hello world"),
    ("newlines", "a\n\nb", "a b"),
    ("tabs", "a\t\tb", "a b"),
    ("ansi colour", "a\x1b[31mred\x1b[0mb", "aredb"),
    ("osc title (BEL-terminated)", "a\x1b]0;title\x07b", "ab"),
    ("osc title (ST-terminated)", "a\x1b]0;title\x1b\\b", "ab"),
    ("bel", "a\x07b", "ab"),
    ("c1 control (NEL)", "a\x85b", "ab"),
    ("combining marks preserved", "éclair", "éclair"),
    ("only whitespace", "   \t\n  ", ""),
    ("leading/trailing whitespace trimmed", "  hi  ", "hi"),
    ("cut lands on a collapsed space", "x" * 63 + " y", "x" * 63),
]


@pytest.mark.parametrize(("name", "raw", "expected"), _CLEAN_CASES, ids=[c[0] for c in _CLEAN_CASES])
def test_clean_label_table(name: str, raw: str, expected: str) -> None:
    assert clean_label(raw) == expected


def test_clean_label_emoji_at_the_cut_is_not_split() -> None:
    text = "x" * 63 + "\U0001f389" + "y"
    cleaned = clean_label(text)
    assert cleaned == "x" * 63 + "\U0001f389"
    assert len(cleaned) == 64


def test_clean_label_exactly_64_is_unchanged() -> None:
    text = "x" * 64
    assert clean_label(text) == text
    assert len(clean_label(text)) == 64


def test_clean_label_65_is_cut_to_64() -> None:
    text = "x" * 65
    cleaned = clean_label(text)
    assert cleaned == "x" * 64
    assert len(cleaned) == 64


def test_render_document_label_renders_against_input_only() -> None:
    label, warning = render_document_label("ticket {{input.ticket}}", {"input": {"ticket": "42"}})
    assert label == "ticket 42"
    assert warning is None


def test_render_document_label_missing_input_renders_empty_and_no_label() -> None:
    label, warning = render_document_label("{{input.ticket}}", {"input": {}})
    assert label is None
    assert warning is None


def test_render_document_label_not_a_string_is_no_label_no_warning() -> None:
    label, warning = render_document_label(None, {"input": {}})
    assert label is None
    assert warning is None

    label, warning = render_document_label("", {"input": {}})
    assert label is None
    assert warning is None


def test_render_document_label_malformed_template_warns_once_no_label() -> None:
    label, warning = render_document_label("{{#open", {"input": {}})
    assert label is None
    assert warning is not None
    assert "label" in warning
    assert "malformed Mustache template" in warning
    assert warning.endswith("the run continues without a label.")


def test_render_document_label_cleans_the_result() -> None:
    label, warning = render_document_label(
        "  {{input.a}}\t{{input.b}}  ", {"input": {"a": "x", "b": "y"}}
    )
    assert label == "x y"
    assert warning is None


def test_render_document_label_does_not_html_escape() -> None:
    label, warning = render_document_label(
        "{{input.raw}}", {"input": {"raw": "R&D \"fix\" <x> 'y'"}}
    )
    assert label == "R&D \"fix\" <x> 'y'"
    assert warning is None
