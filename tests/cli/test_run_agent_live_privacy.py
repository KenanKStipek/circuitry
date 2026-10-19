"""Privacy gate for the agent tool's live summary and transcript (#636):
a marker placed in the prompt, a reply, thinking, tool arguments, tool
output and a written file's content never reaches ``--live-state``,
``--out`` or ``--events``, but does appear in the transcript where the
design says it should (a tool's short target, and a reply's own text).

Every CLI here is a fake replay script (``agent_cli_test_support``'s
``REPLAY_BODY``) that streams a scrubbed, synthetic event stream built in
this file. No test runs a real CLI or calls a real model.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
from agent_cli_test_support import REPLAY_BODY, write_fake_cli, write_replay_stream

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()

MARKER_REPLY = "AGENT-LIVE-MARKER-REPLY-9d2c"
MARKER_THINKING = "AGENT-LIVE-MARKER-THINKING-9d2c"
MARKER_ARGS = "AGENT-LIVE-MARKER-ARGS-9d2c"
MARKER_FILE = "AGENT-LIVE-MARKER-FILE-9d2c"
MARKER_PROMPT = "AGENT-LIVE-MARKER-PROMPT-9d2c"
MARKER_OUTPUT = "AGENT-LIVE-MARKER-OUTPUT-9d2c"

DOCUMENT = """\
effects:
  - type: tool
    name: implement
    provider: agent
    timeout_ms: 30000
    params:
      prompt:
        file: {from: input.prompt_file}
        within: {from: input.work}
      cwd: {from: input.work}
"""

DOCUMENT_CLAUDE = """\
effects:
  - type: tool
    name: implement
    provider: agent
    timeout_ms: 30000
    params:
      engine: claude_code
      prompt:
        file: {from: input.prompt_file}
        within: {from: input.work}
      cwd: {from: input.work}
