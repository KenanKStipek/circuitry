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


# ---------------------------------------------------------------------------
# Phase 1: Adapter Hardening — Non-JSON response tests
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("adapter_cls", "env_key"),
    [
        (OpenAIAdapter, "OPENAI_API_KEY"),
        (AnthropicAdapter, "ANTHROPIC_API_KEY"),
    ],
)
def test_non_json_html_response_raises_runtime_error(
    monkeypatch: pytest.MonkeyPatch,
    adapter_cls: type,
    env_key: str,
) -> None:
    """curl returns 200 but body is HTML (e.g. Cloudflare challenge page)."""
    monkeypatch.setenv(env_key, "test-key")

    html_body = "<html><body>Service Unavailable</body></html>"

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=0, stdout=html_body)

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = adapter_cls()
    with pytest.raises(RuntimeError, match="non-JSON response"):
        adapter.generate(model="model", prompt="ping")


def test_ollama_non_json_html_response_raises_runtime_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Ollama endpoint returns HTML instead of JSON."""
    html_body = "<html><body>Bad Gateway</body></html>"

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=0, stdout=html_body)

    monkeypatch.setattr("circuitry.adapters.ollama.subprocess.run", fake_run)

    adapter = OllamaAdapter()
    with pytest.raises(RuntimeError, match="non-JSON response"):
        adapter.generate(model="phi3:mini", prompt="ping")


# ---------------------------------------------------------------------------
# Phase 1: Malformed JSON response tests
# ---------------------------------------------------------------------------


def test_openai_empty_choices_returns_empty_text(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """OpenAI returns valid JSON but choices array is empty."""
    monkeypatch.setenv("OPENAI_API_KEY", "test-key")

    payload = {"choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 0}}

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=0, stdout=json.dumps(payload))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = OpenAIAdapter()
    result = adapter.generate(model="gpt-4o-mini", prompt="ping")
    assert result.text == ""


def test_openai_missing_message_key_returns_empty_text(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """OpenAI returns choices but first choice has no 'message' key."""
    monkeypatch.setenv("OPENAI_API_KEY", "test-key")

    payload = {"choices": [{}], "usage": {"prompt_tokens": 5, "completion_tokens": 0}}

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=0, stdout=json.dumps(payload))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = OpenAIAdapter()
    result = adapter.generate(model="gpt-4o-mini", prompt="ping")
    assert result.text == ""


def test_openai_missing_usage_returns_none_tokens(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """OpenAI response has no 'usage' key — tokens should be None, not crash."""
    monkeypatch.setenv("OPENAI_API_KEY", "test-key")

    payload = {"choices": [{"message": {"content": "hi"}}]}

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=0, stdout=json.dumps(payload))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = OpenAIAdapter()
    result = adapter.generate(model="gpt-4o-mini", prompt="ping")
    assert result.text == "hi"
    assert result.tokens_sent is None
    assert result.tokens_received is None


def test_anthropic_empty_content_returns_empty_text(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Anthropic returns valid JSON but content array is empty."""
    monkeypatch.setenv("ANTHROPIC_API_KEY", "test-key")

    payload = {"content": [], "usage": {"input_tokens": 3, "output_tokens": 0}}

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=0, stdout=json.dumps(payload))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = AnthropicAdapter()
    result = adapter.generate(model="claude-sonnet-4-20250514", prompt="ping")
    assert result.text == ""


def test_anthropic_missing_usage_returns_none_tokens(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Anthropic response has no 'usage' key — tokens should be None."""
    monkeypatch.setenv("ANTHROPIC_API_KEY", "test-key")

    payload = {"content": [{"type": "text", "text": "hi"}]}

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=0, stdout=json.dumps(payload))

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = AnthropicAdapter()
    result = adapter.generate(model="claude-sonnet-4-20250514", prompt="ping")
    assert result.text == "hi"
    assert result.tokens_sent is None
    assert result.tokens_received is None


def test_ollama_missing_response_key_returns_empty_text(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Ollama returns valid JSON but no 'response' key."""
    payload = {"model": "phi3:mini"}

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=0, stdout=json.dumps(payload))

    monkeypatch.setattr("circuitry.adapters.ollama.subprocess.run", fake_run)

    adapter = OllamaAdapter()
    result = adapter.generate(model="phi3:mini", prompt="ping")
    assert result.text == ""
    assert result.tokens_sent is None
    assert result.tokens_received is None


