#!/usr/bin/env python3
"""Generate electricity-schema's `validate_prompt_schema` golden corpus from
Python's own ``jsonschema.validate`` (issue #498 ruling 6, tightened by PR
#502's review finding 2/orchestrator ruling 2).

``core/prompt.py::PromptRuntime._validate_schema`` calls
``jsonschema.validate(value, schema)``, which raises
``jsonschema.exceptions.best_match(validator.iter_errors(instance))`` on a
mismatch -- so this corpus records, for each hand-written schema/instance
pair, exactly the ``.message`` that call raises (or ``None`` for a pair that
validates). The Rust side (``electricity-schema``'s own
``validate_prompt_schema``) replays every pair and asserts its own message
is identical.

Must be run with ``jsonschema`` installed in the active interpreter (the
lane venv locally; CI pins the version in ``generator-constraints.txt``, so
a fresh install there is this same version). Usage:
``python3 generate_prompt_schema_corpus.py [--check]``
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

import jsonschema  # type: ignore[import-untyped]

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-schema"
    / "tests"
    / "golden"
    / "prompt_schema_corpus.json"
)


def _case(name: str, schema: dict, instance: Any) -> dict:
    try:
        jsonschema.validate(instance, schema)
        message = None
    except jsonschema.ValidationError as e:
        message = e.message
    return {"name": name, "schema": schema, "instance": instance, "message": message}


def cases() -> list[dict]:
    return [
        _case(
            "required_missing_one",
            {"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]},
            {"b": 1},
        ),
        _case(
            "required_nested_in_array",
            {
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {"n": {"type": "integer"}},
                            "required": ["n"],
                        },
                    }
                },
                "required": ["items"],
            },
            {"items": [{"n": 1}, {"m": 2}]},
        ),
        _case(
            "type_scalar_string_expected",
            {"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]},
            {"a": 5},
        ),
        _case("type_object_expected", {"type": "object"}, ["list"]),
        _case("type_number_expected", {"type": "number"}, "nine"),
        _case(
            "type_nested_under_property",
            {
                "type": "object",
                "properties": {
                    "title": {"type": "string"},
                    "score": {"type": "number"},
                },
                "required": ["title", "score"],
            },
            {"title": "x", "score": "high"},
        ),
        _case("type_boolean_expected", {"type": "boolean"}, "yes"),
        _case("type_null_expected", {"type": "null"}, 0),
        _case("type_integer_expected", {"type": "integer"}, 3.5),
        _case("enum_string_mismatch", {"enum": ["red", "green"]}, "blue"),
        _case("enum_number_mismatch", {"enum": [1, 2, 3]}, 4),
        _case("const_mismatch", {"const": "fixed"}, "other"),
        _case(
            "additional_properties_one_extra",
            {
                "type": "object",
                "properties": {"a": {}},
                "additionalProperties": False,
            },
            {"a": 1, "z": 2},
        ),
        _case(
            "additional_properties_two_extra_sorted",
            {
                "type": "object",
                "properties": {"a": {}},
                "additionalProperties": False,
            },
            {"a": 1, "z": 2, "y": 3},
        ),
        _case("min_items_zero_of_one", {"type": "array", "minItems": 1}, []),
        _case("min_items_one_of_two", {"type": "array", "minItems": 2}, ["x"]),
        _case("max_items_one_over_zero", {"type": "array", "maxItems": 0}, ["x"]),
        _case("max_items_one_over_two", {"type": "array", "maxItems": 2}, ["x", "y", "z"]),
        _case("min_length_empty_of_one", {"type": "string", "minLength": 1}, ""),
        _case("min_length_short_of_three", {"type": "string", "minLength": 3}, "ab"),
        _case("max_length_one_over_zero", {"type": "string", "maxLength": 0}, "a"),
        _case("max_length_over_three", {"type": "string", "maxLength": 3}, "abcdef"),
        _case("minimum_below", {"type": "integer", "minimum": 5}, 3),
        _case("maximum_above", {"type": "integer", "maximum": 5}, 9),
        _case("exclusive_minimum_equal", {"type": "number", "exclusiveMinimum": 5}, 5),
        _case("exclusive_maximum_equal", {"type": "number", "exclusiveMaximum": 5}, 5),
        _case("multiple_of_int_divisor", {"type": "integer", "multipleOf": 2}, 7),
        _case("multiple_of_float_divisor", {"type": "number", "multipleOf": 0.5}, 0.3),
        _case("pattern_mismatch", {"type": "string", "pattern": "^[a-z]+$"}, "ABC"),
        _case("unique_items_has_duplicate", {"type": "array", "uniqueItems": True}, [1, 2, 1]),
        _case(
            "string_with_embedded_single_quote",
            {"enum": ["red", "green"]},
            "it's blue",
        ),
        _case(
            "non_ascii_string_mismatch",
            {"enum": ["caf\u00e9"]},
            "th\u00e9",
        ),
        _case("float_instance_type_mismatch", {"type": "string"}, 1.5),
        _case("bool_instance_type_mismatch", {"type": "string"}, True),
        _case("none_instance_required_value", {"type": "object"}, None),
        _case(
            "valid_instance_no_error",
            {"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]},
            {"a": "ok"},
        ),
    ]


def render(cases_: list[dict]) -> str:
    return json.dumps(cases_, indent=2, ensure_ascii=False, sort_keys=True) + "\n"


def main() -> int:
    text = render(cases())
    if "--check" in sys.argv[1:]:
        current = OUTPUT.read_text() if OUTPUT.exists() else ""
        if current != text:
            print(f"{OUTPUT} is stale; run without --check to regenerate", file=sys.stderr)
            return 1
        return 0
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text(text)
    print(f"wrote {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
