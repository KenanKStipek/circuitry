"""``meta.live`` while an agent session runs, through the real
``--live-state`` mirror and the real engine-specific stream parsing
(#636): a replay fake gates the stream with a file, so the test reads
``meta.live`` mid-stream instead of guessing a sleep duration, then
releases it and checks the finished node.
"""

from __future__ import annotations

import json
import threading
import time
from pathlib import Path
from typing import Any

import pytest
from agent_cli_test_support import REPLAY_BODY, read_fixture_stream, write_fake_cli

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()

DOCUMENT = """\
effects:
  - type: tool
    name: implement
    provider: agent
    timeout_ms: 30000
    params:
      prompt: "do the task"
      cwd: {cwd}
      engine: {engine}
"""


def _write(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def _live_meta(path: Path) -> dict[str, Any] | None:
    if not path.exists():
        return None
    try:
        state = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return None
    node = state.get("prime", {}).get("implement", {})
    return node.get("meta", {}).get("live")


def _wait_for(predicate: Any, *, timeout: float = 15.0) -> Any:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.02)
    raise AssertionError("condition never became true within the timeout")


def _wait_for_live_meta(live: Path, *, tool_calls: int, timeout: float = 15.0) -> dict[str, Any]:
    def _check() -> dict[str, Any] | None:
        meta = _live_meta(live)
        return meta if meta is not None and meta["tool_calls"] == tool_calls else None

    return _wait_for(_check, timeout=timeout)


#: Exact mid-stream values each fixture must produce at gate1/gate2, keyed
#: by engine (#636 follow-up -- a loose `> 0`/`is not None` check can't
#: catch Claude Code's per-message.id usage duplication, since any wrong,
#: nonzero number still passes it). Hand-computed from each fixture file:
#: pi's own stream has one assistant message per gate; Claude Code's
#: fixture gives msg_1 two lines (a text block, then a tool_use block)
#: sharing one message.id, so its usage (input 100 + cache_read 10) must
#: count once, not twice, by gate1.
GATE_EXPECTATIONS: dict[str, dict[str, dict[str, Any]]] = {
    "pi": {
        "gate1": {"turns": 0, "tool_calls": 1, "tokens_sent": None, "context_tokens": None},
        "gate2": {"turns": 1, "tool_calls": 2, "tokens_sent": 110, "context_tokens": 110},
    },
    "claude_code": {
        "gate1": {"turns": 1, "tool_calls": 1, "tokens_sent": 110, "context_tokens": 110},
        "gate2": {"turns": 2, "tool_calls": 2, "tokens_sent": 275, "context_tokens": 165},
    },
}

#: The final, authoritative turn count each engine's own end-of-turn parse
#: reports for its fixture (`num_turns` for Claude Code; one per
#: `message_end` for pi) -- unaffected by this gate's own dedup fix, since
#: it comes from the parser these tests don't touch, but still worth
#: pinning down exactly rather than a loose `>= 2`.
FINAL_TURNS = {"pi": 2, "claude_code": 3}


@pytest.mark.parametrize(
    ("engine", "binary_name", "fixture", "fixture_engine_key"),
    [
        ("pi", "pi", "pi-stream.jsonl", "pi"),
        ("claude_code", "claude", "claude-stream.jsonl", "claude_code"),
    ],
)
def test_live_state_tracks_the_running_summary_while_gated(
    tmp_path: Path, engine: str, binary_name: str, fixture: str, fixture_engine_key: str
) -> None:
    work = tmp_path / "work"
    work.mkdir()
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake = write_fake_cli(bin_dir, binary_name, body=REPLAY_BODY)
    stream = _write(tmp_path / "stream.jsonl", read_fixture_stream(fixture))
    orch = _write(tmp_path / "doc.yml", DOCUMENT.format(cwd=json.dumps(str(work)), engine=engine))
    config = _write(
        tmp_path / "config.json",
        json.dumps({"runtime": {"plugins": {"agent": {fixture_engine_key: {"binary": str(fake)}}}}}),
    )
    live = tmp_path / "live.json"
    out = tmp_path / "out.json"
    events = tmp_path / "events.jsonl"

    results: list[Any] = []

    def _run() -> None:
        results.append(
            runner.invoke(
                app,
                [
                    "run",
                    str(orch),
                    "--config",
                    str(config),
                    "--out",
                    str(out),
                    "--live-state",
                    str(live),
                    "--events",
                    str(events),
                ],
                env={"FAKE_STREAM": str(stream)},
            )
        )

    expect = GATE_EXPECTATIONS[engine]

    thread = threading.Thread(target=_run)
    thread.start()
    try:
        # Gated at gate1: the first tool call has started, the second
        # hasn't. The engines' own tool names differ (pi: lowercase;
        # Claude Code: capitalised), so the tool name itself isn't checked
        # here, but every number is, exactly -- a loose `> 0`/`is not None`
        # check can't catch Claude Code's own message.id usage duplication,
        # since a wrong, nonzero number still passes it (#636 follow-up).
        first = _wait_for_live_meta(live, tool_calls=1)
        assert first["engine"] == engine
        assert first["phase"] == "turn"
        assert first["current_tool"] is not None
        assert first["turns"] == expect["gate1"]["turns"]
        assert first["tokens_sent"] == expect["gate1"]["tokens_sent"]
        assert first["context_tokens"] == expect["gate1"]["context_tokens"]

        # The transcript is written as the session goes, not only once it
        # finishes -- already readable, with a timed tool_start line in it,
        # while the CLI is still gated mid-stream (#636).
        transcript_mid_run = Path(first["transcript"])
        assert transcript_mid_run.exists()
        mid_run_lines = [
            json.loads(line)
            for line in transcript_mid_run.read_text(encoding="utf-8").splitlines()
            if line.strip()
        ]
        assert mid_run_lines, "the transcript has nothing in it yet, mid-stream"
        tool_start_mid_run = next(line for line in mid_run_lines if line["type"] == "tool_start")
        assert isinstance(tool_start_mid_run["t"], str) and tool_start_mid_run["t"]

        (work / "gate1").touch()

        # Gated at gate2: the first tool has ended (one assistant message
        # recorded), the second tool call has started.
        second = _wait_for_live_meta(live, tool_calls=2)
        assert second["turns"] == expect["gate2"]["turns"]
        assert second["tokens_sent"] == expect["gate2"]["tokens_sent"]
        assert second["context_tokens"] == expect["gate2"]["context_tokens"]

        (work / "gate2").touch()
        thread.join(timeout=20)
    finally:
        if thread.is_alive():
            (work / "gate1").touch()
            (work / "gate2").touch()
            thread.join(timeout=20)

    assert results and results[0].exit_code == 0, results[0].stdout if results else "no result"
    final_state = json.loads(out.read_text(encoding="utf-8"))
    node = final_state["prime"]["implement"]
    assert "live" not in node["meta"]
    assert node["meta"]["raw"]["turns"] == FINAL_TURNS[engine]
    assert node["meta"]["raw"]["tool_calls"] == 2
    assert node["meta"]["raw"]["tool_counts"]
    assert node["meta"]["raw"]["files_changed"] == 1
    transcript_path = Path(node["meta"]["raw"]["transcript"])
    assert transcript_path.exists()
    lines = [json.loads(line) for line in transcript_path.read_text(encoding="utf-8").splitlines()]
    assert any(line["type"] == "tool_start" for line in lines)
    assert any(line["type"] == "tool_end" for line in lines)
