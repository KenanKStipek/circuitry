"""`circuitry.agent_cli` (#366): the child environment, both engines'
commands, the process-group run with its timeout and cancellation, and
both output formats. Every CLI here is a fake script."""

from __future__ import annotations

import json
import os
import subprocess
import threading
import time
from pathlib import Path

import pytest
from agent_cli_test_support import (
    SPAWN_CHILD_AND_HANG,
    claude_result,
    pi_events,
    pid_alive,
    read_record,
    wait_until_dead,
    write_fake_cli,
)

from circuitry import agent_cli
from circuitry.agent_cli import (
    DEFAULT_UNSET_ENV,
    PARENT_SESSION_ENV,
    AgentCliError,
    AgentCliTimeout,
    child_env,
    claude_command,
    parse_claude_output,
    parse_pi_output,
    pi_command,
    run_agent_cli,
)
from circuitry.core import cancellation
from circuitry.core.cancellation import CancellationToken, RunCancelledBySignal


def _completed(stdout: str, *, returncode: int = 0, stderr: str = "") -> subprocess.CompletedProcess[str]:
    return subprocess.CompletedProcess(["cli"], returncode, stdout, stderr)


# ---------- child_env ----------


def test_child_env_drops_parent_session_and_unset_env_and_keeps_the_rest() -> None:
    base = dict.fromkeys(PARENT_SESSION_ENV, "x")
    base.update({"ANTHROPIC_API_KEY": "k", "ANTHROPIC_AUTH_TOKEN": "t", "PATH": "/bin", "HOME": "/h"})
    assert child_env(base=base) == {"PATH": "/bin", "HOME": "/h"}


def test_child_env_unset_env_replaces_the_default_list() -> None:
    base = {"ANTHROPIC_API_KEY": "k", "OTHER_KEY": "o", "CLAUDECODE": "1"}
    assert child_env(["OTHER_KEY"], base=base) == {"ANTHROPIC_API_KEY": "k"}
    assert child_env([], base=base) == {"ANTHROPIC_API_KEY": "k", "OTHER_KEY": "o"}


