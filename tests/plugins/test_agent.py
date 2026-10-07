"""The ``agent`` tool plugin (#367): a delegated pi or Claude Code session.

Every CLI here is a fake script (``FAKE_AGENT_BODY``) that logs each call,
optionally writes the result file, and prints the engine's own output
format. No test runs a real CLI or calls a real model.
"""

from __future__ import annotations

import json
import os
import tempfile
import threading
import time
from pathlib import Path
from typing import Any

import pytest
from agent_cli_test_support import (
    SPAWN_CHILD_AND_HANG,
    pid_alive,
    wait_until_dead,
    write_fake_cli,
)

from circuitry.agent_cli import AgentCliTimeout
from circuitry.core import cancellation
from circuitry.core.cancellation import CancellationToken, RunCancelledBySignal
from circuitry.plugins.agent import AgentPlugin, make_plugin
from circuitry.plugins.capabilities import FS_WRITE, NETWORK, SHELL, capabilities_of
from circuitry.plugins.factory import build_plugin

#: Logs argv, stdin, attachments and selected env to ``FAKE_AGENT_LOG`` (one
#: JSON line per call). On a first call it writes ``FAKE_RESULT_FIRST`` to
#: ``FAKE_RESULT_PATH``, on a resumed call ``FAKE_RESULT_REPAIR`` (an unset
#: variable writes nothing). Then prints pi's event stream (``--mode`` in
#: argv) or Claude Code's stream-json, with one tool call and one reply.
FAKE_AGENT_BODY = """\
argv = sys.argv[1:]
is_pi = "--mode" in argv
resume_flag = "--session" if is_pi else "--resume"
resumed = argv[argv.index(resume_flag) + 1] if resume_flag in argv else None
attachments = {}
for arg in argv:
    if arg.startswith("@"):
        with open(arg[1:], encoding="utf-8") as handle:
            attachments[arg[1:]] = handle.read()
with open(os.environ["FAKE_AGENT_LOG"], "a", encoding="utf-8") as handle:
    handle.write(json.dumps({
        "argv": argv,
        "stdin": stdin,
        "attachments": list(attachments.values()),
        "cwd": os.getcwd(),
        "env": {key: os.environ.get(key) for key in
                ("ADDED_VAR", "ANTHROPIC_API_KEY", "PI_SESSION_ID", "KEEP_ME")},
    }) + "\\n")
content = os.environ.get("FAKE_RESULT_REPAIR" if resumed else "FAKE_RESULT_FIRST")
if content is not None:
    with open(os.environ["FAKE_RESULT_PATH"], "w", encoding="utf-8") as handle:
        handle.write(content)
session = resumed or "sess-abc"
reply = "repaired" if resumed else "all done"
events = []
if is_pi:
    usage = {"input": 10, "output": 5, "cacheRead": 100, "cacheWrite": 0,
             "cost": {"total": 0.01}}
    events = [
        {"type": "session", "id": session, "cwd": os.getcwd()},
        {"type": "message_end", "message": {"role": "user", "content": [{"type": "text", "text": "q"}]}},
        {"type": "message_end", "message": {"role": "assistant", "usage": usage,
            "content": [{"type": "toolCall", "name": "write"}], "stopReason": "toolUse"}},
        {"type": "tool_execution_start", "toolName": "write", "args": {"path": "out.json"}},
        {"type": "tool_execution_end", "toolName": "write"},
        {"type": "message_end", "message": {"role": "assistant", "usage": usage,
            "content": [{"type": "text", "text": reply}], "stopReason": "stop"}},
        {"type": "agent_end"},
    ]
else:
    events = [
        {"type": "system", "subtype": "init", "session_id": session},
        {"type": "assistant", "message": {"content": [
            {"type": "tool_use", "name": "Write", "input": {"file_path": "out.json"}}]}},
        {"type": "user", "message": {"content": [{"type": "tool_result", "content": "ok"}]}},
        {"type": "assistant", "message": {"content": [{"type": "text", "text": reply}]}},
        {"type": "result", "subtype": "success", "is_error": False, "result": reply,
         "session_id": session, "num_turns": 2, "total_cost_usd": 0.02,
         "usage": {"input_tokens": 7, "output_tokens": 3, "cache_read_input_tokens": 0,
                   "cache_creation_input_tokens": 0}},
    ]
for event in events:
    sys.stdout.write(json.dumps(event) + "\\n")
sys.exit(0)
"""

