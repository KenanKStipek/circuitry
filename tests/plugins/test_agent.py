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

from circuitry.agent_cli import AgentCliError, AgentCliTimeout
from circuitry.core import cancellation
from circuitry.core.cancellation import CancellationToken, RunCancelledBySignal
from circuitry.plugins.agent import (
    AgentPlugin,
    AgentTurn,
    make_plugin,
    run_agent_session,
)
from circuitry.plugins.base import ToolResult
from circuitry.plugins.capabilities import FS_WRITE, NETWORK, SHELL, capabilities_of
from circuitry.plugins.factory import build_plugin

#: Logs argv, stdin, attachments and selected env to ``FAKE_AGENT_LOG`` (one
#: JSON line per call). On a first call it writes ``FAKE_RESULT_FIRST`` to
#: ``FAKE_RESULT_PATH``, on a resumed call ``FAKE_RESULT_REPAIR`` (an unset
#: variable writes nothing). A resumed call hangs when
#: ``FAKE_HANG_ON_RESUME`` is set. Then prints pi's event stream (``--mode``
#: in argv) or Claude Code's stream-json, with one tool call and one reply.
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
if resumed and os.environ.get("FAKE_HANG_ON_RESUME"):
    import time
    time.sleep(60)
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


def _run_claude(
    tmp_path: Path,
    workdir: Path,
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    **params: Any,
) -> ToolResult:
    return _plugin(tmp_path, "claude_code").execute(
        params={
            "prompt": "Task.",
            "cwd": str(cwd or workdir),
            "env": env or _env(tmp_path, workdir),
            **params,
        },
        timeout_seconds=30,
    )


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
    assert argv[argv.index("--tools") + 1] == "Read,Bash"
    assert "--no-session-persistence" not in argv
    assert call["stdin"] == "First line.\nSecond line."
    assert not any("First line" in arg for arg in argv)


def test_claude_code_isolates_repository_settings_by_default(
    tmp_path: Path, workdir: Path
) -> None:
    result = _run_claude(tmp_path, workdir)
    assert result.ok, result.stderr
    (call,) = _calls(tmp_path)
    argv = call["argv"]
    setting = argv.index("--setting-sources")
    assert argv[setting + 1] == "user"
    assert "--strict-mcp-config" in argv
    assert not {"--tools", "--allowedTools", "--permission-mode"} & set(argv)
    assert call["stdin"] == "Task."
    assert result.raw["project_settings"] == "isolated"
    assert result.raw["project_instructions"] is None
    assert result.raw["permission_mode"] is None


_PROJECT_SECTION = (
    "\n\n---\n\nProject instructions from the repository's CLAUDE.md "
    "(Claude Code does not load it in this session):\n\n"
)


def test_claude_code_appends_the_repository_claude_md_to_the_first_prompt(
    tmp_path: Path, workdir: Path
) -> None:
    (workdir / ".git").mkdir()
    (workdir / "CLAUDE.md").write_text("Use pytest.\n", encoding="utf-8")
    result = _run_claude(
        tmp_path,
        workdir,
        result_file="result.json",
        result_schema=SCHEMA,
        env=_env(
            tmp_path, workdir, FAKE_RESULT_FIRST=json.dumps({"summary": "ok", "files": 1})
        ),
    )
    assert result.ok, result.stderr
    (call,) = _calls(tmp_path)
    stdin = call["stdin"]
    assert stdin.startswith("Task." + _PROJECT_SECTION + "Use pytest.\n")
    prompt_at = stdin.index("Task.")
    section_at = stdin.index(_PROJECT_SECTION)
    contract_at = stdin.index("When you have finished")
    assert prompt_at < section_at < contract_at
    assert result.raw["project_instructions"] == str((workdir / "CLAUDE.md").resolve())