# ---------------------------------------------------------------------------
# Phase 1: FileNotFoundError (curl not installed) tests
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("adapter_cls", "env_key"),
    [
        (OpenAIAdapter, "OPENAI_API_KEY"),
        (AnthropicAdapter, "ANTHROPIC_API_KEY"),
    ],
)
def test_curl_not_installed_raises_actionable_error(
    monkeypatch: pytest.MonkeyPatch,
    adapter_cls: type,
    env_key: str,
) -> None:
    """FileNotFoundError from subprocess.run -> actionable RuntimeError."""
    monkeypatch.setenv(env_key, "test-key")

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        raise FileNotFoundError("No such file or directory: 'curl'")

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = adapter_cls()
    with pytest.raises(RuntimeError, match="curl is not installed"):
        adapter.generate(model="model", prompt="ping")


def test_ollama_curl_not_installed_raises_actionable_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Ollama adapter: FileNotFoundError -> actionable RuntimeError."""

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        raise FileNotFoundError("No such file or directory: 'curl'")

    monkeypatch.setattr("circuitry.adapters.ollama.subprocess.run", fake_run)

    adapter = OllamaAdapter()
    with pytest.raises(RuntimeError, match="curl is not installed"):
        adapter.generate(model="phi3:mini", prompt="ping")


# ---------------------------------------------------------------------------
# Phase 1: Missing API key tests
# ---------------------------------------------------------------------------


def test_openai_missing_api_key_raises(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)

    adapter = OpenAIAdapter()
    with pytest.raises(RuntimeError, match="OPENAI_API_KEY"):
        adapter.generate(model="gpt-4o-mini", prompt="ping")


def test_anthropic_missing_api_key_raises(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)

    adapter = AnthropicAdapter()
    with pytest.raises(RuntimeError, match="ANTHROPIC_API_KEY"):
        adapter.generate(model="claude-sonnet-4-20250514", prompt="ping")


# ---------------------------------------------------------------------------
# Phase 1: API key masking tests
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("adapter_cls", "env_key"),
    [
        (OpenAIAdapter, "OPENAI_API_KEY"),
        (AnthropicAdapter, "ANTHROPIC_API_KEY"),
    ],
)
def test_api_key_not_leaked_in_error_message(
    monkeypatch: pytest.MonkeyPatch,
    adapter_cls: type,
    env_key: str,
) -> None:
    """When curl fails, the API key must not appear in the exception message."""
    secret = "sk-super-secret-key-12345"
    monkeypatch.setenv(env_key, secret)

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=22, stderr="HTTP 401 Unauthorized")

    monkeypatch.setattr("subprocess.run", fake_run)

    adapter = adapter_cls()
    with pytest.raises(RuntimeError) as exc:
        adapter.generate(model="model", prompt="ping")

    assert secret not in str(exc.value)


# ---------------------------------------------------------------------------
# Phase 1: LiteLLM import error test
# ---------------------------------------------------------------------------


def test_litellm_import_error_gives_actionable_message(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """When litellm is not installed, the error message tells you how to fix it."""
    monkeypatch.delitem(sys.modules, "litellm", raising=False)

    # Temporarily make the import fail
    import builtins

    original_import = builtins.__import__

    def failing_import(name: str, *args: Any, **kwargs: Any) -> Any:
        if name == "litellm":
            raise ImportError("No module named 'litellm'")
        return original_import(name, *args, **kwargs)

    monkeypatch.setattr(builtins, "__import__", failing_import)

    adapter = LiteLLMAdapter(default_model="openai/gpt-4o-mini")
    with pytest.raises(RuntimeError, match="pip install litellm"):
        adapter.generate(model="", prompt="ping")
