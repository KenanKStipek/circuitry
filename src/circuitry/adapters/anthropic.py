from __future__ import annotations

import json
import os
import shutil
from dataclasses import dataclass
from typing import Any, ClassVar

from ..preflight import CheckResult
from ._curl_errors import curl_failure_message
from ._retry import AdapterCallError, classify_curl_exit
from .base import GenerateOptions, GenerateResult, ImageInput, last_user_index


def _image_block(image: ImageInput) -> dict[str, Any]:
    if image.url is not None:
        return {"type": "image", "source": {"type": "url", "url": image.url}}
    return {
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": image.media_type,
            "data": image.base64_data(),
        },
    }


def _request_body(
    *, model: str, prompt: str, max_tokens: int, options: GenerateOptions
) -> dict[str, Any]:
    """The Messages API body: system turns in ``system``, the rest as turns.

    The API has no ``tool`` role outside tool-use blocks, so a bare tool turn
    is sent as a user turn prefixed ``tool:``. Images become image blocks
    ahead of the text of the last user turn, which is added if there is none.
    The API rejects an empty ``messages`` array, so system-only messages
    without images go out as one user turn.
    """
    system = "\n\n".join(m.content for m in options.messages if m.role == "system")
    turns: list[dict[str, Any]] = [
        {"role": "user", "content": f"tool: {m.content}"}
        if m.role == "tool"
        else {"role": m.role, "content": m.content}
        for m in options.messages
        if m.role != "system"
    ]
    if not options.messages:
        turns = [{"role": "user", "content": prompt}]
    elif not turns and not options.images:
        turns = [{"role": "user", "content": system}]
        system = ""
    if options.images:
        index = last_user_index(turns)
        if index is None:
            turns.append({"role": "user", "content": ""})
            index = len(turns) - 1
        text = turns[index]["content"]
        turns[index]["content"] = [
            *(_image_block(image) for image in options.images),
            *([{"type": "text", "text": text}] if text else []),
        ]

    body: dict[str, Any] = {
        "model": model,
        "max_tokens": options.max_tokens if options.max_tokens is not None else max_tokens,
        "messages": turns,
    }
    if system:
        body["system"] = system
    if options.temperature is not None:
        body["temperature"] = options.temperature
    if options.stop:
        body["stop_sequences"] = list(options.stop)
    body.update(options.params)
    return body


@dataclass(frozen=True)
class AnthropicAdapter:
    """
    Adapter for Anthropic API (Claude models).

    Authentication:
      Set ANTHROPIC_API_KEY environment variable (recommended via .env file)

    Config options (in config.json under runtime.adapters.anthropic):
      - base_url: API base URL (defaults to https://api.anthropic.com)
      - default_model: Default model if not specified (defaults to claude-sonnet-5)
      - max_tokens: Maximum tokens to generate (defaults to 4096)
    """

    #: Current model aliases, most capable first. Static on purpose: the
    #: picker should not need an API key or a round trip just to offer
    #: suggestions, and aliases (no dated suffix) always resolve to the
    #: latest snapshot. Any string still passes through to the API.
    KNOWN_MODELS: ClassVar[tuple[str, ...]] = (
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-haiku-4-5",
    )

    accepts_images: ClassVar[bool] = True
    name: str = "anthropic"
    base_url: str = "https://api.anthropic.com"
    default_model: str = "claude-sonnet-5"
    max_tokens: int = 4096

    def list_models(self) -> list[str]:
        """Current Claude model strings — no network call, no API key."""
        return list(self.KNOWN_MODELS)

    def generate(
        self,
        *,
        model: str,
        prompt: str,
        timeout_seconds: int = 120,
        options: GenerateOptions | None = None,
    ) -> GenerateResult:
        import subprocess

        model = model or self.default_model
        api_key = os.environ.get("ANTHROPIC_API_KEY", "")

        if not api_key:
            raise RuntimeError(
                "Anthropic API key not found. Set ANTHROPIC_API_KEY in the environment "
                "(recommended: ~/.config/circuitry/.env). Run `cof doctor` to verify."
            )

        url = f"{self.base_url.rstrip('/')}/v1/messages"

        payload = _request_body(
            model=model,
            prompt=prompt,
            max_tokens=self.max_tokens,
            options=options or GenerateOptions(),
        )

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
                    adapter="anthropic",
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
                f"Anthropic returned non-JSON response: {proc.stdout[:200]}"
            ) from e

        # Extract response text from content blocks
        text = ""
        content = raw.get("content", [])
        if content and isinstance(content, list):
            text_parts = [
                block.get("text", "")
                for block in content
                if isinstance(block, dict) and block.get("type") == "text"
            ]
            text = "".join(text_parts)

        # Extract token usage
        usage = raw.get("usage", {})
        tokens_sent = usage.get("input_tokens")
        tokens_received = usage.get("output_tokens")
        stop_reason = raw.get("stop_reason")

        return GenerateResult(
            text=text.strip() if text else "",
            raw=raw,
            tokens_sent=int(tokens_sent) if tokens_sent is not None else None,
            tokens_received=int(tokens_received)
            if tokens_received is not None
            else None,
            finish_reason=stop_reason if isinstance(stop_reason, str) else None,
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if not os.environ.get("ANTHROPIC_API_KEY"):
            missing.append("env:ANTHROPIC_API_KEY")
        if shutil.which("curl") is None:
            missing.append("binary:curl")
        return CheckResult(ok=not missing, missing=missing)