def test_claude_code_appends_the_claude_md_to_the_prompt_without_a_result_file(
    tmp_path: Path, workdir: Path
) -> None:
    (workdir / ".git").mkdir()
    (workdir / "CLAUDE.md").write_text("Use pytest.\n", encoding="utf-8")
    result = _run_claude(tmp_path, workdir)
    assert result.ok, result.stderr
    (call,) = _calls(tmp_path)
    assert call["stdin"] == "Task." + _PROJECT_SECTION + "Use pytest.\n"


def test_claude_code_appends_a_claude_md_that_is_not_utf8_with_replacement_characters(
    tmp_path: Path, workdir: Path
) -> None:
    (workdir / ".git").mkdir()
    (workdir / "CLAUDE.md").write_bytes(b"Use \xff pytest.\n")
    result = _run_claude(tmp_path, workdir)
    assert result.ok, result.stderr
    (call,) = _calls(tmp_path)
    assert call["stdin"] == "Task." + _PROJECT_SECTION + "Use \ufffd pytest.\n"


@pytest.mark.parametrize(
    "case",
    ["directory", "empty", "whitespace", "dangling_symlink", "unreadable", "subdirectory_only"],
)
def test_claude_code_runs_without_appending_a_root_claude_md_it_cannot_use(
    tmp_path: Path, workdir: Path, case: str
) -> None:
    (workdir / ".git").mkdir()
    cwd = workdir
    if case == "directory":
        (workdir / "CLAUDE.md").mkdir()
    elif case == "empty":
        (workdir / "CLAUDE.md").write_text("", encoding="utf-8")
    elif case == "whitespace":
        (workdir / "CLAUDE.md").write_text(" \n\t\n", encoding="utf-8")
    elif case == "dangling_symlink":
        (workdir / "CLAUDE.md").symlink_to(tmp_path / "missing.md")
    elif case == "unreadable":
        if os.name != "posix" or os.geteuid() == 0:
            pytest.skip("needs a POSIX user that cannot read a file it owns")
        unreadable = workdir / "CLAUDE.md"
        unreadable.write_text("Use pytest.\n", encoding="utf-8")
        unreadable.chmod(0)
    else:
        cwd = workdir / "sub"
        cwd.mkdir()
        (cwd / "CLAUDE.md").write_text("Use pytest.\n", encoding="utf-8")
    result = _run_claude(tmp_path, workdir, cwd=cwd)
    assert result.ok, result.stderr
    (call,) = _calls(tmp_path)
    assert call["stdin"] == "Task."
    assert result.raw["project_instructions"] is None


@pytest.mark.parametrize(
    ("value", "trusted"),
    [("false", False), (" FALSE ", False), ("true", True), (" True ", True)],
)
def test_claude_code_trust_project_settings_accepts_the_strings_true_and_false(
    tmp_path: Path, workdir: Path, value: str, trusted: bool
) -> None:
    result = _run_claude(tmp_path, workdir, trust_project_settings=value)
    assert result.ok, result.stderr
    (call,) = _calls(tmp_path)
    assert ("--setting-sources" not in call["argv"]) is trusted
    assert result.raw["project_settings"] == ("trusted" if trusted else "isolated")


def test_claude_code_repair_turn_does_not_repeat_the_claude_md(
    tmp_path: Path, workdir: Path
) -> None:
    (workdir / ".git").mkdir()
    (workdir / "CLAUDE.md").write_text("Use pytest.\n", encoding="utf-8")
    result = _run_claude(
        tmp_path,
        workdir,
        result_file="result.json",
        env=_env(
            tmp_path, workdir, FAKE_RESULT_FIRST="{not json", FAKE_RESULT_REPAIR=json.dumps([1])
        ),
    )
    assert result.ok, result.stderr
    first, repair = _calls(tmp_path)
    assert "Use pytest." in first["stdin"]
    assert "Use pytest." not in repair["stdin"]
    assert "is not valid JSON" in repair["stdin"]


