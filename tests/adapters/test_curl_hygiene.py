"""Curl-hygiene regression tests for issue #264 part 3: `-q` first, the
request body on stdin, and secret headers never on argv — for the
LLM adapters that talk to curl directly (openai, anthropic, ollama).
`_openai_compat` (the ~20-provider family) and the special-shaped
adapters (replicate, watsonx, azure, comfyui) have their own coverage in
`test_openai_compat_catalog.py`, `test_gemini.py` and
`test_special_shape_adapters.py`.
"""

from __future__ import annotations

import json
import shutil
import subprocess
from dataclasses import dataclass
from typing import Any

import pytest
from curl_test_support import (
    RecordingJSONHandler,
    assert_not_in_argv,
    assert_q_first,
    local_server,
    read_config_headers,
)

from circuitry.adapters.anthropic import AnthropicAdapter
from circuitry.adapters.ollama import OllamaAdapter
from circuitry.adapters.openai import OpenAIAdapter


@dataclass(frozen=True)
class FakeProc:
    returncode: int
    stdout: str = ""
    stderr: str = ""


def test_openai_canary_key_and_prompt_never_touch_argv(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    secret = "sk-canary-openai-key-0001"
    canary_prompt = "canary prompt: do not leak me"
    monkeypatch.setenv("OPENAI_API_KEY", secret)

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        captured["headers"] = read_config_headers(cmd)
        captured["input"] = kwargs.get("input")
        return FakeProc(
            returncode=0,
            stdout=json.dumps({"choices": [{"message": {"content": "hi"}}]}),
        )

    monkeypatch.setattr("subprocess.run", fake_run)
    OpenAIAdapter().generate(model="gpt-4o-mini", prompt=canary_prompt)

    assert_q_first(captured["cmd"])
    assert_not_in_argv(captured["cmd"], secret, canary_prompt)
    assert captured["headers"]["Authorization"] == f"Bearer {secret}"
    assert canary_prompt in (captured["input"] or "")


def test_openai_curl_failure_message_has_no_argv_and_no_secret(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    secret = "sk-canary-openai-key-0002"
    monkeypatch.setenv("OPENAI_API_KEY", secret)

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=22, stderr="HTTP 401")

    monkeypatch.setattr("subprocess.run", fake_run)
    with pytest.raises(RuntimeError) as exc:
        OpenAIAdapter().generate(model="gpt-4o-mini", prompt="x")
    message = str(exc.value)
    assert secret not in message
    assert "cmd=" not in message


def test_anthropic_canary_key_and_prompt_never_touch_argv(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    secret = "sk-ant-canary-0001"
    canary_prompt = "canary prompt: anthropic edition"
    monkeypatch.setenv("ANTHROPIC_API_KEY", secret)

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        captured["headers"] = read_config_headers(cmd)
        captured["input"] = kwargs.get("input")
        return FakeProc(
            returncode=0,
            stdout=json.dumps({"content": [{"type": "text", "text": "hi"}]}),
        )

    monkeypatch.setattr("subprocess.run", fake_run)
    AnthropicAdapter().generate(model="claude-sonnet-5", prompt=canary_prompt)

    assert_q_first(captured["cmd"])
    assert_not_in_argv(captured["cmd"], secret, canary_prompt)
    assert captured["headers"]["x-api-key"] == secret
    assert captured["headers"]["anthropic-version"] == "2023-06-01"
    assert canary_prompt in (captured["input"] or "")


def test_anthropic_curl_failure_message_has_no_argv_and_no_secret(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    secret = "sk-ant-canary-0002"
    monkeypatch.setenv("ANTHROPIC_API_KEY", secret)

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=22, stderr="HTTP 401")

    monkeypatch.setattr("subprocess.run", fake_run)
    with pytest.raises(RuntimeError) as exc:
        AnthropicAdapter().generate(model="claude-sonnet-5", prompt="x")
    message = str(exc.value)
    assert secret not in message
    assert "cmd=" not in message


def test_ollama_canary_prompt_never_touches_argv(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    canary_prompt = "canary prompt: ollama edition"

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        captured["headers"] = read_config_headers(cmd)
        captured["input"] = kwargs.get("input")
        return FakeProc(
            returncode=0, stdout=json.dumps({"response": "hi", "done_reason": "stop"})
        )

    monkeypatch.setattr("subprocess.run", fake_run)
    OllamaAdapter().generate(model="phi3", prompt=canary_prompt)

    assert_q_first(captured["cmd"])
    assert_not_in_argv(captured["cmd"], canary_prompt)
    assert captured["headers"]["Content-Type"] == "application/json"
    assert canary_prompt in (captured["input"] or "")


def test_ollama_curl_failure_message_has_no_argv(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=7, stderr="curl: (7) Failed to connect")

    monkeypatch.setattr("subprocess.run", fake_run)
    with pytest.raises(RuntimeError) as exc:
        OllamaAdapter().generate(model="phi3", prompt="x")
    assert "cmd=" not in str(exc.value)


# ---------------------------------------------------------------------------
# End to end against a local HTTP server, never a live provider.
# ---------------------------------------------------------------------------


class _OpenAIShapedHandler(RecordingJSONHandler):
    response_body = json.dumps(
        {
            "choices": [{"message": {"content": "hi from local server"}}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 2},
        }
    ).encode()


@pytest.mark.skipif(shutil.which("curl") is None, reason="curl not on PATH")
def test_openai_end_to_end_against_local_server(monkeypatch: pytest.MonkeyPatch) -> None:
    """Real curl, real local server: the whole run_curl plumbing (headers
    off argv, body on stdin, -q first) works end to end for a real call,
    and a canary key/prompt never reach argv even under the real subprocess
    (not just a mock)."""
    secret = "canary-openai-e2e-key"
    canary_prompt = "canary prompt " * 20000  # > 200 KiB, proves no argv ceiling
    monkeypatch.setenv("OPENAI_API_KEY", secret)

    calls: list[list[str]] = []
    real_run = subprocess.run

    def spying_run(cmd: list[str], **kwargs: Any) -> Any:
        calls.append(cmd)
        return real_run(cmd, **kwargs)

    monkeypatch.setattr("subprocess.run", spying_run)

    with local_server(_OpenAIShapedHandler) as base_url:
        adapter = OpenAIAdapter(base_url=base_url)
        result = adapter.generate(model="gpt-4o-mini", prompt=canary_prompt, timeout_seconds=10)

    assert result.text == "hi from local server"
    assert (
        _OpenAIShapedHandler.captured_headers["Authorization"] == f"Bearer {secret}"
    )
    assert canary_prompt.encode() in _OpenAIShapedHandler.captured_body
    for cmd in calls:
        assert_q_first(cmd)
        assert_not_in_argv(cmd, secret, canary_prompt[:200])
