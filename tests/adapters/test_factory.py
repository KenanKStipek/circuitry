from __future__ import annotations

import pytest

from circuitry.adapters.anthropic import AnthropicAdapter
from circuitry.adapters.factory import build_adapter
from circuitry.adapters.litellm import LiteLLMAdapter
from circuitry.adapters.ollama import OllamaAdapter
from circuitry.adapters.openai import OpenAIAdapter

# ---------------------------------------------------------------------------
# Unknown adapter name
# ---------------------------------------------------------------------------


def test_unknown_adapter_raises_value_error() -> None:
    with pytest.raises(ValueError, match="Unknown adapter.*'nope'"):
        build_adapter(adapter_name="nope", runtime={})


def test_empty_adapter_name_raises_value_error() -> None:
    with pytest.raises(ValueError, match="Unknown adapter"):
        build_adapter(adapter_name="", runtime={})


# ---------------------------------------------------------------------------
# Each supported name returns correct type
# ---------------------------------------------------------------------------


def test_build_ollama_returns_ollama_adapter() -> None:
    adapter = build_adapter(adapter_name="ollama", runtime={})
    assert isinstance(adapter, OllamaAdapter)
    assert adapter.name == "ollama"


def test_build_openai_returns_openai_adapter() -> None:
    adapter = build_adapter(adapter_name="openai", runtime={})
    assert isinstance(adapter, OpenAIAdapter)
    assert adapter.name == "openai"


def test_build_anthropic_returns_anthropic_adapter() -> None:
    adapter = build_adapter(adapter_name="anthropic", runtime={})
    assert isinstance(adapter, AnthropicAdapter)
    assert adapter.name == "anthropic"


def test_build_litellm_returns_litellm_adapter() -> None:
    adapter = build_adapter(adapter_name="litellm", runtime={})
    assert isinstance(adapter, LiteLLMAdapter)
    assert adapter.name == "litellm"


# ---------------------------------------------------------------------------
# Config passthrough
# ---------------------------------------------------------------------------


def test_ollama_config_passthrough() -> None:
    runtime = {"adapters": {"ollama": {"base_url": "http://gpu-box:11434"}}}
    adapter = build_adapter(adapter_name="ollama", runtime=runtime)
    assert isinstance(adapter, OllamaAdapter)
    assert adapter.base_url == "http://gpu-box:11434"


def test_openai_config_passthrough() -> None:
    runtime = {
        "adapters": {
            "openai": {
                "base_url": "https://custom.api/v1",
                "default_model": "gpt-3.5-turbo",
            }
        }
    }
    adapter = build_adapter(adapter_name="openai", runtime=runtime)
    assert isinstance(adapter, OpenAIAdapter)
    assert adapter.base_url == "https://custom.api/v1"
    assert adapter.default_model == "gpt-3.5-turbo"


def test_anthropic_config_passthrough() -> None:
    runtime = {
        "adapters": {
            "anthropic": {
                "base_url": "https://custom.anthropic/",
                "default_model": "claude-3-haiku",
                "max_tokens": 2048,
            }
        }
    }
    adapter = build_adapter(adapter_name="anthropic", runtime=runtime)
    assert isinstance(adapter, AnthropicAdapter)
    assert adapter.base_url == "https://custom.anthropic/"
    assert adapter.default_model == "claude-3-haiku"
    assert adapter.max_tokens == 2048


def test_litellm_config_passthrough() -> None:
    runtime = {
        "adapters": {
            "litellm": {
                "default_model": "anthropic/claude-3-haiku",
                "api_base": "https://proxy.example.com",
                "timeout": 60,
            }
        }
    }
    adapter = build_adapter(adapter_name="litellm", runtime=runtime)
    assert isinstance(adapter, LiteLLMAdapter)
    assert adapter.default_model == "anthropic/claude-3-haiku"
    assert adapter.api_base == "https://proxy.example.com"
    assert adapter.timeout == 60


# ---------------------------------------------------------------------------
# Name normalization (whitespace, case)
# ---------------------------------------------------------------------------


def test_adapter_name_is_case_insensitive() -> None:
    adapter = build_adapter(adapter_name="Ollama", runtime={})
    assert isinstance(adapter, OllamaAdapter)


def test_adapter_name_strips_whitespace() -> None:
    adapter = build_adapter(adapter_name="  openai  ", runtime={})
    assert isinstance(adapter, OpenAIAdapter)
