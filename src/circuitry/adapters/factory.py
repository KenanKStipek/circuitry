from __future__ import annotations

from typing import Any, Callable

from .anthropic import AnthropicAdapter
from .base import Adapter
from .gemini import GeminiAdapter
from .host_claude import HostClaudeAdapter
from .litellm import LiteLLMAdapter
from .ollama import OllamaAdapter
from .openai import OpenAIAdapter

AdapterBuilder = Callable[[dict[str, Any]], Adapter]


def _build_ollama(cfg: dict[str, Any]) -> Adapter:
    return OllamaAdapter(base_url=cfg.get("base_url") or "http://localhost:11434")


def _build_openai(cfg: dict[str, Any]) -> Adapter:
    return OpenAIAdapter(
        base_url=cfg.get("base_url") or "https://api.openai.com/v1",
        default_model=cfg.get("default_model") or "gpt-4o-mini",
    )


def _build_anthropic(cfg: dict[str, Any]) -> Adapter:
    return AnthropicAdapter(
        base_url=cfg.get("base_url") or "https://api.anthropic.com",
        default_model=cfg.get("default_model") or "claude-sonnet-4-20250514",
        max_tokens=int(cfg.get("max_tokens") or 4096),
    )


def _build_litellm(cfg: dict[str, Any]) -> Adapter:
    return LiteLLMAdapter(
        default_model=cfg.get("default_model") or "openai/gpt-4o-mini",
        api_base=cfg.get("api_base") or "",
        timeout=int(cfg.get("timeout") or 120),
    )


def _build_gemini(cfg: dict[str, Any]) -> Adapter:
    return GeminiAdapter(
        base_url=cfg.get("base_url")
        or "https://generativelanguage.googleapis.com/v1beta/openai",
        default_model=cfg.get("default_model") or "gemini-2.5-flash",
    )


def _build_host_claude(cfg: dict[str, Any]) -> Adapter:
    raise RuntimeError(
        "host_claude cannot be built from config; it requires a "
        "request_handler injected at runtime via RunRequest.adapter "
        "(see circuitry-mcp). Run `circuitry-mcp` (or `cof mcp`) and "
        "drive the orchestration via the MCP tool loop, or supply "
        "RunRequest(adapter=HostClaudeAdapter(request_handler=...)) "
        "from a programmatic caller."
    )


ADAPTER_REGISTRY: dict[str, AdapterBuilder] = {
    "ollama": _build_ollama,
    "openai": _build_openai,
    "anthropic": _build_anthropic,
    "litellm": _build_litellm,
    "host_claude": _build_host_claude,
    "gemini": _build_gemini,
}


def _supported_names() -> tuple[str, ...]:
    return tuple(sorted(ADAPTER_REGISTRY.keys()))


# Back-compat alias. Preserves original insertion-order tuple shape so callers
# that imported the constant still work; new code should use ADAPTER_REGISTRY.
SUPPORTED_ADAPTERS = ("ollama", "openai", "anthropic", "litellm", "host_claude")


def build_adapter(*, adapter_name: str, runtime: dict[str, Any]) -> Adapter:
    """
    Build an adapter instance from configuration.

    Adapters register themselves in ADAPTER_REGISTRY: a dict from canonical
    lower-case name to a builder callable that takes the per-adapter config
    dict (read from runtime.adapters.<adapter_name>) and returns an Adapter.

    host_claude is a registered name but its builder raises RuntimeError —
    it can only be supplied via RunRequest.adapter at runtime.
    """
    adapter_name = (adapter_name or "").strip().lower()
    adapters_cfg = (runtime or {}).get("adapters") or {}

    builder = ADAPTER_REGISTRY.get(adapter_name)
    if builder is None:
        supported = ", ".join(_supported_names())
        raise ValueError(
            f"Unknown adapter: {adapter_name!r}. Supported adapters: {supported}. "
            "Check runtime.adapters.<adapter_name> and default_adapter/adapter resolution."
        )

    cfg = adapters_cfg.get(adapter_name) or {}
    return builder(cfg)