SCHEMA = {
    "type": "object",
    "properties": {"summary": {"type": "string"}, "files": {"type": "integer"}},
    "required": ["summary", "files"],
}


@pytest.fixture(autouse=True)
def _scratch_in_tmp_path(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """The plugin's scratch dirs (prompt files, transcript) land in tmp_path."""
    scratch = tmp_path / "scratch"
    scratch.mkdir()
    monkeypatch.setattr(tempfile, "tempdir", str(scratch))


@pytest.fixture
def workdir(tmp_path: Path) -> Path:
    path = tmp_path / "work"
    path.mkdir()
    return path


def _plugin(tmp_path: Path, engine: str, body: str = FAKE_AGENT_BODY) -> AgentPlugin:
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir(exist_ok=True)
    name = "pi" if engine == "pi" else "claude"
    fake = write_fake_cli(bin_dir, name, body=body)
    return AgentPlugin(default_engine=engine, binaries={engine: str(fake)})


def _env(tmp_path: Path, workdir: Path, **extra: str) -> dict[str, str]:
    return {
        "FAKE_AGENT_LOG": str(tmp_path / "calls.jsonl"),
        "FAKE_RESULT_PATH": str(workdir / "result.json"),
        **extra,
    }


def _calls(tmp_path: Path) -> list[dict[str, Any]]:
    log = tmp_path / "calls.jsonl"
    if not log.exists():
        return []
    return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]


# ---------- pi ----------


