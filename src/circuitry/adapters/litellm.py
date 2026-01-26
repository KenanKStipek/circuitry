from __future__ import annotations

import os
from dataclasses import dataclass
from typing import Any

from .base import GenerateResult


@dataclass(frozen=True)
class LiteLLMAdapter:
    """
    Adapter for LiteLLM - a unified interface for 100+ LLM providers.

    LiteLLM supports OpenAI, Anthropic, Azure, Hugging Face, Ollama, and many more
    through a unified API. Model names follow the format: provider/model-name

    Examples:
      - openai/gpt-4o-mini
      - anthropic/claude-sonnet-4-20250514
      - ollama/llama3.2
      - azure/gpt-4
      - huggingface/meta-llama/Llama-2-7b-chat-hf

    Authentication:
      Set appropriate API keys as environment variables (recommended via .env file):
      - OPENAI_API_KEY for OpenAI models
      - ANTHROPIC_API_KEY for Anthropic models
      - AZURE_API_KEY for Azure models
      - etc.

    Requirements:
      pip install litellm

    Config options (in config.json under runtime.adapters.litellm):
      - default_model: Default model if not specified (defaults to openai/gpt-4o-mini)
      - api_base: Optional API base URL override
      - timeout: Request timeout in seconds
    """

    name: str = "litellm"
    default_model: str = "openai/gpt-4o-mini"
    api_base: str = ""
    timeout: int = 120

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        try:
            import litellm
        except ImportError as e:
            raise RuntimeError(
                "litellm package not installed. Install with: pip install litellm"
            ) from e

        model = model or self.default_model
        timeout = timeout_seconds or self.timeout

        # Build kwargs
        kwargs: dict[str, Any] = {
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "timeout": timeout,
        }

        if self.api_base:
            kwargs["api_base"] = self.api_base

        try:
            response = litellm.completion(**kwargs)
        except Exception as e:
            raise RuntimeError(f"LiteLLM request failed: {e}") from e

        # Extract response - litellm returns OpenAI-compatible format
        raw = (
            response.model_dump() if hasattr(response, "model_dump") else dict(response)
        )

        text = ""
        choices = getattr(response, "choices", [])
        if choices:
            message = getattr(choices[0], "message", None)
            if message:
                text = getattr(message, "content", "") or ""

        # Extract token usage
        usage = getattr(response, "usage", None)
        tokens_sent = None
        tokens_received = None

        if usage:
            tokens_sent = getattr(usage, "prompt_tokens", None)
            tokens_received = getattr(usage, "completion_tokens", None)

        return GenerateResult(
            text=text.strip() if text else "",
            raw=raw,
            tokens_sent=int(tokens_sent) if tokens_sent is not None else None,
            tokens_received=int(tokens_received)
            if tokens_received is not None
            else None,
        )
