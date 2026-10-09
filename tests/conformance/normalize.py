"""Normalization rules for the conformance suite (electricity/DESIGN.md §12).

Shared by the Python runner (`test_python_runner.py`) and the electricity
runner (`test_electricity_runner.py`) so both engines are checked against
exactly the same rules — a divergence in the normalizer itself would be a
silent hole in the suite, not a caught parity bug.

Most fields are classified by **key name**, regardless of nesting depth,
per `_FIELD_RULES` below — the same field name means the same thing
everywhere in state (timestamps, durations, run ids/UUIDs). This category
is included here because it is test-*environment* dependent rather than
document-content dependent, and would otherwise make every case fail
outside the machine that generated its `expected.json`.

The absolute paths `effective_settings` echoes back (the `--out` path
itself, and the case's own directory under `_orchestration_dir` — both
absolute, both naming something on the machine and the moment that
generated `expected.json`, never portable) and the run's own orchestration
path (`orchestration_path`, already relative — stable, nothing to redact)
are the same kind of environment leakage, but are matched by their **exact
dotted location** from the state root (`_PATH_FIELD_RULES`) instead of by
key name: a key literally named `out` also occurs at
`effective_settings.sources.out`, where its value is a provenance tag
(`"cli"`/`"config"`/...), not a path, and must never be rewritten there.
The two absolute-path fields get a named, stable placeholder (`<out>`,
`<case>`) rather than their real value, both at comparison time
(`normalize()`) and — via the same table, `redact_leaked_paths()` — before
a case's `expected.json` is ever written to disk, so no contributor's own
checkout path or OS temp directory is committed.
"""

from __future__ import annotations

import json
import re
from datetime import datetime
from typing import Any

_UUID_RE = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$", re.IGNORECASE
)
_COMPACT_TIMESTAMP_RE = re.compile(r"^\d{8}_\d{6}$")

# Key name -> normalization kind. Checked against the *last* path segment
# (dict key) a value is stored under, independent of where in the tree it
# appears — the same field name means the same thing everywhere in state.
_FIELD_RULES: dict[str, str] = {
    "run_id": "uuid",
    "_run_id": "uuid",
    "started_at": "timestamp_iso",
    "completed_at": "timestamp_iso",
    "created_at": "timestamp_iso",
    "_timestamp": "timestamp_compact",
    "wall_time_s": "duration",
    "eta_s": "duration",
    "elapsed_s": "duration",
}

# Full dotted key-path from the state root -> normalization kind, for the
# fields that leak the test machine's own filesystem layout. Matched by
# exact location rather than key name: ``out`` and ``orchestration_path``
# are reused elsewhere in state for values that are *not* paths (a
# provenance tag, a relative filename already free of machine detail) and
# must not be touched there. A case that puts one of these fields
# somewhere new (e.g. a future CLI-stdout case needing ``state_out``) adds
# its exact path here rather than widening a key-name match.
_PATH_FIELD_RULES: dict[str, str] = {
    "runtime.effective_settings.out": "out_path",
    "runtime.effective_settings.runtime._orchestration_dir": "case_dir",
    "runtime.last_run.orchestration_path": "path_basename",
}

UUID_PLACEHOLDER = "<UUID>"
TIMESTAMP_PLACEHOLDER = "<TIMESTAMP>"
DURATION_PLACEHOLDER = "<DURATION>"

#: Stable, named placeholders for the two absolute-path fields — not a
#: generic `<PATH>` — so a reader of `expected.json` can tell *which* path
#: was redacted without cross-referencing this module. Keyed by the same
#: `_PATH_FIELD_RULES` kind, so `_normalize_leaf` (comparison time) and
#: `redact_leaked_paths` (write time) can never disagree on the token.
_PATH_PLACEHOLDERS: dict[str, str] = {
    "out_path": "<out>",
    "case_dir": "<case>",
}


class NormalizationError(AssertionError):
    """A value didn't have the shape its field name promises (e.g. a
    non-UUID string under a `run_id` key) — a real divergence, not noise to
    normalize away."""