"""


def _write(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def _invoke(
    tmp_path: Path, work: Path, fake: Path, stream: Path, *, env: dict[str, str] | None = None
) -> dict[str, Path]:
    orch = _write(tmp_path / "doc.yml", DOCUMENT)
    _write(work / "task.md", f"{MARKER_PROMPT}: do the task.\n")
    config = _write(
        tmp_path / "config.json",
        json.dumps({"runtime": {"plugins": {"agent": {"pi": {"binary": str(fake)}}}}}),
    )
    paths = {
        "out": tmp_path / "out.json",
        "live": tmp_path / "live.json",
        "events": tmp_path / "events.jsonl",
    }
    result = runner.invoke(
        app,
        [
            "run",
            str(orch),
            "-e",
            "prompt_file=task.md",
            "-e",
            f"work={work}",
            "--config",
            str(config),
            "--out",
            str(paths["out"]),
            "--live-state",
            str(paths["live"]),
            "--events",
            str(paths["events"]),
        ],
        env={**(env or {}), "FAKE_STREAM": str(stream)},
    )
    assert result.exit_code == 0, result.stdout
    return paths


def _pi_stream(tmp_path: Path) -> Path:
    events: list[Any] = [
        {"type": "session", "id": "fx-pi-marker", "cwd": "."},
        {"type": "tool_execution_start", "toolName": "bash", "args": {"command": f"echo {MARKER_ARGS}"}},
        {"type": "tool_execution_end", "toolName": "bash", "isError": False, "durationMs": 5},
        {
            "type": "tool_execution_start",
            "toolName": "write",
            "args": {"path": "out.txt", "content": f"{MARKER_FILE}\nline2\n"},
        },
        {"type": "tool_execution_end", "toolName": "write", "isError": False, "durationMs": 5},
        {
            "type": "message_end",
            "message": {
                "role": "assistant",
                "usage": {"input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0, "cost": {"total": 0.01}},
                "content": [
                    {"type": "thinking", "text": MARKER_THINKING},
                    {"type": "text", "text": f"{MARKER_REPLY}: done."},
                ],
                "stopReason": "stop",
            },
        },
        {"type": "agent_end"},
    ]
    return write_replay_stream(tmp_path / "pi-marker-stream.jsonl", events)


def test_marker_leaks_nowhere_but_the_transcript_pi(tmp_path: Path) -> None:
    work = tmp_path / "work"
    work.mkdir()
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake = write_fake_cli(bin_dir, "pi", body=REPLAY_BODY)
    stream = _pi_stream(tmp_path)
    paths = _invoke(tmp_path, work, fake, stream)

    # `value` is the effect's own documented result without a result_file
    # (the final assistant text), present wherever a node's value reaches
    # state -- a separate, pre-existing contract, not part of this gate.
    # Everything else, in --out, --live-state and --events, must never
    # carry any of these markers.
    known_value = f"{MARKER_REPLY}: done."
    for name, path in paths.items():
        text = path.read_text(encoding="utf-8").replace(known_value, "")
        for marker in (MARKER_REPLY, MARKER_THINKING, MARKER_ARGS, MARKER_FILE, MARKER_PROMPT):
            assert marker not in text, f"{marker} leaked into {name}"

    out = json.loads(paths["out"].read_text(encoding="utf-8"))

    transcript_path = Path(out["prime"]["implement"]["meta"]["raw"]["transcript"])
    transcript = transcript_path.read_text(encoding="utf-8")
    # The tool target and the first part of a reply are expected there.
    assert MARKER_ARGS in transcript
    assert MARKER_REPLY in transcript
    # Thinking and file content never reach even the local transcript.
    assert MARKER_THINKING not in transcript
    assert MARKER_FILE not in transcript


def test_marker_in_a_tool_result_never_leaks_claude_code(tmp_path: Path) -> None:
    work = tmp_path / "work"
    work.mkdir()
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake = write_fake_cli(bin_dir, "claude", body=REPLAY_BODY)
    events: list[Any] = [
        {"type": "system", "subtype": "init", "session_id": "fx-claude-marker", "model": "claude-x"},
        {
            "type": "assistant",
            "message": {
                "id": "msg_1",
                "usage": {"input_tokens": 10, "output_tokens": 0,
                          "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0},
                "content": [{"type": "tool_use", "id": "call_1", "name": "Bash",
                              "input": {"command": "echo hi"}}],
            },
        },
        {
            "type": "user",
            "message": {"content": [
                {"type": "tool_result", "tool_use_id": "call_1",
                 "content": f"{MARKER_OUTPUT} from the tool", "is_error": False}
            ]},
        },
        {
            "type": "assistant",
            "message": {"id": "msg_2",
                         "usage": {"input_tokens": 15, "output_tokens": 3,
                                   "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0},
                         "content": [{"type": "text", "text": "done."}]},
        },
        {"type": "result", "subtype": "success", "is_error": False, "result": "done.",
         "session_id": "fx-claude-marker", "num_turns": 2, "total_cost_usd": 0.01,
         "usage": {"input_tokens": 15, "output_tokens": 3,
                   "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}},
    ]
    stream = write_replay_stream(tmp_path / "claude-marker-stream.jsonl", events)
    config_runtime = {"runtime": {"plugins": {"agent": {"claude_code": {"binary": str(fake)}}}}}
    orch = _write(tmp_path / "doc.yml", DOCUMENT_CLAUDE)
    _write(work / "task.md", "do the task.\n")
    config = _write(tmp_path / "config.json", json.dumps(config_runtime))
    paths = {
        "out": tmp_path / "out.json",
        "live": tmp_path / "live.json",
        "events": tmp_path / "events.jsonl",
    }
    result = runner.invoke(
        app,
        [
            "run",
            str(orch),
            "-e",
            "prompt_file=task.md",
            "-e",
            f"work={work}",
            "--config",
            str(config),
            "--out",
            str(paths["out"]),
            "--live-state",
            str(paths["live"]),
            "--events",
            str(paths["events"]),
        ],
        env={"FAKE_STREAM": str(stream)},
    )
    assert result.exit_code == 0, result.stdout
    for name, path in paths.items():
        assert MARKER_OUTPUT not in path.read_text(encoding="utf-8"), f"leaked into {name}"
    out = json.loads(paths["out"].read_text(encoding="utf-8"))
    transcript_path = Path(out["prime"]["implement"]["meta"]["raw"]["transcript"])
    assert MARKER_OUTPUT not in transcript_path.read_text(encoding="utf-8")
