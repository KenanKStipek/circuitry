from __future__ import annotations

import pytest

from circuitry.adapters import (
    HostClaudeAdapter,
    HostPromptRequest,
    RunCancelled,
    build_adapter,
    validate_generate_result,
)


def test_generate_invokes_handler_and_returns_text() -> None:
    captured: list[HostPromptRequest] = []

    def handler(req: HostPromptRequest) -> str:
        captured.append(req)
        return "hello"

    adapter = HostClaudeAdapter(request_handler=handler)
    result = adapter.generate(model="claude-sonnet-4", prompt="ping")

    assert result.text == "hello"
    assert result.raw == {"adapter": "host_claude", "model": "claude-sonnet-4"}
    assert validate_generate_result(result, adapter_name="host_claude") == []
    assert len(captured) == 1
    assert captured[0].prompt == "ping"
    assert captured[0].model == "claude-sonnet-4"
    assert captured[0].timeout_seconds == 120


def test_rejects_non_claude_model_by_default() -> None:
    called = []

    def handler(req: HostPromptRequest) -> str:
        called.append(req)
        return "should not run"

    adapter = HostClaudeAdapter(request_handler=handler)
    with pytest.raises(ValueError, match="host_claude only accepts Claude-family models"):
        adapter.generate(model="gpt-4o", prompt="x")

    assert called == []  # handler never invoked


@pytest.mark.parametrize(
    "model",
    ["claude", "claude-sonnet-4", "claude-opus-4-7", "claude-haiku-4-5-20251001", ""],
)
def test_accepts_claude_model_variants(model: str) -> None:
    adapter = HostClaudeAdapter(request_handler=lambda req: "ok")
    result = adapter.generate(model=model, prompt="x")
    assert result.text == "ok"
    assert result.raw["model"] == model
    assert "overridden_from" not in result.raw


def test_handler_can_raise_run_cancelled() -> None:
    def handler(req: HostPromptRequest) -> str:
        raise RunCancelled("user aborted")

    adapter = HostClaudeAdapter(request_handler=handler)
    with pytest.raises(RunCancelled, match="user aborted"):
        adapter.generate(model="claude-sonnet-4", prompt="x")


def test_factory_refuses_to_build_host_claude() -> None:
    with pytest.raises(RuntimeError, match="cannot be built from config"):
        build_adapter(adapter_name="host_claude", runtime={})


def test_override_model_substitutes_non_claude_pin() -> None:
    captured: list[HostPromptRequest] = []

    def handler(req: HostPromptRequest) -> str:
        captured.append(req)
        return "answer"

    adapter = HostClaudeAdapter(
        request_handler=handler,
        override_model=True,
        override_to="claude-opus-4-7",
    )
    result = adapter.generate(model="gpt-oss:20b", prompt="x")

    assert result.text == "answer"
    assert result.raw["model"] == "claude-opus-4-7"
    assert result.raw["overridden_from"] == "gpt-oss:20b"
    assert captured[0].model == "claude-opus-4-7"


def test_override_model_without_override_to_uses_empty_model() -> None:
    """override_model=True with no override_to lets the host pick its own model."""
    adapter = HostClaudeAdapter(
        request_handler=lambda req: "ok",
        override_model=True,
    )
    result = adapter.generate(model="ollama:llama3.1", prompt="x")

    assert result.raw["model"] == ""
    assert result.raw["overridden_from"] == "ollama:llama3.1"


def test_override_model_does_not_alter_claude_pins() -> None:
    """Claude pins are honored as-is even under override mode (no overridden_from)."""
    adapter = HostClaudeAdapter(
        request_handler=lambda req: "ok",
        override_model=True,
        override_to="claude-opus-4-7",
    )
    result = adapter.generate(model="claude-sonnet-4", prompt="x")

    assert result.raw["model"] == "claude-sonnet-4"
    assert "overridden_from" not in result.raw


def test_default_model_used_when_pin_empty() -> None:
    adapter = HostClaudeAdapter(
        request_handler=lambda req: "ok",
        default_model="claude-sonnet-4",
    )
    result = adapter.generate(model="", prompt="x")
    assert result.raw["model"] == "claude-sonnet-4"


def test_handler_returning_non_string_is_coerced() -> None:
    """Defensive: if a handler returns something non-str, we don't blow up."""
    adapter = HostClaudeAdapter(request_handler=lambda req: 42)  # type: ignore[arg-type, return-value]
    result = adapter.generate(model="claude-sonnet-4", prompt="x")
    assert result.text == "42"
    assert isinstance(result.text, str)