def _normalize_leaf(key: str, value: Any, *, field_path: str | None) -> Any:
    kind = _PATH_FIELD_RULES.get(field_path) if field_path is not None else None
    if kind is None:
        kind = _FIELD_RULES.get(key)
    if kind is None:
        return value
    if kind == "uuid":
        if not isinstance(value, str) or not _UUID_RE.match(value):
            raise NormalizationError(f"{key!r}: expected a UUID string, got {value!r}")
        return UUID_PLACEHOLDER
    if kind == "timestamp_iso":
        if not isinstance(value, str):
            raise NormalizationError(f"{key!r}: expected an ISO-8601 string, got {value!r}")
        try:
            datetime.fromisoformat(value)
        except ValueError as exc:
            raise NormalizationError(f"{key!r}: not ISO-8601: {value!r}") from exc
        return TIMESTAMP_PLACEHOLDER
    if kind == "timestamp_compact":
        if not isinstance(value, str) or not _COMPACT_TIMESTAMP_RE.match(value):
            raise NormalizationError(
                f"{key!r}: expected %Y%m%d_%H%M%S, got {value!r}"
            )
        return TIMESTAMP_PLACEHOLDER
    if kind == "duration":
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise NormalizationError(f"{key!r}: expected a numeric duration, got {value!r}")
        if value < 0:
            raise NormalizationError(f"{key!r}: duration must be >= 0, got {value!r}")
        return DURATION_PLACEHOLDER
    if kind == "path_basename":
        if not isinstance(value, str) or not value:
            raise NormalizationError(f"{key!r}: expected a non-empty path string, got {value!r}")
        return value.rsplit("/", 1)[-1]
    if kind in _PATH_PLACEHOLDERS:
        if not isinstance(value, str) or not value:
            raise NormalizationError(f"{key!r}: expected a non-empty path string, got {value!r}")
        return _PATH_PLACEHOLDERS[kind]
    raise AssertionError(f"unreachable normalization kind {kind!r}")  # pragma: no cover


def normalize(value: Any, *, key: str | None = None, field_path: str | None = None) -> Any:
    """Recursively replace volatile/environment-dependent leaves with a
    fixed placeholder, after checking the leaf has the shape its field name
    promises. Dict/list structure and key order are preserved exactly —
    order is part of what the suite compares (§3.4). `field_path` is the
    dotted path of dict keys from the state root (list indices don't widen
    it — a list's elements share their parent's path), used by
    `_PATH_FIELD_RULES` to match a field by exact location rather than by
    name alone."""
    if isinstance(value, dict):
        return {
            k: normalize(v, key=k, field_path=f"{field_path}.{k}" if field_path else k)
            for k, v in value.items()
        }
    if isinstance(value, list):
        return [normalize(v, key=key, field_path=field_path) for v in value]
    if key is None:
        return value
    return _normalize_leaf(key, value, field_path=field_path)


def redact_leaked_paths(value: Any, *, field_path: str | None = None) -> Any:
    """Replace only the absolute-path fields `_PATH_FIELD_RULES` names in
    `_PATH_PLACEHOLDERS` (`<out>`, `<case>`) with that stable token, leaving
    everything else (including run id/timestamp/duration fields, which must
    stay real data on disk — `normalize()` validates their *shape* at
    comparison time, and a placeholder string wouldn't have it) untouched.
    `runtime.last_run.orchestration_path` (`"path_basename"`) is already a
    relative, portable value and is deliberately not redacted here — there
    is nothing of the generating machine left in it to remove. Used by
    `scripts/generate-conformance-cases.py` before a case's expected state
    is written to disk, so a contributor's own checkout path or OS temp
    directory is never committed (electricity/DESIGN.md §12; the suite's
    own comparisons already ignore these fields via `normalize()`, so
    nothing is lost)."""
    if isinstance(value, dict):
        result = {}
        for k, v in value.items():
            child_path = f"{field_path}.{k}" if field_path else k
            placeholder = _PATH_PLACEHOLDERS.get(_PATH_FIELD_RULES.get(child_path, ""))
            if placeholder is not None:
                result[k] = placeholder
            else:
                result[k] = redact_leaked_paths(v, field_path=child_path)
        return result
    if isinstance(value, list):
        return [redact_leaked_paths(v, field_path=field_path) for v in value]
    return value


def assert_states_equal(actual: Any, expected: Any, *, path: str = "$") -> None:
    """Order-sensitive structural equality after `normalize()`. Raises
    `AssertionError` naming the first differing path."""
    if isinstance(actual, dict) and isinstance(expected, dict):
        if list(actual.keys()) != list(expected.keys()):
            raise AssertionError(
                f"{path}: key order/set differs:\n"
                f"  actual:   {list(actual.keys())}\n"
                f"  expected: {list(expected.keys())}"
            )
        for k in actual:
            assert_states_equal(actual[k], expected[k], path=f"{path}.{k}")
        return
    if isinstance(actual, list) and isinstance(expected, list):
        if len(actual) != len(expected):
            raise AssertionError(
                f"{path}: length differs: {len(actual)} != {len(expected)}"
            )
        for i, (a, e) in enumerate(zip(actual, expected, strict=True)):
            assert_states_equal(a, e, path=f"{path}[{i}]")
        return
    if actual != expected:
        raise AssertionError(f"{path}: {actual!r} != {expected!r}")


