"""Unit tests for the shared normalizer (electricity/DESIGN.md §12)."""

from __future__ import annotations

import json

import pytest

from .normalize import (
    THIRD_PARTY_TEXT_PLACEHOLDER,
    NormalizationError,
    _mask_profile_schema_validation_lines,
    apply_known_divergence,
    assert_errors_equal,
    assert_events_equal,
    assert_out_serialization,
    assert_states_equal,
    assert_values_equal,
    mask_known_third_party_text,
    mask_known_third_party_text_in,
    mask_known_third_party_text_to_end,
    normalize,
    normalize_for_comparison,
    normalize_recorded_http_requests,
    redact_events_text_pair,
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


def test_normalize_service_log_field_keeps_only_the_basename() -> None:
    assert normalize({"log": "/tmp/anything/services/svc.log"}) == {"log": "svc.log"}


def test_normalize_service_pgid_field() -> None:
    assert normalize({"pgid": 12345}) == {"pgid": "<PID>"}


def test_normalize_service_pgid_field_allows_none() -> None:
    assert normalize({"pgid": None}) == {"pgid": None}


def test_normalize_rejects_non_positive_pgid() -> None:
    with pytest.raises(NormalizationError):
        normalize({"pgid": 0})
    with pytest.raises(NormalizationError):
        normalize({"pgid": True})


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


def test_assert_states_equal_treats_two_nans_as_equal() -> None:
    # Issue #516 §3: a complexity score's own
    # `signals.*.normalized`/`contribution` can legitimately be NaN on
    # both engines for the same non-finite input -- `float("nan") !=
    # float("nan")` under plain `==`, so this must not raise.
    assert_states_equal({"x": float("nan")}, {"x": float("nan")})


def test_assert_states_equal_still_catches_a_nan_vs_a_real_number() -> None:
    with pytest.raises(AssertionError):
        assert_states_equal({"x": float("nan")}, {"x": 1.0})


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
    config=None,
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


def _run_start_with_config(config: str | None) -> dict:
    return {**_RUN_START, "config": config}


def test_assert_events_equal_compares_a_named_config_exactly() -> None:
    """A case that names a `config` gives both engines that exact name --
    the two sides' own `run_start.config` is compared exactly, like any
    other field."""
    expected = _events_text([_run_start_with_config("config.json"), _RUN_END])
    actual = _events_text([_run_start_with_config("config.json"), _RUN_END])
    assert_events_equal(actual, expected)

    mismatched = _events_text([_run_start_with_config("other.json"), _RUN_END])
    with pytest.raises(AssertionError):
        assert_events_equal(mismatched, expected)


def test_assert_events_equal_accepts_null_or_the_electricity_placeholder_for_no_config() -> None:
    """A case that names no `config` gives `cof` nothing (`null`) but
    still has to give electricity's required config argument *something*
    -- the conformance electricity runner's own placeholder config. Either
    is accepted against an expected `null`; nothing else is."""
    expected = _events_text([_RUN_START, _RUN_END])
    assert_events_equal(_events_text([_RUN_START, _RUN_END]), expected)

    placeholder = _events_text([_run_start_with_config("default_config.json"), _RUN_END])
    assert_events_equal(placeholder, expected)

    bogus = _events_text([_run_start_with_config("something-else.json"), _RUN_END])
    with pytest.raises(AssertionError):
        assert_events_equal(bogus, expected)


def test_assert_events_equal_rejects_a_run_start_missing_config_on_either_side() -> None:
    """A stream from before this field existed is missing the `config` key
    outright, distinct from carrying it as `null` -- caught rather than
    silently treated as a match."""
    expected = _events_text([_RUN_START, _RUN_END])
    pre_field_start = dict(_RUN_START)
    del pre_field_start["config"]

    actual_missing = _events_text([pre_field_start, _RUN_END])
    with pytest.raises(AssertionError):
        assert_events_equal(actual_missing, expected)

    expected_missing = _events_text([pre_field_start, _RUN_END])
    with pytest.raises(AssertionError):
        assert_events_equal(_events_text([_RUN_START, _RUN_END]), expected_missing)


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


def test_assert_events_equal_allows_unnamed_tree_loop_pass_interleaving() -> None:
    """A path shared by several concurrent tree passes of an unnamed loop
    (`Frame::with_pass` only disambiguates a *named* loop's own passes, so
    an unnamed loop's own tree passes all compile to the same body path)
    is compared as a sorted multiset, not an ordered list -- free to
    interleave with itself on either engine, the same way a tree dynamic's
    distinct sibling paths already are (lane H, issue #466)"""

    def stream(order: list[int]) -> str:
        events = [
            _RUN_START,
            _event("start", path="prime.shots"),
            _event("dispatch", path="prime.shots", branches=2, concurrency=2),
        ]
        for i in order:
            events.append(_event("start", path="prime.shots.step", id=i))
            events.append(_event("end", path="prime.shots.step", id=i))
        events.append(_event("end", path="prime.shots"))
        events.append(_RUN_END)
        return _events_text(events)

    expected = stream([1, 2])
    actual = stream([2, 1])
    assert_events_equal(actual, expected)


def test_assert_events_equal_detects_a_multi_start_path_ending_before_it_starts() -> None:
    """Even on a multi-start path (compared as a multiset across engines),
    one engine's own stream pairing an `end` to an `id` that never started
    first is still a real bug -- `_check_instance_pairing` catches it
    within a single stream, before the cross-engine multiset comparison
    even runs. The plain parent/child start-before-start, end-before-end
    check (`_split_and_check_order`) never notices this one: it tracks
    only each path's *first* start and *last* end, not per-instance
    pairing."""
    bad = _events_text(
        [
            _RUN_START,
            _event("start", path="prime.shots"),
            _event("dispatch", path="prime.shots", branches=2, concurrency=2),
            _event("end", path="prime.shots.step", id=1),
            _event("start", path="prime.shots.step", id=1),
            _event("start", path="prime.shots.step", id=2),
            _event("end", path="prime.shots.step", id=2),
            _event("end", path="prime.shots"),
            _RUN_END,
        ]
    )
    good = _events_text(
        [
            _RUN_START,
            _event("start", path="prime.shots"),
            _event("dispatch", path="prime.shots", branches=2, concurrency=2),
            _event("start", path="prime.shots.step", id=1),
            _event("end", path="prime.shots.step", id=1),
            _event("start", path="prime.shots.step", id=2),
            _event("end", path="prime.shots.step", id=2),
            _event("end", path="prime.shots"),
            _RUN_END,
        ]
    )
    with pytest.raises(AssertionError):
        assert_events_equal(bad, good)


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


# ---- Message-shape catalogue (issue #778 step 6) ----


def test_mask_cel_parse_error_keeps_location_and_expression_repr() -> None:
    text = (
        "CEL expression at 'prime.effects[1]' (effect 'broken'): expression does not parse "
        "(state.input.x &&\n              ^). Expression: 'state.input.x &&'"
    )
    assert mask_known_third_party_text(text) == (
        "CEL expression at 'prime.effects[1]' (effect 'broken'): expression does not parse "
        f"({THIRD_PARTY_TEXT_PLACEHOLDER}). Expression: 'state.input.x &&'"
    )


def test_mask_cel_parse_error_leaves_non_parse_failures_untouched() -> None:
    # `expression is nested too deeply (limit 32)` and the scope-check
    # messages (`state_ns.py`) have no library text at all -- never matched.
    text = "CEL expression at 'prime.effects[0]' (effect 'gate'): expression is nested too deeply (limit 32)"
    assert mask_known_third_party_text(text) == text


def test_mask_cel_eval_error_masks_each_occurrence_up_to_its_own_line_end() -> None:
    # `overflow` is cel-python's own text (#400: a relational comparison
    # whose operand doesn't fit CEL's 64-bit `int`) -- the one shape
    # `_CEL_EVAL_OWN_TEXT_RE` doesn't cover, since this module never
    # constructs it itself.
    text = (
        "3 effects failed in parallel:\n"
        "  [1] CelEvaluationError: fan.cel_gate: CEL evaluation failed for '1 / 0': overflow\n"
        "  [2] CelEvaluationError: fan.cel_loop: CEL evaluation failed for '1 / 0': overflow\n"
        "  [3] AnswerParseError: fan.model_gate: could not parse a yes/no answer from 'maybe'"
    )
    masked = mask_known_third_party_text(text)
    assert masked == (
        "3 effects failed in parallel:\n"
        f"  [1] CelEvaluationError: fan.cel_gate: CEL evaluation failed for '1 / 0': {THIRD_PARTY_TEXT_PLACEHOLDER}\n"
        f"  [2] CelEvaluationError: fan.cel_loop: CEL evaluation failed for '1 / 0': {THIRD_PARTY_TEXT_PLACEHOLDER}\n"
        "  [3] AnswerParseError: fan.model_gate: could not parse a yes/no answer from 'maybe'"
    )


def test_mask_cel_eval_error_leaves_circuitrys_own_text_exact() -> None:
    # Every one of these is a fixed template `core/cel_eval.py` builds
    # itself (`_StateEvaluator`/`_error_text`), never cel-python's own
    # wording -- none are masked, on either side of the colon.
    for text in (
        "CEL evaluation failed for 'value.missing_key == 1': no such key: 'missing_key'",
        "CEL evaluation failed for '[1, 2, 3][5]': list index out of range: 5",
        "CEL evaluation failed for '5[0] == 1': cannot index an int with an int",
        "CEL evaluation failed for '1 / 0 == 1': division by zero",
        "CEL evaluation failed for '1 % 0 == 1': modulus by zero",
        "CEL evaluation failed for 'nope(1) == 1': undeclared reference to 'nope'",
        "CEL evaluation failed for '[1] + 1 == [1]': cannot apply '+' to a list and an int",
    ):
        assert mask_known_third_party_text(text) == text


def test_mask_cel_eval_error_does_not_swallow_trailing_circuitry_text() -> None:
    # A `Conditional`'s own warning line wraps the CEL phrase in parens and
    # follows it with `; on_error=...` on the same line -- even for a
    # third-party tail (never constructed by `_error_text` itself), that
    # trailing Circuitry text must survive.
    text = (
        "Conditional 'gate': condition failed (CEL evaluation failed for "
        "'state.prime.big.value > 0': overflow); on_error=continue, taking the else branch"
    )
    assert mask_known_third_party_text(text) == (
        "Conditional 'gate': condition failed (CEL evaluation failed for "
        f"'state.prime.big.value > 0': {THIRD_PARTY_TEXT_PLACEHOLDER}); on_error=continue, "
        "taking the else branch"
    )


def test_mask_yaml_parse_error_covers_both_circuitry_prefixes() -> None:
    assert mask_known_third_party_text("YAML parse error: while parsing a flow sequence") == (
        f"YAML parse error: {THIRD_PARTY_TEXT_PLACEHOLDER}"
    )
    assert mask_known_third_party_text("prime.plan_it: parse_failed: day is out of range for month") == (
        f"prime.plan_it: parse_failed: {THIRD_PARTY_TEXT_PLACEHOLDER}"
    )


def test_mask_yaml_parse_error_leaves_a_duplicate_key_message_exact() -> None:
    # `core/yaml_load.py::DuplicateKeyError` is entirely Circuitry's own
    # text, never PyYAML's -- `yaml.safe_load` itself says nothing about a
    # repeated key at all.
    text = (
        "YAML parse error: duplicate key 'effects' at line 6, column 1 "
        "(first defined at line 1, column 1); YAML would silently keep only the last one"
    )
    assert mask_known_third_party_text(text) == text


def test_mask_curl_detail_keeps_a_trailing_circuitry_hint_exact() -> None:
    text = (
        "ollama request failed (curl exit 7) model='test-model' "
        "url=http://127.0.0.1:1/api/generate: curl: (7) Failed to connect to 127.0.0.1 port 1 "
        "after 0 ms: Couldn't connect to server Ollama at http://127.0.0.1:1 is not reachable. "
        "Start it (`ollama serve`), or set `runtime.adapters.ollama.base_url` in your config. "
        "Run `cof doctor` to verify connectivity."
    )
    assert mask_known_third_party_text(text) == (
        "ollama request failed (curl exit 7) model='test-model' "
        f"url=http://127.0.0.1:1/api/generate: curl: (7) {THIRD_PARTY_TEXT_PLACEHOLDER} Ollama at "
        "http://127.0.0.1:1 is not reachable. Start it (`ollama serve`), or set "
        "`runtime.adapters.ollama.base_url` in your config. Run `cof doctor` to verify connectivity."
    )


def test_mask_curl_detail_stops_at_the_closing_quote_of_an_embedded_repr() -> None:
    # `core/prompt.py`'s own "All adapter attempts failed: [{...}]" wraps
    # each attempt's error text in a Python dict/list repr -- the curl
    # detail must not swallow the sibling keys after its own closing quote.
    text = (
        "All adapter attempts failed: [{'adapter': 'lmstudio', 'status': 'failed', "
        '\'error\': "request failed (curl exit 7) url=http://x: curl: (7) Failed to connect '
        "to 127.0.0.1 port 1 after 0 ms: Couldn't connect to server\", "
        "'tokens_sent': None, 'tokens_received': None}]"
    )
    assert mask_known_third_party_text(text) == (
        "All adapter attempts failed: [{'adapter': 'lmstudio', 'status': 'failed', "
        f'\'error\': "request failed (curl exit 7) url=http://x: curl: (7) {THIRD_PARTY_TEXT_PLACEHOLDER}", '
        "'tokens_sent': None, 'tokens_received': None}]"
    )


def test_mask_curl_detail_with_no_hint_masks_to_end() -> None:
    text = (
        "ollama request failed (curl exit 18) model='test-model' "
        "url=http://127.0.0.1:9/api/generate: curl: (18) transfer closed with 95 bytes remaining to read"
    )
    assert mask_known_third_party_text(text) == (
        "ollama request failed (curl exit 18) model='test-model' "
        f"url=http://127.0.0.1:9/api/generate: curl: (18) {THIRD_PARTY_TEXT_PLACEHOLDER}"
    )


def test_mask_regex_invalid_pattern_keeps_the_position_exact() -> None:
    text = "regex: invalid pattern: missing ), unterminated subpattern at position 0"
    assert mask_known_third_party_text(text) == (
        f"regex: invalid pattern: {THIRD_PARTY_TEXT_PLACEHOLDER} at position 0"
    )


def test_mask_regex_invalid_pattern_with_no_position_masks_to_end() -> None:
    text = "regex: invalid pattern: look-behind requires fixed-width pattern"
    assert mask_known_third_party_text(text) == f"regex: invalid pattern: {THIRD_PARTY_TEXT_PLACEHOLDER}"


def test_mask_schema_well_formedness_masks_a_schemaerror_message() -> None:
    text = (
        "Tool effect 'gate' (cel) at 'prime.effects[0]': params.schema is not a valid JSON "
        "Schema: 'a' is not of type 'array'"
    )
    assert mask_known_third_party_text(text) == (
        "Tool effect 'gate' (cel) at 'prime.effects[0]': params.schema is not a valid JSON "
        f"Schema: {THIRD_PARTY_TEXT_PLACEHOLDER}"
    )


def test_mask_schema_well_formedness_leaves_circuitrys_own_messages_exact() -> None:
    ref_chain = "Prompt 'ask': schema is not a valid JSON Schema: $ref chain is too long (limit 200 consecutive references)"
    multiple_of = "Prompt 'ask': schema is not a valid JSON Schema: multipleOf must be a number greater than 0"
    assert mask_known_third_party_text(ref_chain) == ref_chain
    assert mask_known_third_party_text(multiple_of) == multiple_of


def test_mask_profile_schema_validation_lines_masks_each_message_after_the_header() -> None:
    text = (
        "Profile 'p' at /x/profiles/p.yml failed schema validation:\n"
        "  - adapter: 5 is not of type 'string'"
    )
    assert mask_known_third_party_text(text) == (
        "Profile 'p' at /x/profiles/p.yml failed schema validation:\n"
        f"  - adapter: {THIRD_PARTY_TEXT_PLACEHOLDER}"
    )


def test_mask_profile_schema_validation_lines_never_touches_document_validation() -> None:
    # "failed schema validation:" never matches "Orchestration ... validation
    # failed:" -- the two headers are masked by two different catalogue
    # entries (`_mask_profile_schema_validation_lines` only ever matches
    # its own literal header), so neither interferes with the other.
    text = (
        "Profile 'p' at /x/profiles/p.yml failed schema validation:\n"
        "  - adapter: 5 is not of type 'string'"
    )
    assert _mask_profile_schema_validation_lines(text) == (
        "Profile 'p' at /x/profiles/p.yml failed schema validation:\n"
        f"  - adapter: {THIRD_PARTY_TEXT_PLACEHOLDER}"
    )


def test_mask_document_validation_lines_masks_each_jsonschema_message_body() -> None:
    text = (
        "Orchestration validation failed:\n"
        "  - effects[0].name: 'value' should not be valid under {'pattern': '^(value)$'}"
    )
    assert mask_known_third_party_text(text) == (
        "Orchestration validation failed:\n"
        f"  - effects[0].name: {THIRD_PARTY_TEXT_PLACEHOLDER}"
    )


def test_mask_document_validation_lines_covers_every_header_variant() -> None:
    for header in (
        "Inline orchestration validation failed:",
        "Orchestration /x/child.yml validation failed:",
        "Orchestration prime.use_child validation failed:",
    ):
        text = f"{header}\n  - effects[0].name: 'value' should not be valid under {{'pattern': '^(value)$'}}"
        assert mask_known_third_party_text(text) == (
            f"{header}\n  - effects[0].name: {THIRD_PARTY_TEXT_PLACEHOLDER}"
        )


def test_mask_os_strerror_masks_only_the_strerror_text() -> None:
    text = "systemone: params.state_file '/x' cannot be read: No such file or directory"
    assert mask_known_third_party_text(text) == (
        f"systemone: params.state_file '/x' cannot be read: {THIRD_PARTY_TEXT_PLACEHOLDER}"
    )


def test_mask_unrecognized_text_is_unchanged() -> None:
    text = "effects[0].name: 'value' is reserved per schema XYZ"
    assert mask_known_third_party_text(text) == text


def test_mask_os_errno_keeps_errno_and_path_exact() -> None:
    text = "prime.result: [Errno 20] Not a directory: 'orchestration.yml'"
    assert mask_known_third_party_text(text) == (
        f"prime.result: [Errno 20] {THIRD_PARTY_TEXT_PLACEHOLDER}: 'orchestration.yml'"
    )


def test_mask_os_errno_does_not_swallow_trailing_circuitry_text() -> None:
    text = (
        "Preflight failed: adapter:scripted: not ready — scripted adapter: cannot read "
        "scripted-replies.yaml: [Errno 2] No such file or directory: 'scripted-replies.yaml'). "
        "Re-run with --skip-preflight to bypass."
    )
    assert mask_known_third_party_text(text) == (
        "Preflight failed: adapter:scripted: not ready — scripted adapter: cannot read "
        f"scripted-replies.yaml: [Errno 2] {THIRD_PARTY_TEXT_PLACEHOLDER}: 'scripted-replies.yaml'). "
        "Re-run with --skip-preflight to bypass."
    )


def test_mask_os_errno_inside_an_embedded_repr() -> None:
    text = (
        "All adapter attempts failed: [{'adapter': 'claude_code', 'status': 'failed', "
        "'error': \"circuitry-test-missing-claude could not be started: [Errno 2] No such file "
        "or directory: 'circuitry-test-missing-claude'\", 'tokens_sent': None}]"
    )
    assert mask_known_third_party_text(text) == (
        "All adapter attempts failed: [{'adapter': 'claude_code', 'status': 'failed', "
        "'error': \"circuitry-test-missing-claude could not be started: [Errno 2] "
        f'{THIRD_PARTY_TEXT_PLACEHOLDER}: \'circuitry-test-missing-claude\'", '
        "'tokens_sent': None}]"
    )


def test_mask_known_third_party_text_to_end_masks_from_the_earliest_tail() -> None:
    text = (
        "ollama request failed (curl exit 7) model='m' url=http://127.0.0.1:1/api/generate: "
        "curl: (7) Failed to connect to 127.0.0.1 port 1 after 0 ms: Couldn't connect to server "
        "Ollama at http://127.0.0.1:1 is not reachable. Start it (`ollama serve`), or set "
        "`runtime.adapters.ollama.base_url` in your config. Run `cof doctor` to verify c"  # truncated mid-word
    )
    assert mask_known_third_party_text_to_end(text) == (
        "ollama request failed (curl exit 7) model='m' url=http://127.0.0.1:1/api/generate: "
        f"curl: (7) {THIRD_PARTY_TEXT_PLACEHOLDER}"
    )


def test_mask_known_third_party_text_to_end_leaves_unmatched_text_untouched() -> None:
    text = "effects[0].name: 'value' is reserved per schema XYZ"
    assert mask_known_third_party_text_to_end(text) == text


def _error_events_text(*error_values: str) -> str:
    """Like `_events_text`, but one `start`/`end(ok=false, error=...)`
    pair per *error_values* entry, at distinct paths `prime.e0`, `prime.e1`, ..."""
    events = [_RUN_START]
    for i, error in enumerate(error_values):
        path = f"prime.e{i}"
        events.append(_event("start", path=path))
        events.append(_event("end", path=path, ok=False, ms=0, error=error))
    events.append(_RUN_END)
    return _events_text(events)


def test_redact_events_text_pair_keeps_the_hint_when_neither_side_hit_the_cap() -> None:
    short_error = (
        "ollama request failed (curl exit 7) model='m' url=http://127.0.0.1:1/api/generate: "
        "curl: (7) [Errno 61] Connection refused Ollama at http://127.0.0.1:1 is not reachable. "
        "Start it (`ollama serve`), or set `runtime.adapters.ollama.base_url` in your config. "
        "Run `cof doctor` to verify connectivity."
    )
    actual_text = _error_events_text(short_error)
    expected_text = _error_events_text(short_error)
    actual_masked, expected_masked = redact_events_text_pair(actual_text, expected_text, [])
    assert actual_masked == expected_masked
    assert "is not reachable" in actual_masked


def test_redact_events_text_pair_masks_to_end_when_either_side_hit_the_cap() -> None:
    hint = (
        " Ollama at http://127.0.0.1:1 is not reachable. Start it (`ollama serve`), or set "
        "`runtime.adapters.ollama.base_url` in your config. Run `cof doctor` to verify connectivity."
    )
    prefix = "ollama request failed (curl exit 7) model='m' url=http://127.0.0.1:1/api/generate: curl: (7) "
    # A short, untruncated detail on one side; a detail long enough that
    # prefix + detail + hint reaches the 500-char cap on the other.
    short_side = prefix + "[Errno 61] Connection refused" + hint
    long_detail = "x" * (500 - len(prefix) - len(hint) + 5)
    long_side = (prefix + long_detail + hint)[:500]
    assert len(short_side) < 500
    assert len(long_side) >= 500

    actual_text = _error_events_text(short_side)
    expected_text = _error_events_text(long_side)
    actual_masked, expected_masked = redact_events_text_pair(actual_text, expected_text, [])
    assert actual_masked == expected_masked
    # Masked to the very end on both sides -- the hint is gone, not just the detail.
    assert "is not reachable" not in actual_masked
    assert prefix.rstrip() in actual_masked


def test_redact_events_text_pair_leaves_an_untruncatable_path_alone_even_if_another_path_is_truncated() -> None:
    hint = (
        " Ollama at http://127.0.0.1:1 is not reachable. Start it (`ollama serve`), or set "
        "`runtime.adapters.ollama.base_url` in your config. Run `cof doctor` to verify connectivity."
    )
    prefix = "ollama request failed (curl exit 7) model='m' url=http://127.0.0.1:1/api/generate: curl: (7) "
    long_detail = "x" * (500 - len(prefix) - len(hint) + 5)
    long_error = (prefix + long_detail + hint)[:500]
    short_error = prefix + "[Errno 61] Connection refused" + hint
    assert len(long_error) >= 500
    assert len(short_error) < 500

    actual_text = _error_events_text(short_error, short_error)
    expected_text = _error_events_text(short_error, long_error)
    actual_masked, expected_masked = redact_events_text_pair(actual_text, expected_text, [])
    # prime.e0 (short on both sides) keeps its hint; prime.e1 (long on the
    # expected side) is masked to the end on both sides.
    assert actual_masked.count("is not reachable") == 1
    assert expected_masked.count("is not reachable") == 1


def test_assert_errors_equal_masks_a_catalogued_shape_without_a_location_pattern() -> None:
    actual = "CEL expression at 'prime.effects[1]' (effect 'broken'): expression does not parse (foo\n^). Expression: 'foo'"
    expected = "CEL expression at 'prime.effects[1]' (effect 'broken'): expression does not parse (bar\n ^). Expression: 'foo'"
    assert_errors_equal(actual, expected, byte_for_byte=False)


def test_assert_errors_equal_still_catches_a_different_location_after_masking() -> None:
    actual = "CEL expression at 'prime.effects[1]' (effect 'broken'): expression does not parse (foo\n^). Expression: 'foo'"
    expected = "CEL expression at 'prime.effects[2]' (effect 'broken'): expression does not parse (bar\n ^). Expression: 'foo'"
    with pytest.raises(AssertionError, match="differs even after masking"):
        assert_errors_equal(actual, expected, byte_for_byte=False)


def test_assert_errors_equal_ignores_a_location_pattern_once_a_shape_matched() -> None:
    # A `location_pattern` is only the fallback for a shape the catalogue
    # doesn't recognise at all -- once a shape *did* match on either side,
    # a difference in Circuitry's own text must fail outright, never be
    # excused by a pattern that happens to find the same location in both.
    actual = "CEL expression at 'prime.effects[1]' (effect 'broken'): expression does not parse (foo\n^). Expression: 'foo'"
    expected = "CEL expression at 'prime.effects[2]' (effect 'broken'): expression does not parse (bar\n ^). Expression: 'foo'"
    with pytest.raises(AssertionError, match="differs even after masking"):
        assert_errors_equal(
            actual, expected, byte_for_byte=False, location_pattern=r"CEL expression at '[^']+'"
        )


def test_assert_errors_equal_location_pattern_fallback_only_when_no_shape_matched() -> None:
    # Neither side matches any catalogue shape at all -- the older,
    # weaker "failed in the same place" fallback still applies here.
    actual = "effects[0].name: 'value' is reserved per schema XYZ"
    expected = "effects[0].name: 'value' is reserved per schema ABC"
    assert_errors_equal(actual, expected, byte_for_byte=False, location_pattern=r"effects\[0\]\.name")


def test_mask_known_third_party_text_in_masks_only_error_carrying_keys() -> None:
    state = {
        "meta": {"error": "CEL evaluation failed for 'x': overflow"},
        "value": {
            "errors": ["YAML parse error: while parsing a flow sequence"],
            "attempts": [{"error": "curl: (28) Operation timed out"}],
            "prompt_sent": "CEL evaluation failed for 'x': overflow",
        },
        "raw": {"stderr": "YAML parse error: while parsing a flow sequence"},
        "params_rendered": "curl: (28) Operation timed out",
    }
    masked = mask_known_third_party_text_in(state)
    assert masked["meta"]["error"] == f"CEL evaluation failed for 'x': {THIRD_PARTY_TEXT_PLACEHOLDER}"
    assert masked["value"]["errors"] == [f"YAML parse error: {THIRD_PARTY_TEXT_PLACEHOLDER}"]
    assert masked["value"]["attempts"] == [{"error": f"curl: (28) {THIRD_PARTY_TEXT_PLACEHOLDER}"}]
    # Content Circuitry sent or received, never its own error text --
    # untouched even though it contains a catalogue marker's literal text.
    assert masked["value"]["prompt_sent"] == "CEL evaluation failed for 'x': overflow"
    assert masked["raw"]["stderr"] == f"YAML parse error: {THIRD_PARTY_TEXT_PLACEHOLDER}"
    assert masked["params_rendered"] == "curl: (28) Operation timed out"


def test_normalize_recorded_http_requests_never_masks_a_request_body() -> None:
    # A recorded request's own body is content Circuitry sent onward, not
    # Circuitry's own error text -- even if it happens to contain a
    # catalogue marker's literal text, that text must survive untouched.
    requests = [
        {
            "method": "POST",
            "path": "/v1/chat",
            "query": [],
            "headers": {"host": "127.0.0.1:1"},
            "body": "CEL evaluation failed for 'x': this is ordinary request content, not an error",
        }
    ]
    assert normalize_recorded_http_requests(requests, []) == requests