def test_pi_valid_result_file_is_the_value_with_session_turns_and_tokens(
    tmp_path: Path, workdir: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-should-not-reach-the-cli")
    monkeypatch.setenv("PI_SESSION_ID", "parent-session")
    monkeypatch.setenv("KEEP_ME", "kept")
    plugin = _plugin(tmp_path, "pi")
    result = plugin.execute(
        params={
            "prompt": "Line one of the task.\nLine two: fix the bug.",
            "cwd": str(workdir),
            "model": "anthropic/claude-x",
            "thinking": "high",
            "tools": ["read", "write"],
            "exclude_tools": ["bash"],
            "extra_args": ["--no-skills"],
            "env": _env(
                tmp_path, workdir, ADDED_VAR="added",
                FAKE_RESULT_FIRST=json.dumps({"summary": "fixed", "files": 2}),
            ),
            "result_file": "result.json",
            "result_schema": SCHEMA,
        },
        timeout_seconds=30,
    )
    assert result.ok
    assert result.value == {"summary": "fixed", "files": 2}
    raw = result.raw
    assert raw["engine"] == "pi"
    assert raw["session_id"] == "sess-abc"
    assert (raw["turns"], raw["tool_calls"]) == (2, 1)
    assert raw["tokens"] == {"sent": 220, "received": 10}
    assert raw["cost"] == pytest.approx(0.02)
    assert raw["repair_turn"] is False
    assert Path(raw["transcript"]).parent.parent == tmp_path / "scratch"
    assert [p.name for p in Path(raw["transcript"]).parent.iterdir()] == ["transcript.log"]
    transcript = Path(raw["transcript"]).read_text(encoding="utf-8")
    assert 'tool write {"path": "out.json"}' in transcript
    assert "reply all done" in transcript
    assert "Line one" not in json.dumps(raw)

    (call,) = _calls(tmp_path)
    argv = call["argv"]
    assert argv[argv.index("--tools") + 1] == "read,write"
    assert argv[argv.index("--exclude-tools") + 1] == "bash"
    assert argv[argv.index("--model") + 1] == "anthropic/claude-x"
    assert argv[argv.index("--thinking") + 1] == "high"
    assert "--no-skills" in argv
    assert "--no-tools" not in argv and "--no-session" not in argv
    # The multi-line prompt reaches pi as an attached file, never in argv.
    assert not any("Line one" in arg for arg in argv)
    (attachment,) = call["attachments"]
    assert "Line one of the task.\nLine two: fix the bug." in attachment
    assert str((workdir / "result.json").resolve()) in attachment
    assert '"required"' in attachment
    assert Path(call["cwd"]).resolve() == workdir.resolve()
    assert call["env"] == {
        "ADDED_VAR": "added",
        "ANTHROPIC_API_KEY": None,
        "PI_SESSION_ID": None,
        "KEEP_ME": "kept",
    }


def test_pi_missing_result_file_gets_one_repair_turn_in_the_same_session(
    tmp_path: Path, workdir: Path
) -> None:
    # A result left over from an earlier run must not pass for this one.
    (workdir / "result.json").write_text(json.dumps({"summary": "stale", "files": 9}))
    plugin = _plugin(tmp_path, "pi")
    result = plugin.execute(
        params={
            "prompt": "do it",
            "cwd": str(workdir),
            "env": _env(
                tmp_path, workdir, FAKE_RESULT_REPAIR=json.dumps({"summary": "ok", "files": 1})
            ),
            "result_file": "result.json",
            "result_schema": SCHEMA,
        },
        timeout_seconds=30,
    )
    assert result.ok, result.stderr
    assert result.value == {"summary": "ok", "files": 1}
    first, repair = _calls(tmp_path)
    assert "--session" not in first["argv"]
    assert repair["argv"][repair["argv"].index("--session") + 1] == "sess-abc"
    (repair_prompt,) = repair["attachments"]
    assert "the file was not written" in repair_prompt
    assert result.raw["repair_turn"] is True
    assert (result.raw["turns"], result.raw["tool_calls"]) == (4, 2)
    assert "== repair turn: pi session sess-abc" in Path(result.raw["transcript"]).read_text()


def test_pi_result_still_invalid_after_repair_fails_with_the_schema_errors(
    tmp_path: Path, workdir: Path
) -> None:
    plugin = _plugin(tmp_path, "pi")
    result = plugin.execute(
        params={
            "prompt": "do it",
            "cwd": str(workdir),
            "env": _env(
                tmp_path, workdir,
                FAKE_RESULT_FIRST=json.dumps({"summary": "x"}),
                FAKE_RESULT_REPAIR=json.dumps({"summary": "x", "files": "two"}),
            ),
            "result_file": "result.json",
            "result_schema": SCHEMA,
        },
        timeout_seconds=30,
    )
    assert not result.ok
    assert result.value is None
    assert result.stderr is not None
    assert "still invalid (after one repair turn)" in result.stderr
    assert "files: 'two' is not of type 'integer'" in result.stderr
    _first, repair = _calls(tmp_path)
    assert "'files' is a required property" in repair["attachments"][0]
    assert result.raw["session_id"] == "sess-abc"


def test_pi_without_result_file_returns_the_final_text(tmp_path: Path, workdir: Path) -> None:
    plugin = _plugin(tmp_path, "pi")
    result = plugin.execute(
        params={"prompt": "hi", "cwd": str(workdir), "env": _env(tmp_path, workdir)},
        timeout_seconds=30,
    )
    assert result.ok
    assert result.value == "all done"


def test_pi_session_param_resumes_that_session(tmp_path: Path, workdir: Path) -> None:
    plugin = _plugin(tmp_path, "pi")
    result = plugin.execute(
        params={
            "prompt": "continue",
            "cwd": str(workdir),
            "session": "earlier-id",
            "env": _env(tmp_path, workdir),
        },
        timeout_seconds=30,
    )
    (call,) = _calls(tmp_path)
    assert call["argv"][call["argv"].index("--session") + 1] == "earlier-id"
    assert result.raw["session_id"] == "earlier-id"


# ---------- claude_code ----------


def test_claude_code_runs_stream_json_with_its_tool_lists_and_stdin_prompt(
    tmp_path: Path, workdir: Path
) -> None:
    plugin = _plugin(tmp_path, "claude_code")
    result = plugin.execute(
        params={
            "prompt": "First line.\nSecond line.",
            "cwd": str(workdir),
            "model": "opus",
            "permission_mode": "acceptEdits",
            "tools": ["Read", "Bash(git log:*)"],
            "exclude_tools": ["WebFetch"],
            "env": _env(tmp_path, workdir),
        },
        timeout_seconds=30,
    )
    assert result.ok
    assert result.value == "all done"
    raw = result.raw
    assert (raw["engine"], raw["session_id"]) == ("claude_code", "sess-abc")
    assert (raw["turns"], raw["tool_calls"]) == (2, 1)
    assert raw["tokens"] == {"sent": 7, "received": 3}
    assert raw["cost"] == pytest.approx(0.02)
    (call,) = _calls(tmp_path)
    argv = call["argv"]
    assert argv[argv.index("--output-format") + 1] == "stream-json"
    assert "--verbose" in argv
    assert argv[argv.index("--permission-mode") + 1] == "acceptEdits"
    allowed = argv.index("--allowedTools")
    assert argv[allowed + 1 : allowed + 3] == ["Read", "Bash(git log:*)"]
    assert argv[argv.index("--disallowedTools") + 1] == "WebFetch"
    assert "--tools" not in argv and "--no-session-persistence" not in argv
    assert call["stdin"] == "First line.\nSecond line."
    assert not any("First line" in arg for arg in argv)


def test_claude_code_repair_turn_resumes_the_session(tmp_path: Path, workdir: Path) -> None:
    plugin = _plugin(tmp_path, "claude_code")
    result = plugin.execute(
        params={
            "prompt": "do it",
            "cwd": str(workdir),
            "env": _env(
                tmp_path, workdir,
                FAKE_RESULT_FIRST="{not json",
                FAKE_RESULT_REPAIR=json.dumps([1, 2]),
            ),
            "result_file": "result.json",
        },
        timeout_seconds=30,
    )
    assert result.ok, result.stderr
    assert result.value == [1, 2]
    first, repair = _calls(tmp_path)
    assert "--resume" not in first["argv"]
    assert repair["argv"][repair["argv"].index("--resume") + 1] == "sess-abc"
    assert "is not valid JSON" in repair["stdin"]
    assert result.raw["tokens"] == {"sent": 14, "received": 6}


def test_claude_code_session_param_resumes_that_session(tmp_path: Path, workdir: Path) -> None:
    plugin = _plugin(tmp_path, "claude_code")
    plugin.execute(
        params={
            "prompt": "go on",
            "cwd": str(workdir),
            "session": "abc-123",
            "env": _env(tmp_path, workdir),
        },
        timeout_seconds=30,
    )
    (call,) = _calls(tmp_path)
    assert call["argv"][call["argv"].index("--resume") + 1] == "abc-123"


# ---------- timeout and cancellation ----------


@pytest.mark.skipif(os.name != "posix", reason="process groups are POSIX-only")
@pytest.mark.parametrize("engine", ["pi", "claude_code"])
def test_timeout_stops_the_cli_and_the_child_it_started(
    tmp_path: Path, workdir: Path, engine: str
) -> None:
    plugin = _plugin(tmp_path, engine, body=SPAWN_CHILD_AND_HANG)
    pid_file = tmp_path / "child.pid"
    started = time.monotonic()
    with pytest.raises(AgentCliTimeout, match="did not finish within 2s"):
        plugin.execute(
            params={
                "prompt": "hang",
                "cwd": str(workdir),
                "env": {"FAKE_CLI_CHILD_PID": str(pid_file)},
            },
            timeout_seconds=2,
        )
    assert time.monotonic() - started < 20
    assert wait_until_dead(int(pid_file.read_text())), "the agent's own child survived"
    assert list((tmp_path / "scratch").iterdir()) == []


@pytest.mark.skipif(os.name != "posix", reason="process groups are POSIX-only")
def test_cancelled_run_stops_the_session_and_its_child(
    tmp_path: Path, workdir: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    token = CancellationToken()
    monkeypatch.setattr(cancellation, "_token", token)
    plugin = _plugin(tmp_path, "pi", body=SPAWN_CHILD_AND_HANG)
    pid_file = tmp_path / "child.pid"
    outcome: list[BaseException] = []

    def _call() -> None:
        try:
            plugin.execute(
                params={
                    "prompt": "hang",
                    "cwd": str(workdir),
                    "env": {"FAKE_CLI_CHILD_PID": str(pid_file)},
                },
                timeout_seconds=60,
            )
        except BaseException as exc:
            outcome.append(exc)

    thread = threading.Thread(target=_call)
    thread.start()
    deadline = time.monotonic() + 20
    while not (pid_file.exists() and pid_file.read_text()) and time.monotonic() < deadline:
        time.sleep(0.05)
    child_pid = int(pid_file.read_text())
    assert pid_alive(child_pid)
    token.request()
    thread.join(timeout=20)
    assert not thread.is_alive()
    assert len(outcome) == 1 and isinstance(outcome[0], RunCancelledBySignal)
    assert wait_until_dead(child_pid), "the agent's own child survived the cancellation"


# ---------- params, config, preflight, capabilities ----------


@pytest.mark.parametrize(
    ("params", "message"),
    [
        ({}, "requires params\\['prompt'\\]"),
        ({"prompt": "x", "engine": "codex"}, "unknown engine 'codex'"),
        ({"prompt": "x", "engine": "claude_code", "thinking": "high"}, "'pi' only"),
        ({"prompt": "x", "permission_mode": "plan"}, "'claude_code' only"),
        ({"prompt": "x", "result_schema": {"type": "object"}}, "needs params\\['result_file'\\]"),
        ({"prompt": "x", "result_file": "r.json", "result_schema": {"type": 5}}, "not a valid JSON Schema"),
        ({"prompt": "x", "tools": "read,write"}, "'tools'\\] must be a list of strings"),
        ({"prompt": "x", "cwd": "/no/such/dir/anywhere"}, "is not a directory"),
    ],
)
def test_invalid_params_are_rejected_before_the_cli_runs(
    tmp_path: Path, params: dict[str, Any], message: str
) -> None:
    plugin = _plugin(tmp_path, "pi")
    with pytest.raises(ValueError, match=message):
        plugin.execute(params={**params, "env": {"FAKE_AGENT_LOG": str(tmp_path / "calls.jsonl")}})
    assert _calls(tmp_path) == []


def test_config_sets_the_default_engine_and_each_binary(tmp_path: Path) -> None:
    plugin = build_plugin(
        plugin_name="agent",
        runtime={"plugins": {"agent": {"engine": "claude_code", "claude_code": {"binary": "/x/claude"}}}},
    )
    assert isinstance(plugin, AgentPlugin)
    assert plugin.default_engine == "claude_code"
    assert plugin.binaries == {"claude_code": "/x/claude"}
    with pytest.raises(ValueError, match=r"runtime\.plugins\.agent\.engine"):
        make_plugin({"engine": "codex"})


def test_missing_configured_binary_names_the_setting(tmp_path: Path) -> None:
    plugin = AgentPlugin(binaries={"pi": str(tmp_path / "nope")})
    with pytest.raises(RuntimeError, match=r"runtime\.plugins\.agent\.pi\.binary"):
        plugin.execute(params={"prompt": "x", "cwd": str(tmp_path)})


def test_check_is_ok_when_one_engine_is_available(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("PATH", str(tmp_path / "empty"))
    nothing = AgentPlugin().check()
    assert not nothing.ok
    assert nothing.missing == ["binary:pi", "binary:claude"]
    fake = write_fake_cli(tmp_path, "claude")
    one = AgentPlugin(binaries={"claude_code": str(fake)}).check()
    assert one.ok and one.missing == []
    assert one.message is not None and str(fake) in one.message


def test_agent_needs_shell_fs_write_and_network_consent() -> None:
    assert capabilities_of("agent") == frozenset({SHELL, FS_WRITE, NETWORK})
