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
    import json

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
