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

    thread = threading.Thread(target=_run)
    thread.start()
    try:
        # Gated at gate1: the first tool call has started, the second
        # hasn't. The engines' own tool names differ (pi: lowercase;
        # Claude Code: capitalised), so only the counts are checked here.
        first = _wait_for_live_meta(live, tool_calls=1)
        assert first["engine"] == engine
        assert first["phase"] == "turn"
        assert first["current_tool"] is not None

        (work / "gate1").touch()

        # Gated at gate2: the first tool has ended (one assistant message
        # recorded), the second tool call has started.
        second = _wait_for_live_meta(live, tool_calls=2)
        assert second["turns"] >= 1
        assert second["tokens_sent"] is not None and second["tokens_sent"] > 0
        assert second["context_tokens"] is not None

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
    assert node["meta"]["raw"]["turns"] >= 2
    assert node["meta"]["raw"]["tool_calls"] == 2
    assert node["meta"]["raw"]["tool_counts"]
    assert node["meta"]["raw"]["files_changed"] == 1
    transcript_path = Path(node["meta"]["raw"]["transcript"])
    assert transcript_path.exists()
    lines = [json.loads(line) for line in transcript_path.read_text(encoding="utf-8").splitlines()]
    assert any(line["type"] == "tool_start" for line in lines)
    assert any(line["type"] == "tool_end" for line in lines)
