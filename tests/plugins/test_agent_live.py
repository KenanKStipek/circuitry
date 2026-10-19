"""The agent tool's live summary and streamed transcript (#636):
``LivePublisher``'s throttle, ``LiveTracker``'s incremental parsing of
each engine's stream, and the end-of-attempt consistency with the
authoritative end-of-turn parse. ``--live-state`` end-to-end tests are in
``tests/cli/test_run_agent_live.py``; the privacy gate is in
``tests/cli/test_run_agent_live_privacy.py``.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from circuitry.agent_live import LivePublisher, LiveTracker, diff_stats, tool_target


class _FakeClock:
    """A monotonic clock a test advances by hand, so the publisher's
    throttle decisions are deterministic and the test never sleeps a real
    second (#636)."""

    def __init__(self, start: float = 0.0) -> None:
        self.now = start

    def __call__(self) -> float:
        return self.now

    def advance(self, seconds: float) -> None:
        self.now += seconds


def test_live_publisher_publishes_the_first_update_immediately() -> None:
    clock = _FakeClock()
    published: list[dict[str, Any]] = []
    publisher = LivePublisher(published.append, interval=1.0, now=clock)
    try:
        publisher.update({"n": 1})
        for _ in range(200):
            if published:
                break
            clock.advance(0.01)
    finally:
        publisher.close()
    assert published == [{"n": 1}]


def test_live_publisher_coalesces_updates_inside_the_interval() -> None:
    clock = _FakeClock()
    published: list[dict[str, Any]] = []
    publisher = LivePublisher(published.append, interval=1.0, now=clock)
    try:
        publisher.update({"n": 1})
        _wait_until(lambda: published)
        # Two more updates inside the same second: neither publishes on
        # its own, only the second (the newest) ever will.
        publisher.update({"n": 2})
        publisher.update({"n": 3})
        clock.advance(0.5)
        _settle()
        assert published == [{"n": 1}]
        # Past the interval, the newest pending snapshot goes out on its
        # own -- no further update() call needed.
        clock.advance(0.6)
        _wait_until(lambda: len(published) == 2)
    finally:
        publisher.close()
    assert published == [{"n": 1}, {"n": 3}]


def test_live_publisher_never_publishes_more_than_once_per_interval() -> None:
    """A burst of updates inside one interval publishes at most once for
    it -- the interval is a hard cap, not a best-effort debounce (#636)."""
    clock = _FakeClock()
    published: list[dict[str, Any]] = []
    publisher = LivePublisher(published.append, interval=1.0, now=clock)
    try:
        for n in range(50):
            publisher.update({"n": n})
            clock.advance(0.01)
        _settle()
        assert len(published) <= 1
    finally:
        publisher.close()


def _settle() -> None:
    import time

    time.sleep(0.05)


def _wait_until(predicate: Any, *, timeout: float = 5.0) -> None:
    import time

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.01)
    raise AssertionError("condition never became true")


def test_tool_target_reads_a_path_a_command_and_a_pattern_only() -> None:
    assert tool_target({"path": "a.py"}) == "a.py"
    assert tool_target({"file_path": "a.py"}) == "a.py"
    assert tool_target({"command": "pytest -k thing"}) == "pytest -k thing"
    assert tool_target({"command": ["pytest", "-k", "thing"]}) == "pytest -k thing"
    assert tool_target({"pattern": "TODO", "path": "src/"}) == "TODO src/"
    assert tool_target({"pattern": "TODO"}) == "TODO"
    assert tool_target({"command": "x" * 500}) == "x" * 120
    assert tool_target({"nonsense": "value"}) is None
    assert tool_target(None) is None