def test_child_env_defaults_to_os_environ(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("PI_SESSION_ID", "parent")
    monkeypatch.setenv("ANTHROPIC_API_KEY", "key")
    monkeypatch.setenv("CIRCUITRY_AGENT_CLI_KEEP", "kept")
    env = child_env()
    assert "PI_SESSION_ID" not in env
    assert "ANTHROPIC_API_KEY" not in env
    assert env["CIRCUITRY_AGENT_CLI_KEEP"] == "kept"
    assert DEFAULT_UNSET_ENV == ("ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN")


# ---------- commands ----------


def test_pi_command_defaults_to_one_toolless_unsaved_call() -> None:
    assert pi_command(binary="pi", prompt_file="/w/prompt.md") == [
        "pi", "-p", "--mode", "json", "--no-approve", "--no-tools", "--no-session",
        "@/w/prompt.md", agent_cli.PI_ATTACHED_PROMPT_INSTRUCTION,
    ]  # fmt: skip


def test_pi_command_with_model_thinking_session_tools_and_extra_args() -> None:
    cmd = pi_command(
        binary="/opt/pi",
        prompt_file="/w/p.md",
        model="claude-bridge/claude-sonnet-4-5",
        thinking="high",
        tools=True,
        session_id="abc",
        extra_args=["--verbose"],
        instruction="go",
    )
    assert cmd == [
        "/opt/pi", "-p", "--mode", "json", "--no-approve", "--session", "abc",
        "--model", "claude-bridge/claude-sonnet-4-5", "--thinking", "high", "--verbose",
        "@/w/p.md", "go",
    ]  # fmt: skip


def test_pi_command_persist_session_drops_no_session() -> None:
    assert "--no-session" not in pi_command(binary="pi", prompt_file="p", persist_session=True)


def test_claude_command_defaults_to_one_toolless_unsaved_call() -> None:
    assert claude_command(binary="claude") == [
        "claude", "-p", "--output-format", "json", "--tools", "", "--no-session-persistence",
    ]  # fmt: skip


def test_claude_command_with_model_schema_session_tools_and_extra_args() -> None:
    schema = {"type": "object", "properties": {"a": {"type": "integer"}}}
    cmd = claude_command(
        binary="claude",
        model="claude-sonnet-4-5",
        json_schema=schema,
        tools=True,
        session_id="s1",
        extra_args=["--max-turns", "3"],
    )
    assert cmd == [
        "claude", "-p", "--output-format", "json", "--resume", "s1",
        "--model", "claude-sonnet-4-5", "--json-schema", json.dumps(schema),
        "--max-turns", "3",
    ]  # fmt: skip


# ---------- run_agent_cli ----------


def test_run_agent_cli_passes_stdin_env_and_cwd(tmp_path: Path) -> None:
    record = tmp_path / "record.json"
    fake = write_fake_cli(tmp_path, "fake", stdout="out", stderr="err", exit_code=3)
    workdir = tmp_path / "work"
    workdir.mkdir()
    proc = run_agent_cli(
        [str(fake), "a"],
        env={"FAKE_CLI_RECORD": str(record), "ONLY": "this"},
        timeout_seconds=30,
        input="the prompt",
        cwd=str(workdir),
    )
    assert (proc.returncode, proc.stdout, proc.stderr) == (3, "out", "err")
    seen = read_record(record)
    assert seen["argv"] == ["a"]
    assert seen["stdin"] == "the prompt"
    assert Path(seen["cwd"]).resolve() == workdir.resolve()
    assert seen["env"]["ONLY"] == "this"
    assert "PATH" not in seen["env"]


def test_run_agent_cli_missing_binary_is_an_agent_cli_error(tmp_path: Path) -> None:
    with pytest.raises(AgentCliError, match="could not be started"):
        run_agent_cli([str(tmp_path / "nope")], env={}, timeout_seconds=5)


@pytest.mark.skipif(os.name != "posix", reason="process groups are POSIX-only")
def test_run_agent_cli_timeout_kills_the_whole_process_group(tmp_path: Path) -> None:
    pid_file = tmp_path / "child.pid"
    fake = write_fake_cli(tmp_path, "fake", body=SPAWN_CHILD_AND_HANG)
    env = {**os.environ, "FAKE_CLI_CHILD_PID": str(pid_file)}
    started = time.monotonic()
    with pytest.raises(AgentCliTimeout, match="did not finish within 2s"):
        run_agent_cli([str(fake)], env=env, timeout_seconds=2)
    assert time.monotonic() - started < 20
    child_pid = int(pid_file.read_text())
    assert wait_until_dead(child_pid), "the CLI's own child survived the timeout"


@pytest.mark.skipif(os.name != "posix", reason="process groups are POSIX-only")
def test_run_agent_cli_cancellation_kills_the_group_and_raises(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    token = CancellationToken()
    monkeypatch.setattr(cancellation, "_token", token)
    pid_file = tmp_path / "child.pid"
    fake = write_fake_cli(tmp_path, "fake", body=SPAWN_CHILD_AND_HANG)
    env = {**os.environ, "FAKE_CLI_CHILD_PID": str(pid_file)}
    outcome: list[BaseException] = []

    def _call() -> None:
        try:
            run_agent_cli([str(fake)], env=env, timeout_seconds=60)
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
    assert wait_until_dead(child_pid), "the CLI's own child survived the cancellation"


# ---------- parse_pi_output ----------


def test_parse_pi_output_reads_the_last_assistant_text_usage_and_cost() -> None:
    usage = {"input": 10, "output": 5, "cacheRead": 1000, "cacheWrite": 100, "totalTokens": 1115,
             "cost": {"input": 0.01, "output": 0.02, "total": 0.03}}  # fmt: skip
    result = parse_pi_output(_completed(pi_events("  the answer \n", usage=usage)))
    assert result.text == "the answer"
    assert result.session_id == "sess-1"
    assert (result.tokens_sent, result.tokens_received) == (1110, 5)
    assert result.cost_usd == pytest.approx(0.03)
    assert result.finish_reason == "stop"
    assert result.raw["events"][0]["type"] == "session"


def test_parse_pi_output_takes_a_plain_number_cost_and_sums_turns() -> None:
    turn = {"role": "assistant", "content": [{"type": "toolCall", "name": "x"}],
            "stopReason": "toolUse", "usage": {"input": 1, "output": 2, "cost": 0.5}}  # fmt: skip
    stream = json.dumps({"type": "message_end", "message": turn}) + "\n"
    stream += pi_events("done", usage={"input": 3, "output": 4, "cost": 0.25})
    result = parse_pi_output(_completed(stream))
    assert result.text == "done"
    assert (result.tokens_sent, result.tokens_received) == (4, 6)
    assert result.cost_usd == pytest.approx(0.75)


def test_parse_pi_output_without_usage_reports_none() -> None:
    result = parse_pi_output(_completed(pi_events("x")))
    assert (result.tokens_sent, result.tokens_received, result.cost_usd) == (None, None, None)


def test_parse_pi_output_auth_failure_carries_the_cli_text() -> None:
    text = "Failed to authenticate. API Error: 401 invalid credentials"
    with pytest.raises(AgentCliError, match=r"pi failed \(error\): Failed to authenticate\. API Error: 401"):
        parse_pi_output(_completed(pi_events(text, stop_reason="error"), returncode=1))


def test_parse_pi_output_prefers_error_message() -> None:
    stream = pi_events("", stop_reason="error", error_message="model not found")
    with pytest.raises(AgentCliError, match="model not found"):
        parse_pi_output(_completed(stream))


def test_parse_pi_output_without_an_answer_reports_exit_code_and_stderr() -> None:
    with pytest.raises(AgentCliError, match="pi exited with code 2 without an answer: bad flag"):
        parse_pi_output(_completed("not json\n", returncode=2, stderr="bad flag\n"))


# ---------- parse_claude_output ----------


def test_parse_claude_output_reads_text_usage_cost_and_session() -> None:
    result = parse_claude_output(_completed(claude_result(result=" hi ")))
    assert result.text == " hi "
    assert result.session_id == "sess-2"
    assert (result.tokens_sent, result.tokens_received) == (1110, 5)
    assert result.cost_usd == pytest.approx(0.0123)
    assert result.structured_output is None


def test_parse_claude_output_uses_structured_output() -> None:
    result = parse_claude_output(_completed(claude_result(result="", structured_output={"a": 1})))
    assert result.structured_output == {"a": 1}
    assert json.loads(result.text) == {"a": 1}


def test_parse_claude_output_is_error_passes_the_cli_message_through() -> None:
    message = (
        "Claude Code 2.1.12 does not support this model; version 2.1.280 or newer is required"
    )
    stdout = claude_result(is_error=True, subtype="success", result=message)
    with pytest.raises(AgentCliError, match=r"version 2\.1\.280 or newer is required"):
        parse_claude_output(_completed(stdout, returncode=1))


def test_parse_claude_output_nonzero_exit_without_json() -> None:
    with pytest.raises(AgentCliError, match="claude exited with code 1 without an answer: boom"):
        parse_claude_output(_completed("", returncode=1, stderr="boom"))
