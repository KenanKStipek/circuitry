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
        # Multi-error tie-break shapes (orchestrator follow-up to PR #502's
        # review, finding 2): Python's own `jsonschema.exceptions.
        # by_relevance` compares the error's own *path* element-by-element
        # as a real, second tie-break component -- not just path *length*
        # -- so two same-depth violations don't necessarily resolve to
        # "whichever iter_errors yielded first" the way an earlier revision
        # of this corpus (and of `electricity-schema`'s own engine) assumed.
        _case(
            "multiple_of_under_ref",
            {
                "$ref": "#/$defs/positiveMultiple",
                "$defs": {"positiveMultiple": {"type": "integer", "multipleOf": 3}},
            },
            7,
        ),
        _case(
            "different_depths_shallower_wins",
            {"type": "object", "required": ["a"], "properties": {"b": {"type": "string"}}},
            {"b": 5},
        ),
        _case(
            "required_list_authored_order_three_entries",
            {"type": "object", "required": ["z", "m", "a"]},
            {},
        ),
        _case(
            "pattern_properties_descent_type_mismatch",
            {"type": "object", "patternProperties": {"^b": {"type": "string"}}},
            {"bx": 5},
        ),
        _case(
            "pattern_properties_vs_properties_path_tie",
            {
                "type": "object",
                "properties": {"a": {"type": "string"}},
                "patternProperties": {"^b": {"type": "string"}},
            },
            {"a": 1, "bx": 2},
        ),
        _case(
            "one_of_multiple_valid_first_match_goes_last",
            {"oneOf": [{"type": "number"}, {"minimum": 0}]},
            5,
        ),
        # Every schema/instance pair the orchestrator's own
        # probe-schema/probe-schema2/probe-schema3 scripts exercised,
        # transcribed verbatim (including the ones that already matched at
        # review time, for regression coverage against a future change --
        # not just the ones that diverged).
        _case('probe_schema_c0', {'type': 'object', 'properties': {'a': {'type': 'string'}}, 'required': ['a']}, {'b': 1}),
        _case('probe_schema_c1', {'type': 'object', 'properties': {'a': {'type': 'string'}}, 'required': ['a']}, {'a': 5}),
        _case('probe_schema_c2', {'type': 'object', 'properties': {'a': {'type': 'string'}}, 'required': ['a', 'b']}, {}),
        _case('probe_schema_c3', {'type': 'array', 'items': {'type': 'string'}, 'minItems': 2}, ['x']),
        _case('probe_schema_c4', {'type': 'array', 'items': {'type': 'string'}, 'minItems': 1}, []),
        _case('probe_schema_c5', {'type': 'object', 'additionalProperties': False, 'properties': {'a': {}}}, {'a': 1, 'z': 2}),
        _case('probe_schema_c6', {'type': 'object', 'additionalProperties': False, 'properties': {'a': {}}}, {'a': 1, 'z': 2, 'y': 3}),
        _case('probe_schema_c7', {'enum': ['red', 'green']}, 'blue'),
        _case('probe_schema_c8', {'type': 'number'}, 'nine'),
        _case('probe_schema_c9', {'type': 'object'}, ['list']),
        _case('probe_schema_c10', {'type': 'string', 'maxLength': 3}, 'abcdef'),
        _case('probe_schema_c11', {'type': 'integer', 'minimum': 5}, 3),
        _case('probe_schema_c12', {'type': 'object', 'properties': {'title': {'type': 'string'}, 'score': {'type': 'number'}}, 'required': ['title', 'score']}, {'title': 'x', 'score': 'high'}),
        _case('probe_schema_c13', {'type': 'object', 'properties': {'items': {'type': 'array', 'items': {'type': 'object', 'properties': {'n': {'type': 'integer'}}, 'required': ['n']}}}, 'required': ['items']}, {'items': [{'n': 1}, {'m': 2}]}),
        _case('probe_schema2_c0', {'type': 'object', 'properties': {'a': {'type': 'object', 'properties': {'b': {'type': 'string'}}}}, 'required': ['a', 'z']}, {'a': {'b': 1}}),
        _case('probe_schema2_c1', {'type': 'object', 'properties': {'n': {'type': 'integer'}}, 'required': ['x', 'y']}, {'n': 's'}),
        _case('probe_schema2_c2', {'type': 'integer'}, 'it\'s "quoted"'),
        _case('probe_schema2_c3', {'type': 'integer'}, 'naïve ✓ \\ tab\t'),
        _case('probe_schema2_c4', {'enum': ["a'b", 'c"d']}, 'x'),
        _case('probe_schema2_c5', {'type': 'string'}, 1.5),
        _case('probe_schema2_c6', {'type': 'string'}, 1e+20),
        _case('probe_schema2_c7', {'type': 'string'}, True),
        _case('probe_schema2_c8', {'type': 'string'}, None),
        _case('probe_schema2_c9', {'type': 'string'}, {'k': [1, 2.0, False, None]}),
        _case('probe_schema2_c10', {'type': 'number'}, [1, 'two']),
        _case('probe_schema2_c11', {'type': 'number', 'maximum': 10}, 11.5),
        _case('probe_schema2_c12', {'type': 'number', 'exclusiveMinimum': 3}, 3),
        _case('probe_schema2_c13', {'type': 'number', 'exclusiveMaximum': 3}, 3),
        _case('probe_schema2_c14', {'type': 'integer', 'multipleOf': 3}, 7),
        _case('probe_schema2_c15', {'type': 'number', 'multipleOf': 0.5}, 0.3),
        _case('probe_schema2_c16', {'type': 'string', 'minLength': 3}, 'ab'),
        _case('probe_schema2_c17', {'type': 'string', 'minLength': 1}, ''),
        _case('probe_schema2_c18', {'type': 'string', 'pattern': '^[a-z]+$'}, 'ABC'),
        _case('probe_schema2_c19', {'type': 'array', 'maxItems': 1}, [1, 2]),
        _case('probe_schema2_c20', {'type': 'array', 'maxItems': 0}, [1]),
        _case('probe_schema2_c21', {'type': 'array', 'uniqueItems': True}, [1, 1]),
        _case('probe_schema2_c22', {'type': 'array', 'items': {'type': 'integer'}}, [1, 'x', 3]),
        _case('probe_schema2_c23', {'const': 'fixed'}, 'other'),
        _case('probe_schema2_c24', {'type': 'object', 'properties': {'a': {}}, 'additionalProperties': False}, {'b': 1, 'a': 2, 'c': 3}),
        _case('probe_schema2_c25', {'type': 'object', 'minProperties': 2}, {'a': 1}),
        _case('probe_schema2_c26', {'type': 'object', 'properties': {'shots': {'type': 'array', 'items': {'type': 'object', 'properties': {'prompt': {'type': 'string'}, 'duration': {'type': 'number'}}, 'required': ['prompt', 'duration']}}}, 'required': ['shots']}, {'shots': [{'prompt': 'x', 'duration': '5'}]}),
        _case('probe_schema2_c27', {'type': 'object', 'properties': {'title': {'type': 'string'}, 'tags': {'type': 'array', 'items': {'type': 'string'}, 'minItems': 1}}, 'required': ['title', 'tags']}, {'title': 't', 'tags': []}),
        _case('probe_schema2_c28', {'anyOf': [{'type': 'string'}, {'type': 'integer'}]}, 1.5),
        _case('probe_schema2_c29', {'oneOf': [{'type': 'string'}, {'type': 'number'}]}, None),
        _case('probe_schema3_c0', {'type': 'object', 'properties': {'title': {'type': 'string'}, 'score': {'type': 'number'}}}, {'title': 1, 'score': 'high'}),
        _case('probe_schema3_c1', {'type': 'array', 'items': {'type': 'string'}}, [1, 2]),
        _case('probe_schema3_c2', {'type': 'object', 'properties': {'x': {}}, 'required': ['x'], 'additionalProperties': False}, {'y': 1}),
        _case('probe_schema3_c3', {'type': 'integer', 'minimum': 5}, 3.5),
        _case('probe_schema3_c4', {'type': 'array', 'maxItems': 1}, [{'b': 1, 'a': 2}, {'b': 3, 'a': 4}]),
        _case('probe_schema3_c5', {'type': ['string', 'null']}, 5),
        _case('probe_schema3_c6', {'type': 'object', 'properties': {'b': {'type': 'string'}, 'a': {'type': 'string'}}, 'required': ['b', 'a']}, {}),
        _case('probe_schema3_c7', {'type': 'object', 'properties': {'z': {'type': 'integer'}, 'a': {'type': 'integer'}}}, {'z': 's', 'a': 't'}),
        _case('probe_schema3_c8', {'type': 'object', 'patternProperties': {'^a': {}}, 'additionalProperties': False}, {'zz': 1}),
        _case('probe_schema3_c9', {'enum': [{'b': 1, 'a': 2}]}, {'c': 3}),
        _case('probe_schema3_c10', {'type': 'integer'}, 1180591620717411303424),
        _case('probe_schema3_c11', {'type': 'array', 'items': {'type': 'object', 'properties': {'n': {'type': 'integer'}, 'm': {'type': 'string'}}, 'required': ['n']}}, [{'m': 1}, {'n': 'x'}]),
    ]


def render(cases_: list[dict]) -> str:
    # No `sort_keys` -- unlike `generate_schema_corpus.py`'s own corpus
    # (location/verdict only, never key order), several cases here exist
    # specifically to pin a *tie-break* that depends on the schema's own
    # authored key order (`iter_errors`' own iteration order, `_case`'s
    # own dict/list literals as written above); sorting would destroy
    # exactly what those cases test. Still fully deterministic: a Python
    # `dict`'s own iteration order is its insertion order, always.
    return json.dumps(cases_, indent=2, ensure_ascii=False) + "\n"


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
