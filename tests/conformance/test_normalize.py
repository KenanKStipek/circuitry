"""Unit tests for the shared normalizer (electricity/DESIGN.md §12)."""

from __future__ import annotations

import json

import pytest

from .normalize import (
    NormalizationError,
    apply_known_divergence,
    assert_errors_equal,
    assert_events_equal,
    assert_out_serialization,
    assert_states_equal,
    assert_values_equal,
    normalize,
    normalize_for_comparison,
    redact_leaked_paths,
    redact_runtime_strings,
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


def _event(ev: str, path: str | None = None, **extra: object) -> dict:
    payload: dict[str, object] = {"v": 1, "ev": ev, "ts": "2026-10-09T00:00:00.000Z"}
    if path is not None:
        payload["path"] = path
    payload.update(extra)
    return payload


def _events_text(events: list[dict]) -> str:
    """The real `--events` wire format: compact separators (runtime-
    semantics §8.7, `cli/events.py::EventLog._write_line`'s own
    `separators=(",", ":")`), not `json.dumps`'s spaced default."""
    return "\n".join(json.dumps(event, separators=(",", ":")) for event in events) + "\n"


_RUN_START = _event(
    "run_start",
    run_id="11111111-1111-1111-1111-111111111111",
    pid=1,
    engine="cof 0.0.0",
    orchestration="doc.yml",
)
_RUN_END = _event("run_end")


def test_assert_events_equal_accepts_identical_streams() -> None:
    events = [
        _RUN_START,
        _event("start", path="prime.t"),
        _event("dispatch", path="prime.t", branches=2, concurrency=1),
        _event("end", path="prime.t"),
        _RUN_END,
    ]
    text = _events_text(events)
    assert_events_equal(text, text)


def test_assert_events_equal_detects_same_path_reordering() -> None:
    """A path's own events have exactly one valid order (`start`, then
    `dispatch`, then `end`) -- a multiset comparison can't tell `dispatch`
    before `start` from the real order, since the three events are
    otherwise identical once `ts` is normalized."""
    expected = _events_text(
        [
            _RUN_START,
            _event("start", path="prime.t"),
            _event("dispatch", path="prime.t", branches=2, concurrency=1),
            _event("end", path="prime.t"),
            _RUN_END,
        ]
    )
    actual = _events_text(
        [
            _RUN_START,
            _event("dispatch", path="prime.t", branches=2, concurrency=1),
            _event("start", path="prime.t"),
            _event("end", path="prime.t"),
            _RUN_END,
        ]
    )
    with pytest.raises(AssertionError):
        assert_events_equal(actual, expected)


def test_assert_events_equal_detects_chain_sibling_reordering() -> None:
    """A chain's direct children run strictly in document order on both
    engines -- unlike a tree dynamic's branches, free to interleave with
    each other, a chain container (no `dispatch` event of its own) whose
    children started in a different relative order between the two
    streams is a real divergence."""

    def stream(order: list[str]) -> str:
        events = [_RUN_START, _event("start", path="prime.outer")]
        for name in order:
            events.append(_event("start", path=f"prime.outer.{name}"))
            events.append(_event("end", path=f"prime.outer.{name}"))
        events.append(_event("end", path="prime.outer"))
        events.append(_RUN_END)
        return _events_text(events)

    expected = stream(["a", "b"])
    actual = stream(["b", "a"])
    with pytest.raises(AssertionError):
        assert_events_equal(actual, expected)


def test_assert_events_equal_allows_tree_sibling_interleaving() -> None:
    """A tree dynamic's own `dispatch` event excludes its children from the
    chain-sibling-order check -- branches may interleave freely."""

    def stream(order: list[str]) -> str:
        events = [
            _RUN_START,
            _event("start", path="prime.t"),
            _event("dispatch", path="prime.t", branches=2, concurrency=2),
        ]
        for name in order:
            events.append(_event("start", path=f"prime.t.{name}"))
            events.append(_event("end", path=f"prime.t.{name}"))
        events.append(_event("end", path="prime.t"))
        events.append(_RUN_END)
        return _events_text(events)

    expected = stream(["a", "b"])
    actual = stream(["b", "a"])
    assert_events_equal(actual, expected)


def test_assert_events_equal_rejects_non_compact_separators() -> None:
    """The wire-format check (runtime-semantics §8.7) fails on a stream
    whose lines are valid JSON but weren't written with `cof`'s own
    `separators=(",", ":")` -- a format drift the per-field comparison
    below never catches on its own, since it parses every line first.
    This is the exact drift electricity's `--events` had before it
    switched from `WriteMode::COMPACT` to `WriteMode::EVENTS`: every
    line still parsed and compared equal, but read `"v": 1` instead of
    `"v":1`."""
    events = [_RUN_START, _RUN_END]
    compact = _events_text(events)
    spaced = "\n".join(json.dumps(event) for event in events) + "\n"
    with pytest.raises(AssertionError):
        assert_events_equal(spaced, compact)


def test_redact_runtime_strings_replaces_every_occurrence_regardless_of_key() -> None:
    value = {
        "raw": {"binary": "/case/fakes/fake"},
        "meta": {"binary": "/case/fakes/fake", "cwd": "/case"},
        "list": ["/case/x", "unrelated"],
    }
    redacted = redact_runtime_strings(
        value, [("/case/fakes", "<FAKES_DIR>"), ("/case", "<CASE_DIR>")]
    )
    assert redacted == {
        "raw": {"binary": "<FAKES_DIR>/fake"},
        "meta": {"binary": "<FAKES_DIR>/fake", "cwd": "<CASE_DIR>"},
        "list": ["<CASE_DIR>/x", "unrelated"],
    }


def test_redact_runtime_strings_works_on_a_bare_string() -> None:
    assert redact_runtime_strings("/case/fakes/fake timed out", [("/case", "<CASE_DIR>")]) == (
        "<CASE_DIR>/fakes/fake timed out"
    )


def test_normalize_for_comparison_composes_redaction_and_normalize() -> None:
    value = {"created_at": "2026-10-07T20:24:55.787572+00:00", "path": "/case/x"}
    assert normalize_for_comparison(value, [("/case", "<CASE_DIR>")]) == {
        "created_at": "<TIMESTAMP>",
        "path": "<CASE_DIR>/x",
    }


def test_apply_known_divergence_replaces_a_dict_leaf() -> None:
    state = {"prime": {"pattern": {"value": "python-value"}}}
    result = apply_known_divergence(
        state, location="prime.pattern.value", value="electricity-value"
    )
    assert result == {"prime": {"pattern": {"value": "electricity-value"}}}
    # The original is untouched -- a deep copy, not a mutation.
    assert state == {"prime": {"pattern": {"value": "python-value"}}}


def test_apply_known_divergence_replaces_a_list_element() -> None:
    state = {"prime": {"items": ["a", "b", "c"]}}
    result = apply_known_divergence(state, location="prime.items.1", value="B")
    assert result == {"prime": {"items": ["a", "B", "c"]}}


def test_apply_known_divergence_rejects_a_missing_location() -> None:
    state = {"prime": {"value": 1}}
    with pytest.raises(KeyError):
        apply_known_divergence(state, location="prime.nope", value=1)
