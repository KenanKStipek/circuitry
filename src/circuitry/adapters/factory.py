from __future__ import annotations

import logging
from collections.abc import Callable
from typing import Any

from ..agent_cli import DEFAULT_UNSET_ENV
from .ai21 import AI21Adapter
from .anthropic import AnthropicAdapter
from .azure_openai import AzureOpenAIAdapter
from .base import Adapter
from .claude_code import ClaudeCodeAdapter
from .cloudflare_workers_ai import CloudflareWorkersAIAdapter
from .cohere import CohereAdapter
from .cyberdiner import CyberdinerAdapter
from .databricks import DatabricksAdapter
from .deepseek import DeepSeekAdapter
from .fireworks import FireworksAdapter
from .gemini import GeminiAdapter
from .groq import GroqAdapter
from .huggingface_inference import HuggingFaceInferenceAdapter
from .litellm import LiteLLMAdapter
from .llamacpp import LlamaCppAdapter
from .lmstudio import LMStudioAdapter
from .mistral import MistralAdapter
from .nvidia_nim import NvidiaNimAdapter
from .ollama import OllamaAdapter
from .openai import OpenAIAdapter
from .openrouter import OpenRouterAdapter
from .perplexity import PerplexityAdapter
from .pi import PiAdapter
from .qwen_dashscope import QwenDashScopeAdapter
from .replicate import ReplicateAdapter
from .scripted import ScriptedAdapter
from .tgi import TgiAdapter
from .together import TogetherAdapter
from .vllm import VllmAdapter
from .watsonx import WatsonXAdapter
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
        default_model=cfg.get("default_model") or AnthropicAdapter.default_model,
        max_tokens=int(cfg.get("max_tokens") or 4096),
    )


def _litellm_timeout_raw(cfg: dict[str, Any]) -> Any:
    """``cfg["timeout_seconds"]``, falling back to the deprecated ``timeout``
    alias (warns once per call site) so an existing config keeps working.
    Shared between :func:`_build_litellm` and
    :func:`configured_timeout_seconds` — a per-attempt dispatch
    (``core.prompt._attempt_timeout_seconds``) and the run default
    (``cli.runtime_shim``) both need the same alias resolved, not just the
    adapter instance's own default.
    """
    raw = cfg.get("timeout_seconds")
    if raw is None and cfg.get("timeout") is not None:
        raw = cfg.get("timeout")
        logging.getLogger(__name__).warning(
            "runtime.adapters.litellm.timeout is deprecated; use "
            "runtime.adapters.litellm.timeout_seconds instead."
        )
    return raw


def _build_litellm(cfg: dict[str, Any]) -> Adapter:
    timeout_raw = _litellm_timeout_raw(cfg)
    return LiteLLMAdapter(
        default_model=cfg.get("default_model") or "openai/gpt-4o-mini",
        api_base=cfg.get("api_base") or "",
        timeout=int(timeout_raw or 120),
    )


def configured_timeout_seconds(adapter_name: str, runtime: dict[str, Any] | None) -> int | None:
    """``runtime.adapters.<adapter_name>.timeout_seconds`` for one adapter,
    or ``None`` when nothing usable is configured.

    Normalises ``adapter_name`` the same way :func:`build_adapter` does
    (``.strip().lower()``) so a ``provider: Ollama`` in an orchestration
    still finds a ``runtime.adapters.ollama`` block written in the
    conventional lower-case. Falls back to litellm's deprecated ``timeout``
    alias via :func:`_litellm_timeout_raw`. ``0`` or a negative value counts
    as unset, not "no limit" — callers (the per-attempt dispatch timeout,
    the run default) treat ``None`` as "use the next fallback in the
    precedence chain", and a stray ``0`` must fall through the same way
    rather than being read as unlimited.
    """
    name = (adapter_name or "").strip().lower()
    adapters_cfg = (runtime or {}).get("adapters") or {}
    cfg = adapters_cfg.get(name) or {}
    raw = cfg.get("timeout_seconds")
    if raw is None and name == "litellm":
        raw = _litellm_timeout_raw(cfg)
    if raw is None:
        return None
    try:
        value = int(raw)
    except (TypeError, ValueError):
        return None
    return value if value > 0 else None


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


def _build_mistral(cfg: dict[str, Any]) -> Adapter:
    return MistralAdapter(
        base_url=cfg.get("base_url") or "https://api.mistral.ai/v1",
        default_model=cfg.get("default_model") or "mistral-large-latest",
    )


def _build_ai21(cfg: dict[str, Any]) -> Adapter:
    return AI21Adapter(
        base_url=cfg.get("base_url") or "https://api.ai21.com/studio/v1",
        default_model=cfg.get("default_model") or "jamba-large",
    )


def _build_huggingface_inference(cfg: dict[str, Any]) -> Adapter:
    return HuggingFaceInferenceAdapter(
        base_url=cfg.get("base_url") or "https://router.huggingface.co/v1",
        default_model=cfg.get("default_model")
        or "meta-llama/Llama-3.3-70B-Instruct",
    )


def _build_tgi(cfg: dict[str, Any]) -> Adapter:
    api_key_env = cfg.get("api_key_env", "")
    return TgiAdapter(
        base_url=cfg.get("base_url") or "http://localhost:3000/v1",
        default_model=cfg.get("default_model") or "",
        api_key_env=api_key_env if api_key_env is not None else "",
    )


