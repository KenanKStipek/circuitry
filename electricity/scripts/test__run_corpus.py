"""Unit tests for `_run_corpus.py`'s own `canonical_event_order` (not a
`generate_*.py` script itself -- not picked up by that workflow's glob,
same as `_run_corpus.py`; run directly, e.g. `pytest
electricity/scripts/test__run_corpus.py`).

The orchestrator's own review of PR #432 asked for proof that
`canonical_event_order`'s narrow, branch-region-only reordering cannot
hide a genuinely broken event stream: a chain (no concurrent `dynamic`
at all) keeps its real emission order byte for byte, so any corruption
of it -- a swapped pair, an `end` before its own `start`, a duplicate,
a missing `end` -- must still show up as a different canonical form, not
be silently reordered away like a tree branch's own region is.

Must be run with Python 3.11 (`generator-constraints.txt`'s own pin).
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

from _run_corpus import canonical_event_order, normalize_result


def _chain_events() -> list[dict[str, Any]]:
    """A plain sequential (`flow: chain`) two-step run's own `--events`
    sequence, already in its real, correct emission order -- `start`/
    `end` for `prime.a`, then `start`/`end` for `prime.b`. No `dispatch`
    at all: a chain has no branch region for `canonical_event_order` to
    reorder, so this is the baseline every corrupted variant below is
    compared against.
    """
    return [
        {"v": 1, "seq": 0, "ev": "start", "path": "prime.a", "id": 1},
        {"v": 1, "seq": 1, "ev": "end", "path": "prime.a", "id": 1, "ms": 1},
        {"v": 1, "seq": 2, "ev": "start", "path": "prime.b", "id": 2},
        {"v": 1, "seq": 3, "ev": "end", "path": "prime.b", "id": 2, "ms": 1},
    ]


def _canonical(events: list[dict[str, Any]]) -> list[tuple[str, str, int | None]]:
    """The `(ev, path, id)` triples `canonical_event_order` produces for
    *events*, with `seq`/`ms` (expected to change on renumbering/timing
    alone) dropped -- the comparison shape every test below uses, so a
    difference it finds is a real structural one, not a `seq`/`ms`
    artifact of renumbering itself.
    """
    return [(e["ev"], e["path"], e.get("id")) for e in canonical_event_order(events)]


def test_engine_pid_ts_ms_in_state_are_not_masked() -> None:
    # A document's own content can legitimately use any of these very
    # generic short names for something that isn't Circuitry's own
    # run metadata at all (a timer tool's own `ms:` param, say) --
    # `normalize_result` must leave `state`/`live_state` alone for
    # them, masking only an *event* object's own top-level `engine`/
    # `pid`/`ts`/`ms` (orchestrator ruling on PR #432's review).
    result = {
        "returncode": 0,
        "stdout": "",
        "stderr": "",
        "state": {"prime": {"ms": 5, "pid": "abc", "ts": "t0", "engine": "steam"}},
        "events": [
            {
                "v": 1,
                "seq": 0,
                "ts": "2026-01-01T00:00:00Z",
                "ev": "run_start",
                "engine": "electricity 0.1.0",
                "pid": 4242,
            },
        ],
        "live_state": {"ms": 5},
    }
    normalized = normalize_result(result, root="/tmp/does-not-appear")
    assert normalized["state"] == {
        "prime": {"ms": 5, "pid": "abc", "ts": "t0", "engine": "steam"}
    }
    assert normalized["live_state"] == {"ms": 5}
    event = normalized["events"][0]
    assert event["engine"] == "<ENGINE>"
    assert event["pid"] == "<PID>"
    assert event["ts"] == "<TS>"


def test_a_chains_own_correct_order_is_its_own_canonical_form() -> None:
    assert _canonical(_chain_events()) == [
        ("start", "prime.a", 0),
        ("end", "prime.a", 0),
        ("start", "prime.b", 1),
        ("end", "prime.b", 1),
    ]


def test_a_swapped_pair_differs_from_the_canonical_form() -> None:
    # `prime.a`'s start/end swapped with `prime.b`'s start -- the kind of
    # corruption a buggy engine's own interleaving could produce.
    events = _chain_events()
    events[1], events[2] = events[2], events[1]
    assert _canonical(events) != _canonical(_chain_events())


def test_an_end_before_its_own_start_differs_from_the_canonical_form() -> None:
    events = [
        {"v": 1, "seq": 0, "ev": "end", "path": "prime.a", "id": 1, "ms": 1},
        {"v": 1, "seq": 1, "ev": "start", "path": "prime.a", "id": 1},
        {"v": 1, "seq": 2, "ev": "start", "path": "prime.b", "id": 2},
        {"v": 1, "seq": 3, "ev": "end", "path": "prime.b", "id": 2, "ms": 1},
    ]
    assert _canonical(events) != _canonical(_chain_events())


def test_a_duplicated_end_differs_from_the_canonical_form() -> None:
    events = _chain_events()
    events.insert(2, dict(events[1]))  # a second `prime.a` end, same id
    assert _canonical(events) != _canonical(_chain_events())
    # The duplicate is visible in the canonical form too, not swallowed
    # by renumbering -- there are still two `end` events for `prime.a`.
    ends = [e for e in _canonical(events) if e[0] == "end" and e[1] == "prime.a"]
    assert len(ends) == 2


def test_an_ends_id_with_no_matching_start_in_this_stream_gets_a_sentinel() -> None:
    # `prime.a`'s `start` dropped outright -- its lone `end` still
    # carries the raw numeric id a corrupted/truncated stream handed it
    # (not `cof run --events`'s own documented `id: null` for "no
    # matching start at all", a different, already-normal case). Left
    # as the raw id, this could coincide with some other event's own
    # freshly renumbered canonical id and compare equal to a
    # differently-broken stream by accident -- the sentinel can't.
    events = [
        {"v": 1, "seq": 0, "ev": "end", "path": "prime.a", "id": 7, "ms": 1},
        {"v": 1, "seq": 1, "ev": "start", "path": "prime.b", "id": 2},
        {"v": 1, "seq": 2, "ev": "end", "path": "prime.b", "id": 2, "ms": 1},
    ]
    canonical = canonical_event_order(events)
    assert canonical[0]["id"] == "<UNMATCHED:7>"


def test_a_missing_container_end_differs_from_the_canonical_form() -> None:
    # A tree `dynamic`'s own dispatch, with both branches' events intact
    # but the dispatching container's own closing `end` dropped --
    # `_reorder_branches` reads the branch region up to (and including)
    # that `end`; if it's simply absent, the container never closes.
    with_end = [
        {"v": 1, "seq": 0, "ev": "dispatch", "path": "prime.fan_out", "branches": 2},
        {"v": 1, "seq": 1, "ev": "start", "path": "prime.fan_out.left", "id": 1},
        {"v": 1, "seq": 2, "ev": "end", "path": "prime.fan_out.left", "id": 1, "ms": 1},
        {"v": 1, "seq": 3, "ev": "start", "path": "prime.fan_out.right", "id": 2},
        {"v": 1, "seq": 4, "ev": "end", "path": "prime.fan_out.right", "id": 2, "ms": 1},
        {"v": 1, "seq": 5, "ev": "end", "path": "prime.fan_out", "id": None, "ms": 2},
    ]
    missing_end = with_end[:-1]
    branch_order = {"prime.fan_out": ["left", "right"]}
    canonical_with_end = [
        (e["ev"], e["path"], e.get("id"))
        for e in canonical_event_order(with_end, branch_order)
    ]
    canonical_missing_end = [
        (e["ev"], e["path"], e.get("id"))
        for e in canonical_event_order(missing_end, branch_order)
    ]
    assert canonical_missing_end != canonical_with_end
    assert ("end", "prime.fan_out", None) not in canonical_missing_end