@pytest.mark.parametrize("git_entry", ["directory", "file"])
def test_claude_code_finds_the_root_claude_md_from_a_subdirectory(
    tmp_path: Path, workdir: Path, git_entry: str
) -> None:
    if git_entry == "directory":
        (workdir / ".git").mkdir()
    else:
        (workdir / ".git").write_text("gitdir: ../.git/worktrees/work\n", encoding="utf-8")
    (workdir / "CLAUDE.md").write_text("Use pytest.\n", encoding="utf-8")
    sub = workdir / "sub"
    sub.mkdir()
    result = _run_claude(tmp_path, workdir, cwd=sub)
    assert result.ok, result.stderr
    (call,) = _calls(tmp_path)
    assert call["stdin"].startswith("Task.")
    assert "Use pytest." in call["stdin"]
    assert result.raw["project_instructions"] == str((workdir / "CLAUDE.md").resolve())


def test_claude_code_does_not_append_a_claude_md_linked_from_outside_the_repository(
    tmp_path: Path, workdir: Path
) -> None:
    (workdir / ".git").mkdir()
    outside = tmp_path / "outside.md"
    outside.write_text("Secret.\n", encoding="utf-8")
    (workdir / "CLAUDE.md").symlink_to(outside)
    result = _run_claude(tmp_path, workdir)
    assert result.ok, result.stderr
    (call,) = _calls(tmp_path)
    assert call["stdin"] == "Task."
    assert result.raw["project_instructions"] is None


def test_claude_code_trusted_settings_keep_its_own_discovery(
    tmp_path: Path, workdir: Path
) -> None:
    (workdir / ".git").mkdir()
    (workdir / "CLAUDE.md").write_text("Use pytest.\n", encoding="utf-8")
    result = _run_claude(tmp_path, workdir, trust_project_settings=True)
    assert result.ok, result.stderr
    (call,) = _calls(tmp_path)
    assert "--setting-sources" not in call["argv"]
    assert "--strict-mcp-config" not in call["argv"]
    assert call["stdin"] == "Task."
    assert result.raw["project_settings"] == "trusted"
    assert result.raw["project_instructions"] is None


def test_claude_code_tools_are_an_allowlist_that_runs_under_dontask(
    tmp_path: Path, workdir: Path
) -> None:
    tools = ["Read", "Edit", "Bash(git log:*)", "Bash(pytest:*)", "mcp__srv__ping"]
    result = _run_claude(tmp_path, workdir, tools=tools)
    assert result.ok, result.stderr
    (call,) = _calls(tmp_path)
    argv = call["argv"]
    assert argv[argv.index("--tools") + 1] == "Read,Edit,Bash"
    allowed = argv.index("--allowedTools")
    assert argv[allowed + 1 : allowed + 1 + len(tools)] == tools
    assert argv[argv.index("--permission-mode") + 1] == "dontAsk"
    assert result.raw["permission_mode"] == "dontAsk"


def test_claude_code_only_mcp_tools_leave_no_builtin_tool(tmp_path: Path, workdir: Path) -> None:
    result = _run_claude(tmp_path, workdir, tools=["mcp__srv__ping"])
    assert result.ok, result.stderr
    (call,) = _calls(tmp_path)
    argv = call["argv"]
    assert argv[argv.index("--tools") + 1] == ""
    assert argv[argv.index("--allowedTools") + 1] == "mcp__srv__ping"


def test_claude_code_explicit_permission_mode_replaces_dontask(
    tmp_path: Path, workdir: Path
) -> None:
    result = _run_claude(tmp_path, workdir, tools=["Read"], permission_mode="acceptEdits")
    assert result.ok, result.stderr
    (call,) = _calls(tmp_path)
    argv = call["argv"]
    assert argv[argv.index("--permission-mode") + 1] == "acceptEdits"
    assert "dontAsk" not in argv
    assert result.raw["permission_mode"] == "acceptEdits"


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


