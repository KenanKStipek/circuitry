from __future__ import annotations

import json
import os
from dataclasses import dataclass

from .base import GenerateResult


@dataclass(frozen=True)
class AnthropicAdapter:
    """
    Adapter for Anthropic API (Claude models).

    Authentication:
      Set ANTHROPIC_API_KEY environment variable (recommended via .env file)

    Config options (in config.json under runtime.adapters.anthropic):
      - base_url: API base URL (defaults to https://api.anthropic.com)
      - default_model: Default model if not specified (defaults to claude-sonnet-4-20250514)
      - max_tokens: Maximum tokens to generate (defaults to 4096)
    """

    name: str = "anthropic"
    base_url: str = "https://api.anthropic.com"
    default_model: str = "claude-sonnet-4-20250514"
    max_tokens: int = 4096

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        import shlex
        import subprocess

        model = model or self.default_model
        api_key = os.environ.get("ANTHROPIC_API_KEY", "")

        if not api_key:
            raise RuntimeError(
                "Anthropic API key not found. Set ANTHROPIC_API_KEY environment variable."
            )

        url = f"{self.base_url.rstrip('/')}/v1/messages"

        payload = {
            "model": model,
            "max_tokens": self.max_tokens,
            "messages": [{"role": "user", "content": prompt}],
        }

        cmd = [
            "curl",
            "--silent",
            "--show-error",
            "--fail-with-body",
            "--max-time",
            str(int(timeout_seconds)),
            "-H",
            "Content-Type: application/json",
            "-H",
            f"x-api-key: {api_key}",
            "-H",
            "anthropic-version: 2023-06-01",
            "-d",
            json.dumps(payload),
            url,
        ]

        try:
            proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
        except FileNotFoundError as e:
            raise RuntimeError("curl is not installed or not on PATH") from e

        if proc.returncode != 0:
            # Mask API key in error message
            safe_cmd = " ".join(shlex.quote(c) for c in cmd).replace(api_key, "***")
            err = (proc.stderr or proc.stdout or "").strip()
            raise RuntimeError(
                f"curl failed (exit {proc.returncode}). cmd={safe_cmd}. error={err}"
            )

        try:
            raw = json.loads(proc.stdout)
        except json.JSONDecodeError as e:
            raise RuntimeError(
                f"Anthropic returned non-JSON response: {proc.stdout[:200]}"
            ) from e

        # Extract response text from content blocks
        text = ""
        content = raw.get("content", [])
        if content and isinstance(content, list):
            text_parts = []
            for block in content:
                if isinstance(block, dict) and block.get("type") == "text":
                    text_parts.append(block.get("text", ""))
            text = "".join(text_parts)

        # Extract token usage
        usage = raw.get("usage", {})
        tokens_sent = usage.get("input_tokens")
        tokens_received = usage.get("output_tokens")

        return GenerateResult(
            text=text.strip() if text else "",
            raw=raw,
            tokens_sent=int(tokens_sent) if tokens_sent is not None else None,
            tokens_received=int(tokens_received)
            if tokens_received is not None
            else None,
        )
