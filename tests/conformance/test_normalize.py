"""Unit tests for the shared normalizer (electricity/DESIGN.md §12)."""

from __future__ import annotations

import pytest

from .normalize import (
    NormalizationError,
    assert_errors_equal,
    assert_out_serialization,
    assert_states_equal,
    assert_values_equal,
    normalize,
    redact_leaked_paths,
)


def test_normalize_leaves_non_volatile_fields_untouched() -> None:
    value = {"name": "step", "count": 3, "flag": True, "nothing": None}
    assert normalize(value) == value


def test_normalize_timestamp_iso_fields() -> None:
    value = {"created_at": "2026-10-07T20:24:55.787572+00:00"}
    assert normalize(value) == {"created_at": "<TIMESTAMP>"}


def test_normalize_rejects_malformed_timestamp() -> None:
    with pytest.raises(NormalizationError):
        normalize({"created_at": "not-a-timestamp"})


def test_normalize_compact_timestamp_field() -> None:
    assert normalize({"_timestamp": "20261007_202455"}) == {"_timestamp": "<TIMESTAMP>"}


def test_normalize_rejects_malformed_compact_timestamp() -> None:
    with pytest.raises(NormalizationError):
        normalize({"_timestamp": "2026-10-07"})


def test_normalize_run_id_and_uuid_fields() -> None:
    value = {"run_id": "a9e38161-fa90-4405-99d9-3ca60a5aca64"}
    assert normalize(value) == {"run_id": "<UUID>"}


def test_normalize_rejects_malformed_uuid() -> None:
    with pytest.raises(NormalizationError):
        normalize({"run_id": "not-a-uuid"})


def test_normalize_duration_fields() -> None:
    assert normalize({"wall_time_s": 0.0130892}) == {"wall_time_s": "<DURATION>"}
    assert normalize({"eta_s": 0}) == {"eta_s": "<DURATION>"}


def test_normalize_rejects_negative_duration() -> None:
    with pytest.raises(NormalizationError):
        normalize({"wall_time_s": -1})


def test_normalize_path_basename_field() -> None:
    value = {"runtime": {"last_run": {"orchestration_path": "/a/b/c/orchestration.yml"}}}
    assert normalize(value) == {
        "runtime": {"last_run": {"orchestration_path": "orchestration.yml"}}
    }


def test_normalize_path_ignore_fields() -> None:
    value = {
        "runtime": {
            "effective_settings": {
                "out": "/tmp/anything/out.json",
                "runtime": {"_orchestration_dir": "/tmp/anything"},
            }
        }
    }
    assert normalize(value) == {
        "runtime": {
            "effective_settings": {
                "out": "<out>",
                "runtime": {"_orchestration_dir": "<case>"},
            }
        }
    }


def test_normalize_does_not_touch_an_out_field_outside_its_known_location() -> None:
    # effective_settings.sources.out is a provenance tag ("cli"/"config"/...),
    # not a path — only the exact-location rule may rewrite a field named
    # `out`, never a name match alone.
    value = {"runtime": {"effective_settings": {"sources": {"out": "cli"}}}}
    assert normalize(value) == value


def test_redact_leaked_paths_only_touches_known_path_locations() -> None:
    value = {
        "runtime": {
            "effective_settings": {
                "out": "/tmp/anything/out.json",
                "runtime": {"_orchestration_dir": "/tmp/anything"},
                "sources": {"out": "cli"},
            },
            "last_run": {"orchestration_path": "orchestration.yml", "run_id": "keep-me"},
        }
    }
    redacted = redact_leaked_paths(value)
    assert redacted["runtime"]["effective_settings"]["out"] == "<out>"
    assert redacted["runtime"]["effective_settings"]["runtime"]["_orchestration_dir"] == "<case>"
    assert redacted["runtime"]["effective_settings"]["sources"]["out"] == "cli"
    assert redacted["runtime"]["last_run"]["orchestration_path"] == "orchestration.yml"
    assert redacted["runtime"]["last_run"]["run_id"] == "keep-me"


def test_normalize_recurses_into_nested_structures() -> None:
    value = {"runtime": {"last_run": {"run_id": "a9e38161-fa90-4405-99d9-3ca60a5aca64"}}}
    assert normalize(value) == {"runtime": {"last_run": {"run_id": "<UUID>"}}}


def test_normalize_recurses_into_lists() -> None:
    value = [{"created_at": "2026-10-07T20:24:55.787572+00:00"}, {"other": 1}]
    assert normalize(value) == [{"created_at": "<TIMESTAMP>"}, {"other": 1}]


def test_assert_states_equal_is_order_sensitive() -> None:
    a = {"x": 1, "y": 2}
    b = {"y": 2, "x": 1}
    with pytest.raises(AssertionError, match="key order"):
        assert_states_equal(a, b)


def test_assert_states_equal_passes_on_identical_order() -> None:
    assert_states_equal({"x": 1, "y": 2}, {"x": 1, "y": 2})


def test_assert_values_equal_ignores_order() -> None:
    assert_values_equal({"x": 1, "y": 2}, {"y": 2, "x": 1})


def test_assert_values_equal_still_catches_real_differences() -> None:
    with pytest.raises(AssertionError):
        assert_values_equal({"x": 1}, {"x": 2})


def test_circuitry_own_errors_compared_byte_for_byte() -> None:
    assert_errors_equal("boom", "boom", byte_for_byte=True)
    with pytest.raises(AssertionError, match="error text differs"):
        assert_errors_equal("boom", "boom!", byte_for_byte=True)


def test_third_party_errors_compared_loosely_not_byte_for_byte() -> None:
    # Different wording, same failure location — must pass under
    # byte_for_byte=False even though the two strings are not equal.
    assert_errors_equal(
        "effects[0].name: 'value' should not be valid under {'pattern': '...'}",
        "effects[0].name: 'value' is reserved per schema XYZ",
        byte_for_byte=False,
        location_pattern=r"effects\[\d+\]\.name",
    )


def test_third_party_errors_require_a_non_empty_message() -> None:
    with pytest.raises(AssertionError, match="empty"):
        assert_errors_equal("", "something", byte_for_byte=False)
    with pytest.raises(AssertionError, match="empty"):
        assert_errors_equal("something", "", byte_for_byte=False)


def test_third_party_errors_must_fail_at_the_same_location() -> None:
    with pytest.raises(AssertionError, match="different locations"):
        assert_errors_equal(
            "effects[0].name: bad",
            "effects[1].name: bad",
            byte_for_byte=False,
            location_pattern=r"effects\[\d+\]\.name",
        )


def test_assert_out_serialization_accepts_plain_layout() -> None:
    assert_out_serialization('{"a": 1, "b": 2}\n', pretty=False)


def test_assert_out_serialization_accepts_pretty_layout() -> None:
    assert_out_serialization('{\n  "a": 1,\n  "b": 2\n}\n', pretty=True)


def test_assert_out_serialization_rejects_unsorted_keys_for_pretty() -> None:
    with pytest.raises(AssertionError):
        assert_out_serialization('{\n  "b": 2,\n  "a": 1\n}\n', pretty=True)


def test_assert_out_serialization_rejects_missing_trailing_newline() -> None:
    with pytest.raises(AssertionError):
        assert_out_serialization('{"a": 1}', pretty=False)
