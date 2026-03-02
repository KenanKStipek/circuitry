from __future__ import annotations

from typing import Any

from .anthropic import AnthropicAdapter
from .base import Adapter
from .litellm import LiteLLMAdapter
from .ollama import OllamaAdapter
from .openai import OpenAIAdapter

SUPPORTED_ADAPTERS = ("ollama", "openai", "anthropic", "litellm")


def build_adapter(*, adapter_name: str, runtime: dict[str, Any]) -> Adapter:
    """
    Build an adapter instance from configuration.

    Supported adapters:
      - ollama: Local Ollama instance
      - openai: OpenAI API (requires OPENAI_API_KEY)
      - anthropic: Anthropic API (requires ANTHROPIC_API_KEY)
      - litellm: LiteLLM unified interface (requires litellm package)

    Config is read from runtime.adapters.<adapter_name>
    """
    adapter_name = (adapter_name or "").strip().lower()
    adapters_cfg = (runtime or {}).get("adapters") or {}

    if adapter_name == "ollama":
        cfg = adapters_cfg.get("ollama") or {}
        base_url = cfg.get("base_url") or "http://localhost:11434"
        return OllamaAdapter(base_url=base_url)

    if adapter_name == "openai":
        cfg = adapters_cfg.get("openai") or {}
        return OpenAIAdapter(
            base_url=cfg.get("base_url") or "https://api.openai.com/v1",
            default_model=cfg.get("default_model") or "gpt-4o-mini",
        )

    if adapter_name == "anthropic":
        cfg = adapters_cfg.get("anthropic") or {}
        return AnthropicAdapter(
            base_url=cfg.get("base_url") or "https://api.anthropic.com",
            default_model=cfg.get("default_model") or "claude-sonnet-4-20250514",
            max_tokens=int(cfg.get("max_tokens") or 4096),
        )

    if adapter_name == "litellm":
        cfg = adapters_cfg.get("litellm") or {}
        return LiteLLMAdapter(
            default_model=cfg.get("default_model") or "openai/gpt-4o-mini",
            api_base=cfg.get("api_base") or "",
            timeout=int(cfg.get("timeout") or 120),
        )

    supported = ", ".join(SUPPORTED_ADAPTERS)
    raise ValueError(
        f"Unknown adapter: {adapter_name!r}. Supported adapters: {supported}. "
        "Check runtime.adapters.<adapter_name> and default_adapter/adapter resolution."
    )