def assert_values_equal(actual: Any, expected: Any, *, path: str = "$") -> None:
    """Structural equality after `normalize()`, ignoring dict key order —
    for asserting plain and `--pretty` carry the *same value*, independent
    of the layout difference `assert_states_equal` itself enforces."""
    if isinstance(actual, dict) and isinstance(expected, dict):
        if set(actual.keys()) != set(expected.keys()):
            raise AssertionError(
                f"{path}: key set differs:\n"
                f"  actual:   {sorted(actual.keys())}\n"
                f"  expected: {sorted(expected.keys())}"
            )
        for k in actual:
            assert_values_equal(actual[k], expected[k], path=f"{path}.{k}")
        return
    if isinstance(actual, list) and isinstance(expected, list):
        if len(actual) != len(expected):
            raise AssertionError(f"{path}: length differs: {len(actual)} != {len(expected)}")
        for i, (a, e) in enumerate(zip(actual, expected, strict=True)):
            assert_values_equal(a, e, path=f"{path}[{i}]")
        return
    if actual != expected:
        raise AssertionError(f"{path}: {actual!r} != {expected!r}")


def assert_errors_equal(
    actual: str,
    expected: str,
    *,
    byte_for_byte: bool,
    location_pattern: str | None = None,
) -> None:
    """Compare a failure's message per §12: Circuitry's own messages
    byte-for-byte (`byte_for_byte=True`); a third-party library's message
    only for "failed in the same place, with a non-empty message"
    (`byte_for_byte=False`) — never byte-for-byte, per the spec."""
    if byte_for_byte:
        if actual != expected:
            raise AssertionError(
                f"error text differs:\n  actual:   {actual!r}\n  expected: {expected!r}"
            )
        return
    if not actual:
        raise AssertionError("actual error message is empty")
    if not expected:
        raise AssertionError("fixture's expected error message is empty")
    if location_pattern is not None:
        actual_match = re.search(location_pattern, actual)
        expected_match = re.search(location_pattern, expected)
        if actual_match is None or expected_match is None:
            raise AssertionError(
                f"location pattern {location_pattern!r} not found in one of the two "
                f"error messages:\n  actual:   {actual!r}\n  expected: {expected!r}"
            )
        if actual_match.group(0) != expected_match.group(0):
            raise AssertionError(
                f"failed at different locations: "
                f"{actual_match.group(0)!r} != {expected_match.group(0)!r}"
            )


def assert_out_serialization(raw_text: str, *, pretty: bool) -> None:
    """Re-derive `--out`'s exact byte layout from its own parsed content and
    assert it round-trips (C23, §3.4.1): plain is insertion-order, no
    indent; `--pretty` is alphabetically sorted, 2-space indent. Both always
    `ensure_ascii=True` and end with exactly one trailing newline."""
    data = json.loads(raw_text)
    if pretty:
        expected_text = json.dumps(data, indent=2, sort_keys=True, ensure_ascii=True) + "\n"
    else:
        expected_text = json.dumps(data, separators=(", ", ": "), ensure_ascii=True) + "\n"
    if expected_text != raw_text:
        raise AssertionError(
            f"--out{' --pretty' if pretty else ''} is not the expected serialization "
            f"(insertion order + no indent for plain; sorted keys + 2-space indent for "
            f"--pretty; both ensure_ascii) — see electricity/DESIGN.md §3.4.1"
        )


_EVENT_TS_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$")

EVENT_TS_PLACEHOLDER = "<EVENT_TS>"
EVENT_MS_PLACEHOLDER = "<MS>"
EVENT_PID_PLACEHOLDER = "<PID>"
EVENT_ENGINE_PLACEHOLDER = "<ENGINE>"

#: `--events` fields normalized like any other volatile field (runtime-
#: semantics §8.7, issue #431's events-conformance acceptance criterion):
#: shape-checked, then replaced with a fixed placeholder. `run_id` reuses
#: the same UUID check/placeholder `normalize()` uses for state's own
#: `run_id`/`_run_id`.
_EVENT_FIELD_RULES = ("ts", "ms", "run_id", "pid", "engine")


