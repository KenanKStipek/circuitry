"""The `pi` and `claude_code` adapters (#366), driven through real prompt
effects against fake CLI scripts set as `runtime.adapters.<name>.binary`.
No test runs a real CLI or calls a real model."""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any

import pytest
import typer.testing
import yaml
from agent_cli_test_support import (
    SPAWN_CHILD_AND_HANG,
    claude_result,
    pi_events,
    read_record,
    wait_until_dead,
    write_fake_cli,
)

from circuitry.adapters._retry import AdapterCallError
from circuitry.adapters.base import GenerateOptions
from circuitry.adapters.claude_code import ClaudeCodeAdapter
from circuitry.adapters.factory import build_adapter
from circuitry.adapters.pi import PiAdapter
from circuitry.api import run_orchestration
from circuitry.cli.app import app
from circuitry.cli.config import CircuitryConfig

SCHEMA = {
    "type": "object",
    "properties": {"count": {"type": "integer"}},
    "required": ["count"],
}

runner = typer.testing.CliRunner()


@pytest.fixture
def record(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    path = tmp_path / "record.json"
    monkeypatch.setenv("FAKE_CLI_RECORD", str(path))
    return path


def _run(*args: Any, **kwargs: Any) -> dict[str, Any]:
    return _run_state(*args, **kwargs)[0]


def _run_state(
    tmp_path: Path,
    adapter: str,
    adapter_cfg: dict[str, Any],
    effect: dict[str, Any],
    *,
    model: str = "test-model",
) -> tuple[dict[str, Any], dict[str, Any]]:
    doc = tmp_path / "orch.yml"
    doc.write_text(yaml.safe_dump({"effects": [effect]}), encoding="utf-8")
    config = CircuitryConfig(
        default_adapter=adapter,
        default_model=model,
        runtime={"adapters": {adapter: adapter_cfg}},
    )
    result = run_orchestration(orchestration_path=doc, config=config, raise_on_error=False)
    node = result.state["prime"][effect["name"]]
    assert isinstance(node, dict)
    return node, result.state


# ---------- pi ----------


def test_pi_prompt_effect_answers_with_tokens_and_cost(
    tmp_path: Path, record: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    usage = {"input": 7, "output": 3, "cacheRead": 20, "cacheWrite": 0,
             "cost": {"total": 0.002}}  # fmt: skip
    fake = write_fake_cli(tmp_path, "fake-pi", stdout=pi_events("Paris", usage=usage))
    monkeypatch.setenv("PI_SESSION_ID", "parent-session")
    monkeypatch.setenv("CLAUDECODE", "1")
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-test")

    node = _run(
        tmp_path,
        "pi",
        {"binary": str(fake), "thinking": "low"},
        {"type": "prompt", "name": "ask", "template": "Capital of {{input.country}}?\nOne word."},
        model="claude-bridge/claude-sonnet-4-5",
    )

    assert node["meta"]["error"] is None
    assert node["value"] == "Paris"
    assert node["meta"]["adapter"] == "pi"
    assert (node["meta"]["tokens_sent"], node["meta"]["tokens_received"]) == (27, 3)
    assert node["meta"]["cost_usd"] == pytest.approx(0.002)
    assert node["meta"]["finish_reason"] == "stop"

    seen = read_record(record)
    argv = seen["argv"]
    assert argv[:7] == ["-p", "--mode", "json", "--no-approve", "--no-tools", "--no-session", "--model"]
    assert argv[7] == "claude-bridge/claude-sonnet-4-5"
    assert argv[8:10] == ["--thinking", "low"]
    prompt_arg = argv[10]
    assert prompt_arg.startswith("@")
    assert seen["attachments"][prompt_arg[1:]] == "Capital of ?\nOne word."
    assert Path(prompt_arg[1:]).parent.resolve() == Path(seen["cwd"]).resolve()
    assert not Path(seen["cwd"]).exists(), "the temporary working directory is removed"
    for name in ("PI_SESSION_ID", "CLAUDECODE", "ANTHROPIC_API_KEY"):
        assert name not in seen["env"]
    assert seen["env"]["FAKE_CLI_RECORD"] == str(record)


def test_pi_json_prompt_is_decoded_and_validated(tmp_path: Path) -> None:
    fake = write_fake_cli(tmp_path, "fake-pi", stdout=pi_events('```json\n{"count": 3}\n```'))
    node = _run(
        tmp_path,
        "pi",
        {"binary": str(fake)},
        {"type": "prompt", "name": "n", "template": "count", "prompt_type": "json", "schema": SCHEMA},
    )
    assert node["meta"]["error"] is None
    assert node["value"] == {"count": 3}


def test_pi_auth_failure_fails_the_effect_with_the_cli_message(tmp_path: Path) -> None:
    text = "Failed to authenticate. API Error: 401 invalid x-api-key"
    fake = write_fake_cli(
        tmp_path, "fake-pi", stdout=pi_events(text, stop_reason="error"), exit_code=1
    )
    node = _run(tmp_path, "pi", {"binary": str(fake)}, {"type": "prompt", "name": "ask", "template": "hi"})
    assert node["value"] is None
    assert "Failed to authenticate. API Error: 401" in node["meta"]["error"]


def test_pi_unset_env_replaces_the_default_list_and_extra_args_are_passed(
    tmp_path: Path, record: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = write_fake_cli(tmp_path, "fake-pi", stdout=pi_events("ok"))
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-kept")
    monkeypatch.setenv("MY_PROXY_TOKEN", "dropped")
    monkeypatch.setenv("PI_MODEL", "parent-model")
    adapter = build_adapter(
        adapter_name="pi",
        runtime={"adapters": {"pi": {"binary": str(fake), "unset_env": ["MY_PROXY_TOKEN"],
                                     "extra_args": ["--provider-option", "x"]}}},
    )  # fmt: skip
    adapter.generate(model="", prompt="p", timeout_seconds=30)
    seen = read_record(record)
    assert seen["env"]["ANTHROPIC_API_KEY"] == "sk-kept"
    assert "MY_PROXY_TOKEN" not in seen["env"]
    assert "PI_MODEL" not in seen["env"]
    assert "--model" not in seen["argv"]
    PiAdapter(binary=str(fake), default_model="p/m").generate(model="", prompt="p", timeout_seconds=30)
    assert read_record(record)["argv"][6:8] == ["--model", "p/m"]
    assert seen["argv"][-4:-2] == ["--provider-option", "x"]


def test_pi_ignored_generation_options_are_warned_about(tmp_path: Path) -> None:
    fake = write_fake_cli(tmp_path, "fake-pi", stdout=pi_events("ok"))
    result = PiAdapter(binary=str(fake)).generate(
        model="m", prompt="p", timeout_seconds=30, options=GenerateOptions(temperature=0.2)
    )
    assert result.warnings == ("adapter 'pi' ignored: temperature",)


@pytest.mark.skipif(os.name != "posix", reason="process groups are POSIX-only")
def test_pi_timeout_stops_the_cli_and_its_children_and_is_retryable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    pid_file = tmp_path / "child.pid"
    monkeypatch.setenv("FAKE_CLI_CHILD_PID", str(pid_file))
    fake = write_fake_cli(tmp_path, "fake-pi", body=SPAWN_CHILD_AND_HANG)
    with pytest.raises(AdapterCallError, match="did not finish within 1s") as caught:
        PiAdapter(binary=str(fake)).generate(model="", prompt="p", timeout_seconds=1)
    assert caught.value.retry_info.retryable is True
    assert wait_until_dead(int(pid_file.read_text()))


# ---------- claude_code ----------


def test_claude_code_prompt_effect_sends_the_prompt_on_stdin(
    tmp_path: Path, record: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = write_fake_cli(tmp_path, "fake-claude", stdout=claude_result(result="Paris"))
    monkeypatch.setenv("CLAUDE_CODE_ENTRYPOINT", "cli")
    monkeypatch.setenv("ANTHROPIC_AUTH_TOKEN", "tok")

    node, state = _run_state(
        tmp_path,
        "claude_code",
        {"binary": str(fake)},
        {"type": "prompt", "name": "ask", "template": "Capital of France?"},
        model="claude-sonnet-4-5",
    )

    assert node["meta"]["error"] is None
    assert node["value"] == "Paris"
    assert node["meta"]["adapter"] == "claude_code"
    assert (node["meta"]["tokens_sent"], node["meta"]["tokens_received"]) == (1110, 5)
    assert node["meta"]["cost_usd"] == pytest.approx(0.0123)
    assert state["runtime"]["last_run"]["totals"]["cost_usd"] == pytest.approx(0.0123)

    seen = read_record(record)
    assert seen["stdin"] == "Capital of France?"
    assert seen["argv"] == [
        "-p", "--output-format", "json", "--tools", "", "--no-session-persistence",
        "--model", "claude-sonnet-4-5",
    ]  # fmt: skip
    assert not Path(seen["cwd"]).exists()
    assert "CLAUDE_CODE_ENTRYPOINT" not in seen["env"]
    assert "ANTHROPIC_AUTH_TOKEN" not in seen["env"]


def test_claude_code_json_prompt_passes_the_schema_and_uses_structured_output(
    tmp_path: Path, record: Path
) -> None:
    fake = write_fake_cli(
        tmp_path,
        "fake-claude",
        stdout=claude_result(result="Here you go.", structured_output={"count": 4}),
    )
    node = _run(
        tmp_path,
        "claude_code",
        {"binary": str(fake)},
        {"type": "prompt", "name": "n", "template": "count", "prompt_type": "json", "schema": SCHEMA},
    )
    assert node["meta"]["error"] is None
    assert node["value"] == {"count": 4}
    argv = read_record(record)["argv"]
    assert json.loads(argv[argv.index("--json-schema") + 1]) == SCHEMA


def test_claude_code_version_error_is_passed_through(tmp_path: Path) -> None:
    message = "Claude Code 2.1.12 does not support this model; version 2.1.280 or newer is required"
    fake = write_fake_cli(
        tmp_path, "fake-claude", stdout=claude_result(is_error=True, result=message), exit_code=1
    )
    node = _run(
        tmp_path, "claude_code", {"binary": str(fake)}, {"type": "prompt", "name": "ask", "template": "hi"}
    )
    assert node["value"] is None
    assert "version 2.1.280 or newer is required" in node["meta"]["error"]


def test_claude_code_error_is_not_retryable(tmp_path: Path) -> None:
    fake = write_fake_cli(tmp_path, "fake-claude", stdout=claude_result(is_error=True, result="nope"))
    with pytest.raises(AdapterCallError, match="claude failed") as caught:
        ClaudeCodeAdapter(binary=str(fake)).generate(model="", prompt="p", timeout_seconds=30)
    assert caught.value.retry_info.retryable is False


# ---------- config and preflight ----------


def test_builders_default_to_the_cli_names_and_the_default_unset_env() -> None:
    pi = build_adapter(adapter_name="pi", runtime={})
    claude = build_adapter(adapter_name="claude_code", runtime={})
    assert isinstance(pi, PiAdapter) and pi.binary == "pi"
    assert isinstance(claude, ClaudeCodeAdapter) and claude.binary == "claude"
    assert pi.unset_env == claude.unset_env == ("ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN")
    empty = build_adapter(adapter_name="pi", runtime={"adapters": {"pi": {"unset_env": []}}})
    assert isinstance(empty, PiAdapter) and empty.unset_env == ()


@pytest.mark.parametrize("key", ["unset_env", "extra_args"])
def test_builders_reject_a_non_list_setting(key: str) -> None:
    with pytest.raises(ValueError, match=rf"runtime\.adapters\.claude_code\.{key} must be a list"):
        build_adapter(adapter_name="claude_code", runtime={"adapters": {"claude_code": {key: "X"}}})


@pytest.mark.parametrize(("name", "binary"), [("pi", "pi"), ("claude_code", "claude")])
def test_check_reports_a_missing_binary(
    name: str, binary: str, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("PATH", "")
    result = build_adapter(adapter_name=name, runtime={}).check()
    assert result.ok is False
    assert result.missing == [f"binary:{binary}"]


def test_check_finds_a_configured_binary(tmp_path: Path) -> None:
    fake = write_fake_cli(tmp_path, "fake-pi")
    assert PiAdapter(binary=str(fake)).check().ok is True


def test_cof_check_reports_a_missing_cli_binary(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("PATH", "")
    config = tmp_path / "config.json"
    config.write_text(json.dumps({"default_model": "m"}), encoding="utf-8")
    doc = tmp_path / "orch.yml"
    doc.write_text(
        "adapter: claude_code\neffects:\n  - {type: prompt, name: g, template: x}\n",
        encoding="utf-8",
    )
    result = runner.invoke(app, ["check", str(doc), "--config", str(config)])
    assert result.exit_code != 0
    assert "binary:claude" in result.output


def test_cof_doctor_reports_a_missing_cli_binary(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("PATH", "")
    monkeypatch.setenv("CIRCUITRY_ENABLED_ADAPTERS", "pi")
    monkeypatch.setenv("CIRCUITRY_ENABLED_TOOLS", "")
    monkeypatch.setenv("CIRCUITRY_ENABLED_PLUGINS", "")
    config = tmp_path / "config.json"
    config.write_text(json.dumps({"default_adapter": "pi"}), encoding="utf-8")
    result = runner.invoke(app, ["doctor", "--config", str(config)])
    assert "binary:pi" in result.output
