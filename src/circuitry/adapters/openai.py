from __future__ import annotations

import json
import os
from dataclasses import dataclass

from .base import GenerateResult


@dataclass(frozen=True)
class OpenAIAdapter:
    """
    Adapter for OpenAI API.

    Authentication:
      Set OPENAI_API_KEY environment variable (recommended via .env file)

    Config options (in config.json under runtime.adapters.openai):
      - base_url: API base URL (defaults to https://api.openai.com/v1)
      - default_model: Default model if not specified (defaults to gpt-4o-mini)
    """

    name: str = "openai"
    base_url: str = "https://api.openai.com/v1"
    default_model: str = "gpt-4o-mini"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        import shlex
        import subprocess

        model = model or self.default_model
        api_key = os.environ.get("OPENAI_API_KEY", "")

        if not api_key:
            raise RuntimeError(
                "OpenAI API key not found. Set OPENAI_API_KEY environment variable."
            )

        url = f"{self.base_url.rstrip('/')}/chat/completions"

        payload = {
            "model": model,
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
            f"Authorization: Bearer {api_key}",
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
                f"OpenAI returned non-JSON response: {proc.stdout[:200]}"
            ) from e

        # Extract response text
        text = ""
        choices = raw.get("choices", [])
        if choices and isinstance(choices, list):
            message = choices[0].get("message", {})
            text = message.get("content", "")

        # Extract token usage
        usage = raw.get("usage", {})
        tokens_sent = usage.get("prompt_tokens")
        tokens_received = usage.get("completion_tokens")

        return GenerateResult(
            text=text.strip() if text else "",
            raw=raw,
            tokens_sent=int(tokens_sent) if tokens_sent is not None else None,
            tokens_received=int(tokens_received)
            if tokens_received is not None
            else None,
        )