def _normalize_event_field(key: str, value: Any) -> Any:
    if key == "ts":
        if not isinstance(value, str) or not _EVENT_TS_RE.match(value):
            raise NormalizationError(f"event {key!r}: not a millisecond UTC timestamp: {value!r}")
        return EVENT_TS_PLACEHOLDER
    if key == "ms":
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise NormalizationError(f"event {key!r}: expected a non-negative int, got {value!r}")
        return EVENT_MS_PLACEHOLDER
    if key == "run_id":
        if not isinstance(value, str) or not _UUID_RE.match(value):
            raise NormalizationError(f"event {key!r}: expected a UUID string, got {value!r}")
        return UUID_PLACEHOLDER
    if key == "pid":
        if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
            raise NormalizationError(f"event {key!r}: expected a positive int, got {value!r}")
        return EVENT_PID_PLACEHOLDER
    if key == "engine":
        if not isinstance(value, str) or not value:
            raise NormalizationError(f"event {key!r}: expected a non-empty string, got {value!r}")
        return EVENT_ENGINE_PLACEHOLDER
    raise AssertionError(f"unreachable event field {key!r}")  # pragma: no cover


def _comparable_event(event: dict[str, Any]) -> dict[str, Any]:
    """One event line, ready to compare across engines: `ts`/`ms`/
    `run_id`/`pid`/`engine` shape-checked and replaced with a fixed
    placeholder (`_normalize_event_field`); `seq` and the per-instance
    `id` dropped outright rather than normalized — both are assigned from
    one counter in *dispatch* order, and a tree dynamic's branches are
    free to start/finish in a different wall-clock order on the two
    engines (cof's real OS threads vs. electricity's single-threaded
    cooperative scheduler); comparing their absolute values would assert
    an implementation detail neither engine commits to. Every other key
    (`v`, `ev`, `path`, `ok`, `error`, `branches`, `concurrency`,
    `orchestration`, `signal`) is compared as-is."""
    comparable = {k: v for k, v in event.items() if k not in ("seq", "id")}
    for key in _EVENT_FIELD_RULES:
        if key in comparable:
            comparable[key] = _normalize_event_field(key, comparable[key])
    return comparable


def parse_event_lines(text: str) -> list[dict[str, Any]]:
    """One JSON object per non-empty line, in file order (`--events`'
    JSONL format, runtime-semantics §8.7)."""
    return [json.loads(line) for line in text.splitlines() if line.strip()]


def _split_and_check_order(
    events: list[dict[str, Any]], *, label: str
) -> tuple[dict[str, Any], list[dict[str, Any]], dict[str, Any]]:
    """Checks the structural ordering rule runtime-semantics §8.7
    specifies (`run_start` first, `run_end` last, every container's own
    `start` before any child's `start` and every child's `end` before its
    container's `end`), then splits off the two bracket events. Applied to
    *one* engine's own stream at a time — this is an internal-consistency
    check on that stream, not a cross-engine comparison; `assert_events_equal`
    calls it once per side precisely so a stream that is internally
    consistent but differs from the other engine's is still caught by the
    per-path comparison below, not mistaken for an ordering bug."""
    if not events:
        raise AssertionError(f"{label}: event stream is empty")
    if events[0].get("ev") != "run_start":
        raise AssertionError(f"{label}: event stream's first line is not run_start: {events[0]!r}")
    if events[-1].get("ev") != "run_end":
        raise AssertionError(f"{label}: event stream's last line is not run_end: {events[-1]!r}")

    starts: dict[str, int] = {}
    ends: dict[str, int] = {}
    for i, event in enumerate(events):
        path = event.get("path")
        if path is None:
            continue
        if event.get("ev") == "start":
            starts.setdefault(path, i)
        elif event.get("ev") == "end":
            ends[path] = i

    for child_path, child_start in starts.items():
        for parent_path, parent_start in starts.items():
            if parent_path == child_path or not child_path.startswith(f"{parent_path}."):
                continue
            if not parent_start < child_start:
                raise AssertionError(
                    f"{label}: {parent_path!r}'s start does not precede its child "
                    f"{child_path!r}'s start"
                )
            parent_end = ends.get(parent_path)
            child_end = ends.get(child_path)
            if parent_end is not None and child_end is not None and not child_end < parent_end:
                raise AssertionError(
                    f"{label}: {child_path!r}'s end does not precede its container "
                    f"{parent_path!r}'s end"
                )

    return events[0], events[1:-1], events[-1]