def _build_cyberdiner(cfg: dict[str, Any]) -> Adapter:
    raw_valid_tiers = cfg.get("valid_tiers") or ()
    if isinstance(raw_valid_tiers, str):
        raw_valid_tiers = (raw_valid_tiers,)
    return CyberdinerAdapter(
        expo_url=cfg.get("expo_url") or "",
        token=cfg.get("token") or "",
        default_tier=cfg.get("default_tier") or "cheap",
        valid_tiers=tuple(
            str(tier).strip() for tier in raw_valid_tiers if str(tier).strip()
        ),
        poll_interval_ms=int(cfg.get("poll_interval_ms") or 500),
        timeout_seconds=int(cfg.get("timeout_seconds") or 30),
        max_in_flight=int(cfg.get("max_in_flight") or 0),
    )


def _build_databricks(cfg: dict[str, Any]) -> Adapter:
    return DatabricksAdapter(
        base_url=cfg.get("base_url") or "",
        default_model=cfg.get("default_model")
        or "databricks-meta-llama-3-3-70b-instruct",
    )


def _build_qwen_dashscope(cfg: dict[str, Any]) -> Adapter:
    return QwenDashScopeAdapter(
        base_url=cfg.get("base_url")
        or "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
        default_model=cfg.get("default_model") or "qwen-max",
    )


def _build_cohere(cfg: dict[str, Any]) -> Adapter:
    return CohereAdapter(
        base_url=cfg.get("base_url") or "https://api.cohere.com",
        default_model=cfg.get("default_model") or "command-r-plus",
    )


def _build_cloudflare_workers_ai(cfg: dict[str, Any]) -> Adapter:
    return CloudflareWorkersAIAdapter(
        base_url=cfg.get("base_url") or "",
        account_id=cfg.get("account_id") or "",
        default_model=cfg.get("default_model")
        or "@cf/meta/llama-3.3-70b-instruct-fp8-fast",
    )


def _build_azure_openai(cfg: dict[str, Any]) -> Adapter:
    return AzureOpenAIAdapter(
        endpoint=cfg.get("endpoint") or cfg.get("base_url") or "",
        api_version=cfg.get("api_version") or "2024-10-21",
        default_model=cfg.get("default_model") or "",
    )


def _build_replicate(cfg: dict[str, Any]) -> Adapter:
    return ReplicateAdapter(
        base_url=cfg.get("base_url") or "https://api.replicate.com/v1",
        default_model=cfg.get("default_model") or "meta/meta-llama-3-70b-instruct",
    )


def _build_watsonx(cfg: dict[str, Any]) -> Adapter:
    return WatsonXAdapter(
        base_url=cfg.get("base_url") or "",
        default_model=cfg.get("default_model")
        or "meta-llama/llama-3-3-70b-instruct",
        api_version=cfg.get("api_version") or "2024-03-14",
    )


def _string_list(
    cfg: dict[str, Any], key: str, *, adapter_name: str, default: tuple[str, ...] = ()
) -> tuple[str, ...]:
    """``cfg[key]`` as a tuple of strings, ``default`` when absent; an empty
    list is kept as empty."""
    raw = cfg.get(key)
    if raw is None:
        return default
    if not isinstance(raw, list) or not all(isinstance(item, str) for item in raw):
        raise ValueError(
            f"runtime.adapters.{adapter_name}.{key} must be a list of strings, got {raw!r}"
        )
    return tuple(raw)


def _build_pi(cfg: dict[str, Any]) -> Adapter:
    return PiAdapter(
        binary=cfg.get("binary") or "pi",
        default_model=cfg.get("default_model") or "",
        thinking=cfg.get("thinking") or "",
        unset_env=_string_list(cfg, "unset_env", adapter_name="pi", default=DEFAULT_UNSET_ENV),
        extra_args=_string_list(cfg, "extra_args", adapter_name="pi"),
    )


def _build_claude_code(cfg: dict[str, Any]) -> Adapter:
    return ClaudeCodeAdapter(
        binary=cfg.get("binary") or "claude",
        default_model=cfg.get("default_model") or "",
        unset_env=_string_list(
            cfg, "unset_env", adapter_name="claude_code", default=DEFAULT_UNSET_ENV
        ),
        extra_args=_string_list(cfg, "extra_args", adapter_name="claude_code"),
    )


def _build_scripted(cfg: dict[str, Any]) -> Adapter:
    return ScriptedAdapter(
        replies_file=cfg.get("replies_file") or "scripted-replies.yaml"
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
    "mistral": _build_mistral,
    "ai21": _build_ai21,
    "huggingface-inference": _build_huggingface_inference,
    "tgi": _build_tgi,
    "cyberdiner": _build_cyberdiner,
    "databricks": _build_databricks,
    "qwen-dashscope": _build_qwen_dashscope,
    "cohere": _build_cohere,
    "cloudflare-workers-ai": _build_cloudflare_workers_ai,
    "azure-openai": _build_azure_openai,
    "replicate": _build_replicate,
    "watsonx": _build_watsonx,
    # Coding-agent CLIs, called through their own login (agent_cli).
    "pi": _build_pi,
    "claude_code": _build_claude_code,
    "scripted": _build_scripted,
}


def _supported_names() -> tuple[str, ...]:
    return tuple(sorted(ADAPTER_REGISTRY.keys()))


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