def test_claude_code_result_still_invalid_after_repair_fails_with_the_errors(
    tmp_path: Path, workdir: Path
) -> None:
    plugin = _plugin(tmp_path, "claude_code")
    result = plugin.execute(
        params={
            "prompt": "do it",
            "cwd": str(workdir),
            "env": _env(tmp_path, workdir, FAKE_RESULT_REPAIR=json.dumps({"summary": 3})),
            "result_file": "result.json",
            "result_schema": SCHEMA,
        },
        timeout_seconds=30,
    )
    assert not result.ok
    assert result.stderr is not None
    assert "still invalid (after one repair turn)" in result.stderr
    assert "summary: 3 is not of type 'string'" in result.stderr
    first, repair = _calls(tmp_path)
    assert "--resume" not in first["argv"]
    assert repair["argv"][repair["argv"].index("--resume") + 1] == "sess-abc"
    assert "the file was not written" in repair["stdin"]
    assert result.raw["session_id"] == "sess-abc"
    assert result.raw["repair_turn"] is True


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


# ---------- the repair turn, through a fake engine ----------


class _FakeEngine:
    """An engine whose turns report ``session_id`` and record their prompts."""

    name = "fake"

    def __init__(self, session_id: str | None) -> None:
        self.session_id = session_id
        self.prompts: list[str] = []

    def run_turn(
        self, prompt: str, *, session_id: str | None, timeout_seconds: float, scratch_dir: Path
    ) -> AgentTurn:
        self.prompts.append(prompt)
        return AgentTurn(text="done", session_id=self.session_id, turns=1, tool_calls=0)


def test_no_repair_turn_without_a_session_id_to_resume(tmp_path: Path) -> None:
    engine = _FakeEngine(session_id=None)
    result = run_agent_session(
        engine,
        "do it",
        session_id=None,
        result_file=tmp_path / "result.json",
        result_schema=None,
        timeout_seconds=30,
        scratch_dir=tmp_path,
    )
    assert not result.ok
    assert result.stderr is not None
    assert "no session id to resume" in result.stderr
    assert "the file was not written" in result.stderr
    assert len(engine.prompts) == 1
    assert result.raw["repair_turn"] is False


def test_no_repair_turn_when_no_time_is_left(tmp_path: Path) -> None:
    engine = _FakeEngine(session_id="sess-1")
    result = run_agent_session(
        engine,
        "do it",
        session_id=None,
        result_file=tmp_path / "result.json",
        result_schema=None,
        timeout_seconds=0.5,
        scratch_dir=tmp_path,
    )
    assert not result.ok
    assert result.stderr is not None
    assert "no time was left for a repair turn" in result.stderr
    assert len(engine.prompts) == 1
    assert result.raw["session_id"] == "sess-1"


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


@pytest.mark.parametrize("engine", ["pi", "claude_code"])
def test_repair_turn_timeout_names_the_session_and_keeps_the_transcript(
    tmp_path: Path, workdir: Path, engine: str
) -> None:
    plugin = _plugin(tmp_path, engine)
    with pytest.raises(AgentCliTimeout) as caught:
        plugin.execute(
            params={
                "prompt": "do it",
                "cwd": str(workdir),
                "env": _env(tmp_path, workdir, FAKE_HANG_ON_RESUME="1"),
                "result_file": "result.json",
            },
            timeout_seconds=4,
        )
    message = str(caught.value)
    assert "session sess-abc can be resumed with params['session']" in message
    (scratch,) = (tmp_path / "scratch").iterdir()
    transcript = scratch / "transcript.log"
    assert f"transcript: {transcript}" in message
    assert "reply all done" in transcript.read_text(encoding="utf-8")


def test_cli_error_with_nothing_to_resume_leaves_no_scratch_dir(
    tmp_path: Path, workdir: Path
) -> None:
    plugin = _plugin(tmp_path, "pi", body="sys.exit(3)")
    with pytest.raises(AgentCliError) as caught:
        plugin.execute(params={"prompt": "x", "cwd": str(workdir)}, timeout_seconds=30)
    assert "can be resumed" not in str(caught.value)
    assert list((tmp_path / "scratch").iterdir()) == []