def _group_events_by_path(events: list[dict[str, Any]]) -> dict[str, list[dict[str, Any]]]:
    """Every event that carries a `path`, in stream order, grouped by that
    path. A path's own events have a fixed internal order in M0-H --
    `start`, then (a tree container only) `dispatch`, then `end` -- so
    comparing each path's own list *as a list*, not a multiset, catches an
    end-before-start or a dispatch-after-end on the same path, which a
    `collections.Counter` comparison cannot: distinct `ev` values compare
    equal as a set regardless of which order they came in."""
    groups: dict[str, list[dict[str, Any]]] = {}
    for event in events:
        path = event.get("path")
        if path is None:
            continue
        groups.setdefault(path, []).append(_comparable_event(event))
    return groups


def _chain_sibling_order(events: list[dict[str, Any]]) -> dict[str, list[str]]:
    """For every container path whose own stream carries no `dispatch`
    event (a chain, never a tree -- `fire_concurrent_dispatch` only ever
    fires for a tree-flow dynamic), the order its own *direct* children's
    `start` events occurred in, keyed by the container's path. A chain's
    children run strictly one after another, in document order, on both
    engines -- unlike a tree dynamic's branches, which are free to
    interleave with each other on either engine (cof's real OS threads vs.
    electricity's single-threaded cooperative scheduler) and are
    deliberately excluded here by the `dispatch`-event check."""
    dispatch_paths = {
        event["path"]
        for event in events
        if event.get("ev") == "dispatch" and event.get("path") is not None
    }
    order: dict[str, list[str]] = {}
    for event in events:
        if event.get("ev") != "start":
            continue
        path = event.get("path")
        if path is None or "." not in path:
            continue
        parent = path.rsplit(".", 1)[0]
        if parent in dispatch_paths:
            continue
        order.setdefault(parent, []).append(path)
    return order


def assert_events_equal(actual_text: str, expected_text: str) -> None:
    """Compare two `--events` JSONL streams (runtime-semantics §8.7,
    issue #431's events-conformance acceptance criterion): `run_start`
    and `run_end` are compared directly after normalization; both streams
    are independently checked against the start-before-child / child-end-
    before-container ordering rule (`_split_and_check_order`); every other
    event is grouped by its own `path` and compared *as an ordered list*
    (`_group_events_by_path`) -- a path's own events have exactly one
    valid order (`start`, optionally `dispatch`, then `end`), so this
    catches a same-path reordering a multiset comparison would miss; and a
    chain container's own direct children are compared for the order their
    `start` events occurred in (`_chain_sibling_order`) -- a tree
    dynamic's branches are excluded from that check and may interleave
    freely with each other on either engine, since only the nesting order
    between a container and its descendants (not sibling interleaving) is
    asserted for one of those."""
    actual = parse_event_lines(actual_text)
    expected = parse_event_lines(expected_text)
    actual_start, actual_middle, actual_end = _split_and_check_order(actual, label="actual")
    expected_start, expected_middle, expected_end = _split_and_check_order(
        expected, label="expected"
    )

    actual_start_c = _comparable_event(actual_start)
    expected_start_c = _comparable_event(expected_start)
    if actual_start_c != expected_start_c:
        raise AssertionError(
            f"run_start differs:\n  actual:   {actual_start_c!r}\n"
            f"  expected: {expected_start_c!r}"
        )
    actual_end_c = _comparable_event(actual_end)
    expected_end_c = _comparable_event(expected_end)
    if actual_end_c != expected_end_c:
        raise AssertionError(
            f"run_end differs:\n  actual:   {actual_end_c!r}\n  expected: {expected_end_c!r}"
        )

    actual_groups = _group_events_by_path(actual_middle)
    expected_groups = _group_events_by_path(expected_middle)
    if set(actual_groups) != set(expected_groups):
        raise AssertionError(
            f"event paths differ:\n  actual:   {sorted(actual_groups)}\n"
            f"  expected: {sorted(expected_groups)}"
        )
    for path in actual_groups:
        if actual_groups[path] != expected_groups[path]:
            raise AssertionError(
                f"{path}: events differ:\n  actual:   {actual_groups[path]!r}\n"
                f"  expected: {expected_groups[path]!r}"
            )

    actual_siblings = _chain_sibling_order(actual)
    expected_siblings = _chain_sibling_order(expected)
    if actual_siblings != expected_siblings:
        raise AssertionError(
            f"chain sibling start order differs:\n  actual:   {actual_siblings!r}\n"
            f"  expected: {expected_siblings!r}"
        )