def test_diff_stats_counts_lines_only_for_recognised_tools() -> None:
    assert diff_stats("write", {"path": "a.txt", "content": "a\nb\nc"}) == ("a.txt", 3, 0)
    # An empty content value counts the same as a missing one -- null, not
    # zero, since this module never distinguishes "absent" from "empty".
    assert diff_stats("write", {"path": "a.txt", "content": ""}) == ("a.txt", None, None)
    assert diff_stats("edit", {"path": "a.txt", "old_string": "a\nb", "new_string": "a\nb\nc"}) == (
        "a.txt",
        3,
        2,
    )
    assert diff_stats(
        "multiedit",
        {
            "path": "a.txt",
            "edits": [
                {"old_string": "a", "new_string": "a\nb"},
                {"old_string": "x\ny", "new_string": "z"},
            ],
        },
    ) == ("a.txt", 3, 3)
    # Nothing recognised in the arguments: stays null, not zero.
    assert diff_stats("write", {"path": "a.txt"}) == ("a.txt", None, None)
    assert diff_stats("bash", {"command": "rm a.txt"}) == (None, None, None)


def test_live_tracker_pi_stream_matches_sync_turn_totals_at_the_end(tmp_path: Path) -> None:
    published: list[dict[str, Any]] = []
    tracker = LiveTracker(
        engine="pi",
        model="sonnet",
        attempt=1,
        transcript_path=tmp_path / "t.log",
        publish=published.append,
        interval=0.01,
    )
    try:
        tracker.start_turn(phase="turn", session_id=None)
        for line in (
            '{"type": "session", "id": "sess-x"}',
            '{"type": "tool_execution_start", "toolName": "read", "args": {"path": "a.py"}}',
            '{"type": "tool_execution_end", "toolName": "read", "isError": false, "durationMs": 12}',
            (
                '{"type": "message_end", "message": {"role": "assistant", '
                '"usage": {"input": 10, "output": 5, "cacheRead": 2, "cacheWrite": 0, '
                '"cost": {"total": 0.01}}, "content": [{"type": "text", "text": "ok"}], '
                '"stopReason": "stop"}}'
            ),
        ):
            tracker.handle_line(line)
        tracker.sync_turn_totals(
            turns=1, tool_calls=1, tokens_sent=12, tokens_received=5, cost_usd=0.01
        )
        extra = tracker.raw_extra()
    finally:
        tracker.close()

    assert published, "the first update should have published immediately"
    last = published[-1]
    assert (last["turns"], last["tool_calls"]) == (1, 1)
    assert (last["tokens_sent"], last["tokens_received"]) == (12, 5)
    assert last["session_id"] == "sess-x"
    assert last["tools"] == {"read": {"calls": 1, "ms": 12}}
    assert extra["tool_counts"] == {"read": {"calls": 1, "ms": 12}}
    assert extra["context_tokens"] == 12


def test_live_tracker_skips_message_update_lines_before_json_loads(tmp_path: Path) -> None:
    """pi's ``message_update`` deltas are prefiltered without a full parse
    (#636) -- a malformed one (invalid JSON past the prefix) would raise if
    it ever reached ``json.loads``."""
    tracker = LiveTracker(
        engine="pi",
        model="",
        attempt=1,
        transcript_path=tmp_path / "t.log",
        publish=lambda _snapshot: None,
    )
    try:
        tracker.handle_line('{"type": "message_update", "not": "valid json ][')
    finally:
        tracker.close()


def test_live_tracker_tool_names_cap_at_64_chars_and_32_distinct_names(tmp_path: Path) -> None:
    tracker = LiveTracker(
        engine="pi",
        model="",
        attempt=1,
        transcript_path=tmp_path / "t.log",
        publish=lambda _snapshot: None,
    )
    try:
        long_name = "x" * 100
        tracker.handle_line(
            f'{{"type": "tool_execution_start", "toolName": "{long_name}", "args": {{}}}}'
        )
        for i in range(32):
            tracker.handle_line(
                f'{{"type": "tool_execution_start", "toolName": "tool-{i}", "args": {{}}}}'
            )
        snapshot = tracker._snapshot()
    finally:
        tracker.close()
    assert len(long_name[:64]) == 64
    assert long_name[:64] in snapshot["tools"]
    # 32 distinct real names kept (the long one plus tool-0..tool-30); the
    # 33rd distinct name (tool-31) is bucketed under "other" instead.
    assert len(snapshot["tools"]) == 33
    assert "other" in snapshot["tools"]