@pytest.mark.skipif(os.name != "posix", reason="process groups are POSIX-only")
@pytest.mark.parametrize("engine", ["pi", "claude_code"])
def test_cancelled_run_stops_the_session_and_its_child(
    tmp_path: Path, workdir: Path, monkeypatch: pytest.MonkeyPatch, engine: str
) -> None:
    token = CancellationToken()
    monkeypatch.setattr(cancellation, "_token", token)
    plugin = _plugin(tmp_path, engine, body=SPAWN_CHILD_AND_HANG)
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
        ({"prompt": "x", "env": {"OTHER": None}}, "is null; unset_env removes"),
        ({"prompt": "x", "trust_project_settings": True}, "'claude_code' only"),
        ({"prompt": "x", "engine": "claude_code", "trust_project_settings": "yes"}, "must be true or false"),
        ({"prompt": "x", "engine": "claude_code", "trust_project_settings": "none"}, "must be true or false"),
        ({"prompt": "x", "tools": []}, "'tools'\\] is empty"),
        ({"prompt": "x", "engine": "claude_code", "tools": []}, "'tools'\\] is empty"),
        ({"prompt": "x", "tools": ["read", " "]}, "'tools'\\]\\[1\\] is blank"),
        ({"prompt": "x", "engine": "claude_code", "tools": ["Read", ""]}, "'tools'\\]\\[1\\] is blank"),
        ({"prompt": "x", "exclude_tools": ["read", ""]}, "'exclude_tools'\\]\\[1\\] is blank"),
        ({"prompt": "x", "engine": "claude_code", "exclude_tools": [" "]}, "'exclude_tools'\\]\\[0\\] is blank"),
    ],
)
def test_invalid_params_are_rejected_before_the_cli_runs(
    tmp_path: Path, params: dict[str, Any], message: str
) -> None:
    plugin = _plugin(tmp_path, "pi")
    with pytest.raises(ValueError, match=message):
        log = {"FAKE_AGENT_LOG": str(tmp_path / "calls.jsonl")}
        plugin.execute(params={**params, "env": {**log, **params.get("env", {})}})
    assert _calls(tmp_path) == []


def test_result_file_that_is_a_directory_is_rejected_before_the_cli_runs(
    tmp_path: Path, workdir: Path
) -> None:
    (workdir / "result.json").mkdir()
    plugin = _plugin(tmp_path, "pi")
    with pytest.raises(ValueError, match=r"agent: result_file .* could not be cleared"):
        plugin.execute(
            params={"prompt": "x", "cwd": str(workdir), "env": _env(tmp_path, workdir),
                    "result_file": "result.json"},
        )
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


def test_tool_effect_lands_value_and_raw_in_state_without_the_transcript(
    tmp_path: Path, workdir: Path
) -> None:
    from unittest.mock import MagicMock

    from circuitry.core.compiler import compile_orchestration
    from circuitry.core.dynamic import DynamicRuntime
    from circuitry.core.store import Store

    fake = write_fake_cli(tmp_path, "pi", body=FAKE_AGENT_BODY)
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "delegate",
                "provider": "agent",
                "timeout_ms": 30000,
                "params": {
                    "prompt": "Task: {{input.task}}",
                    "cwd": str(workdir),
                    "env": _env(tmp_path, workdir, FAKE_RESULT_FIRST='{"summary": "s", "files": 3}'),
                    "result_file": "result.json",
                    "result_schema": SCHEMA,
                },
            }
        ]
    }
    store = Store(state={"input": {"task": "rename the module"}})
    DynamicRuntime(
        compile_orchestration(orch=orch),
        adapter=MagicMock(),
        model="test",
        runtime_config={"plugins": {"agent": {"pi": {"binary": str(fake)}}}},
    ).execute(store=store)
    node = store.state["prime"]["delegate"]
    assert node["meta"]["error"] is None
    assert node["value"] == {"summary": "s", "files": 3}
    assert node["meta"]["raw"]["session_id"] == "sess-abc"
    assert node["meta"]["raw"]["tool_calls"] == 1
    assert "Task: rename the module" in _calls(tmp_path)[0]["attachments"][0]
    assert "reply all done" not in json.dumps(store.state["prime"], default=str)
    assert "reply all done" in Path(node["meta"]["raw"]["transcript"]).read_text()
