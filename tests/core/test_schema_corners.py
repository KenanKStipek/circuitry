"""`uniqueItems`/`multipleOf`/`$ref`-chain corners (issue #778, step 5)."""

from __future__ import annotations

import math

import pytest

from circuitry.core.schema_corners import (
    MAX_REF_CHAIN_HOPS,
    MULTIPLE_OF_ERROR,
    _has_duplicate,
    ref_chain_depth_exceeds_cap,
    schema_well_formedness_error,
)


@pytest.mark.parametrize(
    ("items", "expected"),
    [
        ([1, 2, 1], True),
        ([1, 1.0], True),
        ([1, True], False),
        ([True, False], False),
        ([float("nan"), float("nan")], False),
        ([float("nan")], False),
        ([{"a": 1, "b": 2}, {"b": 2, "a": 1}], True),
        ([{"a": 1, "b": 2}, {"a": 1, "b": 3}], False),
        ([[1], [True]], False),
        ([[1], [1]], True),
        # jsonschema's own `_utils.uniq` sorts then compares only adjacent
        # pairs; with a value its own `<` can't order sitting between two
        # equal ones, the old algorithm misses the duplicate it would find
        # if those two were adjacent instead.
        ([0, 1, True, 1.0], True),
        ([], False),
        ([1], False),
    ],
)
def test_has_duplicate_compares_every_pair(items: list, expected: bool) -> None:
    assert _has_duplicate(items) == expected


@pytest.mark.parametrize(
    "multiple_of",
    [0, -1, -0.5, "nope", True, False, float("nan"), float("inf"), float("-inf")],
)
def test_multiple_of_must_be_a_positive_number(multiple_of: object) -> None:
    assert schema_well_formedness_error({"multipleOf": multiple_of}) == MULTIPLE_OF_ERROR


@pytest.mark.parametrize("multiple_of", [1, 2, 0.5, 3.14])
def test_multiple_of_positive_number_is_well_formed(multiple_of: object) -> None:
    assert schema_well_formedness_error({"multipleOf": multiple_of}) is None


def test_multiple_of_is_found_nested() -> None:
    schema = {"properties": {"a": {"type": "integer", "multipleOf": 0}}}
    assert schema_well_formedness_error(schema) == MULTIPLE_OF_ERROR


def test_ref_chain_1000_hops_exceeds_the_cap() -> None:
    defs = {}
    for i in range(1000):
        defs[f"L{i}"] = {"$ref": f"#/$defs/L{i + 1}"} if i + 1 < 1000 else {"type": "object"}
    schema = {"$defs": defs, "$ref": "#/$defs/L0"}
    assert ref_chain_depth_exceeds_cap(schema)
    assert schema_well_formedness_error(schema) != MULTIPLE_OF_ERROR
    assert f"limit {MAX_REF_CHAIN_HOPS}" in (schema_well_formedness_error(schema) or "")


def test_ref_chain_within_the_cap_is_well_formed() -> None:
    defs = {}
    hops = MAX_REF_CHAIN_HOPS - 1
    for i in range(hops):
        defs[f"L{i}"] = {"$ref": f"#/$defs/L{i + 1}"} if i + 1 < hops else {"type": "object"}
    schema = {"$defs": defs, "$ref": "#/$defs/L0"}
    assert not ref_chain_depth_exceeds_cap(schema)
    assert schema_well_formedness_error(schema) is None


def test_two_schema_ref_cycle_exceeds_the_cap() -> None:
    schema = {
        "$defs": {"A": {"$ref": "#/$defs/B"}, "B": {"$ref": "#/$defs/A"}},
        "$ref": "#/$defs/A",
    }
    assert ref_chain_depth_exceeds_cap(schema)
    assert schema_well_formedness_error(schema) is not None


def test_recursive_tree_schema_is_unaffected_by_the_ref_chain_cap() -> None:
    schema = {
        "type": "object",
        "properties": {
            "children": {"type": "array", "items": {"$ref": "#"}},
        },
    }
    assert not ref_chain_depth_exceeds_cap(schema)
    assert schema_well_formedness_error(schema) is None


def test_other_malformed_schema_still_reported() -> None:
    error = schema_well_formedness_error({"type": 5})
    assert error is not None
    assert error != MULTIPLE_OF_ERROR


def test_nan_is_not_finite_sanity() -> None:
    assert not math.isfinite(float("nan"))
