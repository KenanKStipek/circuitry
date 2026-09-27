"""Unit tests for the shared lenient answer parser (issue #247)."""

from __future__ import annotations

import pytest

from circuitry.core.answers import (
    AnswerParseError,
    parse_boolean_answer,
    parse_number_answer,
)


@pytest.mark.parametrize(
    "text",
    [
        "Yes.",
        "yes, because it's cheaper",
        "**TRUE**",
        "Y",
        "y",
        '"yes"',
        "**Yes**",
        "true",
        "1",
    ],
)
def test_parse_boolean_answer_true_cases(text: str) -> None:
    assert parse_boolean_answer(text) is True


@pytest.mark.parametrize(
    "text",
    [
        "No.",
        "false!",
        "n",
        "0",
        "**FALSE**",
        "no, that's wrong",
    ],
)
def test_parse_boolean_answer_false_cases(text: str) -> None:
    assert parse_boolean_answer(text) is False


@pytest.mark.parametrize("text", ["maybe", "", "   ", "unclear"])
def test_parse_boolean_answer_raises_on_unparseable(text: str) -> None:
    with pytest.raises(AnswerParseError) as excinfo:
        parse_boolean_answer(text)
    assert excinfo.value.raw_response_text == text
    assert text.strip() in str(excinfo.value) or text == ""


@pytest.mark.parametrize(
    "text,expected",
    [
        ("42", 42),
        ("42.", 42),
        ("3.5", 3.5),
        ("-1", -1),
        ("1e3", 1000.0),
        ("  7  ", 7),
    ],
)
def test_parse_number_answer_valid_cases(text: str, expected: int | float) -> None:
    got = parse_number_answer(text)
    assert got == expected
    assert type(got) is type(expected)


@pytest.mark.parametrize("text", ["about 42", "42 degrees", "maybe", "", "forty-two"])
def test_parse_number_answer_raises_rather_than_guessing(text: str) -> None:
    with pytest.raises(AnswerParseError) as excinfo:
        parse_number_answer(text)
    assert excinfo.value.raw_response_text == text
