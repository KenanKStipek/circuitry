from __future__ import annotations

from typing import Any, Callable

from .anthropic import AnthropicAdapter
from .base import Adapter
from .deepseek import DeepSeekAdapter
from .fireworks import FireworksAdapter
from .gemini import GeminiAdapter
from .groq import GroqAdapter
from .host_claude import HostClaudeAdapter
from .litellm import LiteLLMAdapter
from .llamacpp import LlamaCppAdapter
from .lmstudio import LMStudioAdapter
from .nvidia_nim import NvidiaNimAdapter
from .ollama import OllamaAdapter
from .openai import OpenAIAdapter
from .openrouter import OpenRouterAdapter
from .perplexity import PerplexityAdapter
from .together import TogetherAdapter
from .vllm import VllmAdapter
from .xai import XaiAdapter

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


# OpenAI-compatible adapters share the same shape: per-provider defaults
# baked into the dataclass, runtime config can override base_url and
# default_model. Network transport is in adapters/_openai_compat.py.
def _build_groq(cfg: dict[str, Any]) -> Adapter:
    return GroqAdapter(
        base_url=cfg.get("base_url") or "https://api.groq.com/openai/v1",
        default_model=cfg.get("default_model") or "llama-3.3-70b-versatile",
    )


def _build_openrouter(cfg: dict[str, Any]) -> Adapter:
    return OpenRouterAdapter(
        base_url=cfg.get("base_url") or "https://openrouter.ai/api/v1",
        default_model=cfg.get("default_model") or "openai/gpt-4o-mini",
    )


def _build_perplexity(cfg: dict[str, Any]) -> Adapter:
    return PerplexityAdapter(
        base_url=cfg.get("base_url") or "https://api.perplexity.ai",
        default_model=cfg.get("default_model") or "sonar",
    )


def _build_xai(cfg: dict[str, Any]) -> Adapter:
    return XaiAdapter(
        base_url=cfg.get("base_url") or "https://api.x.ai/v1",
        default_model=cfg.get("default_model") or "grok-2-latest",
    )


def _build_deepseek(cfg: dict[str, Any]) -> Adapter:
    return DeepSeekAdapter(
        base_url=cfg.get("base_url") or "https://api.deepseek.com/v1",
        default_model=cfg.get("default_model") or "deepseek-chat",
    )


def _build_together(cfg: dict[str, Any]) -> Adapter:
    return TogetherAdapter(
        base_url=cfg.get("base_url") or "https://api.together.xyz/v1",
        default_model=cfg.get("default_model")
        or "meta-llama/Llama-3.3-70B-Instruct-Turbo",
    )


def _build_fireworks(cfg: dict[str, Any]) -> Adapter:
    return FireworksAdapter(
        base_url=cfg.get("base_url") or "https://api.fireworks.ai/inference/v1",
        default_model=cfg.get("default_model")
        or "accounts/fireworks/models/llama-v3p3-70b-instruct",
    )


def _build_nvidia_nim(cfg: dict[str, Any]) -> Adapter:
    # api_key_env: empty string is a valid explicit override (self-hosted
    # NIM containers don't need a key); only fall back to the default
    # when the field is absent entirely. A plain ``or`` would coerce the
    # intentional empty string back to NIM_API_KEY.
    api_key_env = cfg.get("api_key_env", "NIM_API_KEY")
    return NvidiaNimAdapter(
        base_url=cfg.get("base_url") or "https://integrate.api.nvidia.com/v1",
        default_model=cfg.get("default_model") or "meta/llama-3.3-70b-instruct",
        api_key_env=api_key_env if api_key_env is not None else "NIM_API_KEY",
    )


def _build_vllm(cfg: dict[str, Any]) -> Adapter:
    return VllmAdapter(
        base_url=cfg.get("base_url") or "http://localhost:8000/v1",
        default_model=cfg.get("default_model") or "",
    )


def _build_llamacpp(cfg: dict[str, Any]) -> Adapter:
    return LlamaCppAdapter(
        base_url=cfg.get("base_url") or "http://localhost:8080/v1",
        default_model=cfg.get("default_model") or "",
    )


def _build_lmstudio(cfg: dict[str, Any]) -> Adapter:
    return LMStudioAdapter(
        base_url=cfg.get("base_url") or "http://localhost:1234/v1",
        default_model=cfg.get("default_model") or "",
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
    # OpenAI-compatible providers (shared transport via _openai_compat).
    "groq": _build_groq,
    "openrouter": _build_openrouter,
    "perplexity": _build_perplexity,
    "xai": _build_xai,
    "deepseek": _build_deepseek,
    "together": _build_together,
    "fireworks": _build_fireworks,
    "nvidia-nim": _build_nvidia_nim,
    "vllm": _build_vllm,
    "llamacpp": _build_llamacpp,
    "lmstudio": _build_lmstudio,
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
