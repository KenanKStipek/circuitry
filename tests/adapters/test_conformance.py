from __future__ import annotations

import json
import sys
from dataclasses import dataclass
from types import SimpleNamespace
from typing import Any

import pytest

from circuitry.adapters import (
    AnthropicAdapter,
    LiteLLMAdapter,
    OllamaAdapter,
    OpenAIAdapter,
)
from circuitry.adapters.base import GenerateResult
from circuitry.adapters.conformance import validate_generate_result


@dataclass(frozen=True)
class FakeProc:
    returncode: int
    stdout: str = ""
    stderr: str = ""


def test_openai_adapter_conformance_with_mocked_transport(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("OPENAI_API_KEY", "test-key")

    payload = {
        "choices": [{"message": {"content": "hello from openai"}}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7},
    }

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=0, stdout=json.dumps(payload))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = OpenAIAdapter()
    result = adapter.generate(model="gpt-4o-mini", prompt="ping")

    assert result.text == "hello from openai"
    assert validate_generate_result(result, adapter_name="openai") == []


def test_anthropic_adapter_conformance_with_mocked_transport(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("ANTHROPIC_API_KEY", "test-key")

    payload = {
        "content": [{"type": "text", "text": "hello from anthropic"}],
        "usage": {"input_tokens": 3, "output_tokens": 5},
    }

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=0, stdout=json.dumps(payload))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = AnthropicAdapter()
    result = adapter.generate(model="claude-sonnet-4-20250514", prompt="ping")

    assert result.text == "hello from anthropic"
    assert validate_generate_result(result, adapter_name="anthropic") == []


def test_ollama_adapter_conformance_with_mocked_transport(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    payload = {
        "response": "hello from ollama",
        "prompt_eval_count": 9,
        "eval_count": 4,
    }

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=0, stdout=json.dumps(payload))

    monkeypatch.setattr("circuitry.adapters.ollama.subprocess.run", fake_run)

    adapter = OllamaAdapter()
    result = adapter.generate(model="phi3:mini", prompt="ping")

    assert result.text == "hello from ollama"
    assert validate_generate_result(result, adapter_name="ollama") == []


def test_litellm_adapter_conformance_with_mocked_provider_module(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class FakeLiteResponse:
        def __init__(self) -> None:
            self.choices = [
                SimpleNamespace(message=SimpleNamespace(content="hello from litellm"))
            ]
            self.usage = SimpleNamespace(prompt_tokens=8, completion_tokens=6)

        def model_dump(self) -> dict[str, Any]:
            return {"provider": "litellm"}

    def fake_completion(**kwargs: Any) -> FakeLiteResponse:
        del kwargs
        return FakeLiteResponse()

    fake_module = SimpleNamespace(completion=fake_completion)
    monkeypatch.setitem(sys.modules, "litellm", fake_module)

    adapter = LiteLLMAdapter(default_model="openai/gpt-4o-mini")
    result = adapter.generate(model="", prompt="ping")

    assert result.text == "hello from litellm"
    assert validate_generate_result(result, adapter_name="litellm") == []


def test_conformance_validator_surfaces_actionable_contract_mismatch() -> None:
    bad = GenerateResult(
        text="ok",
        raw="not-a-dict",  # type: ignore[arg-type]
        tokens_sent=-1,
        tokens_received="bad",  # type: ignore[arg-type]
    )
    diagnostics = validate_generate_result(bad, adapter_name="fake")

    assert len(diagnostics) == 3
    assert "fake: 'raw' must be dict" in diagnostics[0]
    assert "tokens_sent" in " ".join(diagnostics)
    assert "tokens_received" in " ".join(diagnostics)


@pytest.mark.parametrize(
    ("adapter", "error_substring"),
    [
        (OpenAIAdapter(), "curl failed"),
        (AnthropicAdapter(), "curl failed"),
    ],
)
def test_direct_provider_errors_are_actionable(
    monkeypatch: pytest.MonkeyPatch,
    adapter: Any,
    error_substring: str,
) -> None:
    if isinstance(adapter, OpenAIAdapter):
        monkeypatch.setenv("OPENAI_API_KEY", "test-key")
    if isinstance(adapter, AnthropicAdapter):
        monkeypatch.setenv("ANTHROPIC_API_KEY", "test-key")

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=28, stderr="operation timed out")

    monkeypatch.setattr("subprocess.run", fake_run)

    with pytest.raises(RuntimeError) as exc:
        adapter.generate(model="model", prompt="ping")

    message = str(exc.value)
    assert error_substring in message
    assert "cmd=" in message
    assert "error=" in message


def test_litellm_errors_are_actionable(monkeypatch: pytest.MonkeyPatch) -> None:
    def fake_completion(**kwargs: Any) -> Any:
        del kwargs
        raise RuntimeError("provider mismatch")

    fake_module = SimpleNamespace(completion=fake_completion)
    monkeypatch.setitem(sys.modules, "litellm", fake_module)

    adapter = LiteLLMAdapter(default_model="openai/gpt-4o-mini")
    with pytest.raises(RuntimeError) as exc:
        adapter.generate(model="", prompt="ping")

    assert "LiteLLM request failed" in str(exc.value)
    assert "provider mismatch" in str(exc.value)
