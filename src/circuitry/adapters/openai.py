from __future__ import annotations

import json
import os
import shutil
from dataclasses import dataclass
from typing import Any, ClassVar

from ..preflight import CheckResult
from ._curl_errors import curl_failure_message
from ._openai_compat import chat_messages, parse_chat_response, sampling_fields
from ._retry import AdapterCallError, classify_curl_exit
from .base import DETERMINISTIC_SEED, GenerateOptions, GenerateResult


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

    accepts_images: ClassVar[bool] = True
    name: str = "openai"
    base_url: str = "https://api.openai.com/v1"
    default_model: str = "gpt-4o-mini"

    def generate(
        self,
        *,
        model: str,
        prompt: str,
        timeout_seconds: int = 120,
        options: GenerateOptions | None = None,
    ) -> GenerateResult:
        import subprocess

        options = options or GenerateOptions()
        model = model or self.default_model
        api_key = os.environ.get("OPENAI_API_KEY", "")

        if not api_key:
            raise RuntimeError(
                "OpenAI API key not found. Set OPENAI_API_KEY in the environment "
                "(recommended: ~/.config/circuitry/.env). Run `cof doctor` to verify."
            )

        url = f"{self.base_url.rstrip('/')}/chat/completions"

        payload: dict[str, Any] = {
            "model": model,
            "messages": chat_messages(prompt, options),
            # max_tokens is deprecated here and rejected by reasoning models.
            **sampling_fields(options, max_tokens_key="max_completion_tokens"),
        }
        if options.deterministic:
            payload.setdefault("seed", DETERMINISTIC_SEED)

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
            # The body goes on stdin: with base64 images it can outgrow the
            # argv size limit (128 KiB per argument on Linux).
            "--data-binary",
            "@-",
            url,
        ]

        try:
            proc = subprocess.run(
                cmd,
                input=json.dumps(payload),
                capture_output=True,
                text=True,
                check=False,
            )
        except FileNotFoundError as e:
            raise RuntimeError("curl is not installed or not on PATH") from e

        if proc.returncode != 0:
            raise AdapterCallError(
                curl_failure_message(
                    adapter="openai",
                    model=model,
                    url=url,
                    returncode=proc.returncode,
                    stdout=proc.stdout,
                    stderr=proc.stderr,
                    secrets=[api_key],
                ),
                retry_info=classify_curl_exit(proc.returncode, proc.stderr),
            )

        try:
            raw = json.loads(proc.stdout)
        except json.JSONDecodeError as e:
            raise RuntimeError(
                f"OpenAI returned non-JSON response: {proc.stdout[:200]}"
            ) from e

        text, finish_reason = parse_chat_response(raw)

        # Extract token usage
        usage = raw.get("usage", {})
        tokens_sent = usage.get("prompt_tokens")
        tokens_received = usage.get("completion_tokens")

        return GenerateResult(
            text=text,
            raw=raw,
            tokens_sent=int(tokens_sent) if tokens_sent is not None else None,
            tokens_received=int(tokens_received)
            if tokens_received is not None
            else None,
            finish_reason=finish_reason,
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if not os.environ.get("OPENAI_API_KEY"):
            missing.append("env:OPENAI_API_KEY")
        if shutil.which("curl") is None:
            missing.append("binary:curl")
        return CheckResult(ok=not missing, missing=missing)
