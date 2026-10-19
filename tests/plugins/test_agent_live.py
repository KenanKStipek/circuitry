"""The agent tool's live summary and streamed transcript (#636):
``LivePublisher``'s throttle, ``LiveTracker``'s incremental parsing of
each engine's stream, and the end-of-attempt consistency with the
authoritative end-of-turn parse. ``--live-state`` end-to-end tests are in
``tests/cli/test_run_agent_live.py``; the privacy gate is in
``tests/cli/test_run_agent_live_privacy.py``.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

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


def test_live_publisher_survives_a_failing_publish_call(
    caplog: pytest.LogCaptureFixture,
) -> None:
    """A failing *publish* call (the running node already gone, say) must
    not kill the writer thread -- it logs once and keeps looping, the
    same as ``LiveStateMirror._write_loop`` already does for its own
    writer thread (#636 review)."""
    clock = _FakeClock()
    calls: list[dict[str, Any]] = []

    def _publish(snapshot: dict[str, Any]) -> None:
        calls.append(snapshot)
        if len(calls) == 1:
            raise RuntimeError("boom")

    publisher = LivePublisher(_publish, interval=1.0, now=clock)
    try:
        publisher.update({"n": 1})
        _wait_until(lambda: len(calls) == 1)
        clock.advance(1.5)
        publisher.update({"n": 2})
        _wait_until(lambda: len(calls) == 2)
    finally:
        publisher.close()
    assert calls == [{"n": 1}, {"n": 2}]
    assert any("publish" in record.message for record in caplog.records)


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
    # pi's own `write`: {path, content}.
    assert diff_stats("write", {"path": "a.txt", "content": "x\ny\nz"}) == ("a.txt", 3, 0)
    # pi's own `edit`: {path, edits: [{oldText, newText}, ...]}, two edits.
    assert diff_stats(
        "edit",
        {
            "path": "a.txt",
            "edits": [
                {"oldText": "a", "newText": "a\nb"},
                {"oldText": "x\ny", "newText": "z"},
            ],
        },
    ) == ("a.txt", 3, 3)
    # A single top-level oldText/newText pair instead of an edits list.
    assert diff_stats("edit", {"path": "a.txt", "oldText": "a\nb", "newText": "a\nb\nc"}) == (
        "a.txt",
        3,
        2,
    )
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
        # Before any sync -- the running totals this module computed on its
        # own from the stream, not merely values a later call will hand
        # back (#636 follow-up). Read via `_snapshot()` directly, not the
        # throttled `published` list, which may not have caught up yet.
        mid_stream = tracker._snapshot()
        assert (mid_stream["turns"], mid_stream["tool_calls"]) == (1, 1)
        assert (mid_stream["tokens_sent"], mid_stream["tokens_received"]) == (12, 5)
        assert mid_stream["session_id"] == "sess-x"
        assert mid_stream["tools"] == {"read": {"calls": 1, "ms": 12}}

        # The authoritative end-of-turn parse can disagree (a repair turn
        # folding in an earlier turn's own count, say) -- `sync_turn_totals`
        # must actually overwrite, not just echo back what it was handed,
        # so these numbers are deliberately different from the stream's own.
        tracker.sync_turn_totals(
            turns=2, tool_calls=3, tokens_sent=99, tokens_received=40, cost_usd=0.5
        )
        extra = tracker.raw_extra()
    finally:
        tracker.close()

    assert published, "the first update should have published immediately"
    last = published[-1]
    assert (last["turns"], last["tool_calls"]) == (2, 3)
    assert (last["tokens_sent"], last["tokens_received"]) == (99, 40)
    assert last["cost_usd"] == 0.5
    # The sync overwrites only turns/tool_calls/tokens/cost -- the tool
    # breakdown and context figures stay whatever the stream itself showed.
    assert last["tools"] == {"read": {"calls": 1, "ms": 12}}
    assert extra["tool_counts"] == {"read": {"calls": 1, "ms": 12}}
    assert extra["context_tokens"] == 12


def test_live_tracker_skips_message_update_lines_before_json_loads(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """pi's ``message_update`` deltas are prefiltered without a full parse
    (#636): ``json.loads`` is patched to fail the test if it's ever called
    at all, so this can't pass merely because ``handle_line`` already
    catches ``JSONDecodeError`` -- only the prefilter, not that catch,
    keeps this line from ever reaching ``json.loads``."""

    def _must_not_be_called(*_args: Any, **_kwargs: Any) -> Any:
        raise AssertionError("json.loads should not be called for a message_update line")

    monkeypatch.setattr("circuitry.agent_live.json.loads", _must_not_be_called)
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


def test_live_tracker_dedupes_claude_message_id_and_skips_subagent_usage(
    tmp_path: Path,
) -> None:
    """Claude Code prints one line per content block; lines from the same
    API call share ``message.id`` and repeat the same ``usage``. Each id
    must count once, for both turns and usage, and a subagent message
    (``parent_tool_use_id`` set) must never contribute to usage, tokens or
    context -- though its own tool call is still counted (#636)."""
    tracker = LiveTracker(
        engine="claude_code",
        model="claude-x",
        attempt=1,
        transcript_path=tmp_path / "t.log",
        publish=lambda _snapshot: None,
    )
    usage_a = {
        "input_tokens": 100, "output_tokens": 0,
        "cache_read_input_tokens": 10, "cache_creation_input_tokens": 0,
    }
    try:
        # Two lines, same message.id and the same usage repeated -- one API
        # call that emitted a text block and a tool call.
        tracker.handle_line(json.dumps({
            "type": "assistant",
            "message": {
                "id": "msg_1", "usage": usage_a,
                "content": [{"type": "text", "text": "Checking."}],
            },
        }))
        tracker.handle_line(json.dumps({
            "type": "assistant",
            "message": {
                "id": "msg_1", "usage": usage_a,
                "content": [
                    {"type": "tool_use", "id": "call_1", "name": "Read",
                     "input": {"path": "a.py"}},
                ],
            },
        }))
        after_dedupe = tracker._snapshot()
        assert after_dedupe["turns"] == 1
        assert after_dedupe["tool_calls"] == 1
        assert after_dedupe["tokens_sent"] == 110
        assert after_dedupe["context_tokens"] == 110
        assert after_dedupe["turn_tokens"] == [110]
        assert after_dedupe["tokens_cached_in"] == 10
        assert after_dedupe["tokens_cache_write"] == 0

        # A Task subagent message: its own id, its own tool call, but
        # parent_tool_use_id names the Task call it's running under. Its
        # usage is deliberately huge, so any leak into the totals below is
        # unmistakable.
        usage_sub = {
            "input_tokens": 999999, "output_tokens": 0,
            "cache_read_input_tokens": 999999, "cache_creation_input_tokens": 999999,
        }
        tracker.handle_line(json.dumps({
            "type": "assistant",
            "parent_tool_use_id": "call_1",
            "message": {
                "id": "msg_sub", "usage": usage_sub,
                "content": [
                    {"type": "tool_use", "id": "call_sub", "name": "Bash",
                     "input": {"command": "echo hi"}},
                ],
            },
        }))
        after_subagent = tracker._snapshot()
        # Its own message never counts as one of this session's own turns
        # (#636 review) -- its tool call is still counted...
        assert after_subagent["turns"] == 1
        assert after_subagent["tool_calls"] == 2
        # ...but none of its usage reached tokens, context or turn_tokens.
        assert after_subagent["tokens_sent"] == 110
        assert after_subagent["context_tokens"] == 110
        assert after_subagent["turn_tokens"] == [110]
        assert after_subagent["tokens_cached_in"] == 10
        assert after_subagent["tokens_cache_write"] == 0

        # A later, genuinely new message id resumes contributing normally.
        usage_c = {
            "input_tokens": 50, "output_tokens": 0,
            "cache_read_input_tokens": 5, "cache_creation_input_tokens": 5,
        }
        tracker.handle_line(json.dumps({
            "type": "assistant",
            "message": {
                "id": "msg_2", "usage": usage_c,
                "content": [{"type": "text", "text": "done"}],
            },
        }))
        final = tracker._snapshot()
        assert final["turns"] == 2
        assert final["tool_calls"] == 2
        assert final["tokens_sent"] == 170
        assert final["context_tokens"] == 60
        assert final["turn_tokens"] == [110, 60]
        assert final["tokens_cached_in"] == 15
        assert final["tokens_cache_write"] == 5
    finally:
        tracker.close()


def test_live_tracker_matches_tool_ends_by_id_not_fifo_claude(tmp_path: Path) -> None:
    """Two overlapping Claude tool calls (started before either ends) are
    matched to their own end by ``tool_use.id``/``tool_result.tool_use_id``,
    never by queue order -- a Task subagent's own first result must not pop
    its still-running parent off a FIFO queue (#636)."""
    tracker = LiveTracker(
        engine="claude_code",
        model="",
        attempt=1,
        transcript_path=tmp_path / "t.log",
        publish=lambda _snapshot: None,
    )
    try:
        # call_a (Write, has a diff) starts first; call_b (Read, no diff)
        # starts second, while call_a is still pending.
        tracker.handle_line(
            '{"type": "assistant", "message": {"id": "m1", "content": ['
            '{"type": "tool_use", "id": "call_a", "name": "Write", '
            '"input": {"path": "a.txt", "content": "line1\\nline2"}}]}}'
        )
        tracker.handle_line(
            '{"type": "assistant", "message": {"id": "m2", "content": ['
            '{"type": "tool_use", "id": "call_b", "name": "Read", '
            '"input": {"path": "b.txt"}}]}}'
        )
        # call_b (the newer one) ends first. FIFO would wrongly pop call_a
        # (Write) here instead, crediting its diff stats to the wrong call.
        tracker.handle_line(
            '{"type": "user", "message": {"content": ['
            '{"type": "tool_result", "tool_use_id": "call_b", "is_error": false}]}}'
        )
        assert tracker.raw_extra()["lines_added"] is None

        # call_a ends next -- only now does its own diff stat land.
        tracker.handle_line(
            '{"type": "user", "message": {"content": ['
            '{"type": "tool_result", "tool_use_id": "call_a", "is_error": false}]}}'
        )
        assert tracker.raw_extra()["lines_added"] == 2
    finally:
        tracker.close()


def test_live_tracker_matches_tool_ends_by_id_not_fifo_pi(tmp_path: Path) -> None:
    """The same matching, for pi's own ``toolCallId`` (#636)."""
    tracker = LiveTracker(
        engine="pi",
        model="",
        attempt=1,
        transcript_path=tmp_path / "t.log",
        publish=lambda _snapshot: None,
    )
    try:
        tracker.handle_line(
            '{"type": "tool_execution_start", "toolCallId": "call_a", "toolName": "write", '
            '"args": {"path": "a.txt", "content": "line1\\nline2"}}'
        )
        tracker.handle_line(
            '{"type": "tool_execution_start", "toolCallId": "call_b", "toolName": "read", '
            '"args": {"path": "b.txt"}}'
        )
        tracker.handle_line(
            '{"type": "tool_execution_end", "toolCallId": "call_b", "toolName": "read", '
            '"isError": false, "durationMs": 5}'
        )
        assert tracker.raw_extra()["lines_added"] is None
        tracker.handle_line(
            '{"type": "tool_execution_end", "toolCallId": "call_a", "toolName": "write", '
            '"isError": false, "durationMs": 5}'
        )
        assert tracker.raw_extra()["lines_added"] == 2
    finally:
        tracker.close()


def test_live_tracker_claude_task_subagent_current_tool_and_turns(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The Claude Task sequence (#636 review): a ``Task`` tool call, a
    subagent's own tool call under it (``parent_tool_use_id`` set), the
    subagent's own tool result, then the Task's own tool result.
    ``current_tool`` must track whichever tool is actually still running
    at each step -- not go blank just because the subagent's own tool
    ended first -- and a subagent's own message must never count as one
    of this session's own ``turns``, though its tool call still counts.
    """
    clock = [100.0]

    def _monotonic() -> float:
        return clock[0]

    monkeypatch.setattr("circuitry.agent_live.time.monotonic", _monotonic)
    tracker = LiveTracker(
        engine="claude_code",
        model="",
        attempt=1,
        transcript_path=tmp_path / "t.log",
        publish=lambda _snapshot: None,
    )
    try:
        # The Task call starts.
        tracker.handle_line(json.dumps({
            "type": "assistant",
            "message": {
                "id": "m1",
                "content": [
                    {"type": "tool_use", "id": "call_task", "name": "Task",
                     "input": {"description": "do a thing"}},
                ],
            },
        }))
        snapshot = tracker._snapshot()
        assert snapshot["turns"] == 1
        assert snapshot["current_tool"] is not None
        assert snapshot["current_tool"]["name"] == "Task"

        clock[0] += 0.5
        # The subagent's own tool call starts under it.
        tracker.handle_line(json.dumps({
            "type": "assistant",
            "parent_tool_use_id": "call_task",
            "message": {
                "id": "m2",
                "content": [
                    {"type": "tool_use", "id": "call_sub", "name": "Bash",
                     "input": {"command": "echo hi"}},
                ],
            },
        }))
        snapshot = tracker._snapshot()
        # A subagent's own message never counts as one of this session's
        # own turns (#636 review) -- still 1, not 2 (exact mid-stream
        # assertion).
        assert snapshot["turns"] == 1
        assert snapshot["tool_calls"] == 2
        assert snapshot["current_tool"] is not None
        assert snapshot["current_tool"]["name"] == "Bash"

        clock[0] += 0.5
        # The subagent's own tool call ends.
        tracker.handle_line(json.dumps({
            "type": "user",
            "message": {
                "content": [
                    {"type": "tool_result", "tool_use_id": "call_sub", "is_error": False},
                ],
            },
        }))
        snapshot = tracker._snapshot()
        # current_tool must fall back to the still-running Task call, not
        # go blank just because the subagent's own tool ended first (#636
        # review).
        assert snapshot["current_tool"] is not None
        assert snapshot["current_tool"]["name"] == "Task"

        clock[0] += 1.0
        # The Task call itself ends.
        tracker.handle_line(json.dumps({
            "type": "user",
            "message": {
                "content": [
                    {"type": "tool_result", "tool_use_id": "call_task", "is_error": False},
                ],
            },
        }))
        snapshot = tracker._snapshot()
        assert snapshot["current_tool"] is None
        assert snapshot["tools"]["Task"]["ms"] == 2000
    finally:
        tracker.close()


def test_live_tracker_repair_phase_differs_from_transcript_label(tmp_path: Path) -> None:
    """``meta.live.phase`` is the short ``\"turn\"``/``\"repair\"`` the
    issue's own contract names; the transcript's ``turn`` line keeps the
    more readable ``\"repair turn\"`` label (#636)."""
    tracker = LiveTracker(
        engine="pi",
        model="",
        attempt=1,
        transcript_path=tmp_path / "t.log",
        publish=lambda _snapshot: None,
    )
    try:
        tracker.start_turn(phase="turn", session_id="sess-1")
        assert tracker._snapshot()["phase"] == "turn"
        tracker.start_turn(phase="repair", label="repair turn", session_id="sess-1")
        assert tracker._snapshot()["phase"] == "repair"
    finally:
        tracker.close()
    lines = [
        json.loads(line)
        for line in (tmp_path / "t.log").read_text(encoding="utf-8").splitlines()
    ]
    turn_lines = [line for line in lines if line["type"] == "turn"]
    assert [line["label"] for line in turn_lines] == ["turn", "repair turn"]
