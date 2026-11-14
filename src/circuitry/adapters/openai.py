from __future__ import annotations

import json
import os
import shutil
from dataclasses import dataclass
from typing import Any, ClassVar

from ..curl_support import (
    curl_error_code,
    curl_failure_message,
    run_curl,
    run_curl_streaming,
)
from ..preflight import CheckResult
from ._openai_compat import (
    StreamAssembly,
    chat_messages,
    parse_chat_response,
    parse_sse_stream,
    sampling_fields,
    stream_error_retry_info,
    usage_tokens,
)
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
      - api_key_env: Name of the environment variable to read the key from
        (defaults to OPENAI_API_KEY)
    """

    accepts_images: ClassVar[bool] = True
    name: str = "openai"
    base_url: str = "https://api.openai.com/v1"
    default_model: str = "gpt-4o-mini"
    api_key_env: str = "OPENAI_API_KEY"
    streams: bool = True

    def generate(
        self,
        *,
        model: str,
        prompt: str,
        timeout_seconds: int = 120,
        options: GenerateOptions | None = None,
    ) -> GenerateResult:
        options = options or GenerateOptions()
        model = model or self.default_model
        api_key = os.environ.get(self.api_key_env, "")

        if not api_key:
            raise RuntimeError(
                f"OpenAI API key not found. Set {self.api_key_env} in the environment "
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
        if self.streams:
            payload["stream"] = True
            payload["stream_options"] = {"include_usage": True}

        headers = {
            "Content-Type": "application/json",
            "Authorization": f"Bearer {api_key}",
        }
        assembly = StreamAssembly(options.on_progress) if self.streams else None
        try:
            if self.streams:
                assert assembly is not None
                proc = run_curl_streaming(
                    url=url,
                    headers=headers,
                    data=json.dumps(payload),
                    timeout_seconds=timeout_seconds,
                    on_line=assembly.on_line,
                )
            else:
                proc = run_curl(
                    url=url,
                    headers=headers,
                    data=json.dumps(payload),
                    timeout_seconds=timeout_seconds,
                )
        except FileNotFoundError as e:
            raise RuntimeError("curl is not installed or not on PATH") from e

        if proc.returncode != 0:
            # On the streaming path, a non-22 failure leaves whatever
            # OpenAI had already answered in `proc.stdout` -- only a
            # complete HTTP response (22) has a body worth reporting.
            stdout_for_message = proc.stdout if (proc.returncode == 22 or not self.streams) else ""
            raise AdapterCallError(
                curl_failure_message(
                    source="openai",
                    model=model,
                    url=url,
                    returncode=proc.returncode,
                    stdout=stdout_for_message,
                    stderr=proc.stderr,
                    secrets=[api_key],
                ),
                retry_info=classify_curl_exit(proc.returncode, proc.stderr),
                error_code=curl_error_code(stdout_for_message, [api_key]),
            )

        if assembly is not None and assembly.saw_data_line:
            text, finish_reason, usage, saw_done, stream_error = parse_sse_stream(proc.stdout)

            if stream_error is not None:
                error_body = json.dumps({"error": stream_error})
                raise AdapterCallError(
                    curl_failure_message(
                        source="openai",
                        model=model,
                        url=url,
                        returncode=22,
                        stdout=error_body,
                        stderr="",
                        secrets=[api_key],
                    ),
                    retry_info=stream_error_retry_info(stream_error),
                    error_code=curl_error_code(error_body, [api_key]),
                )

            if not saw_done:
                raise AdapterCallError(
                    curl_failure_message(
                        source="openai",
                        model=model,
                        url=url,
                        returncode=18,
                        stdout="",
                        stderr="curl: (18) transfer closed with outstanding read data remaining",
                        secrets=[api_key],
                    ),
                    retry_info=classify_curl_exit(18, ""),
                )

            tokens_sent, tokens_received, tokens_cached_in = usage_tokens(usage)
            return GenerateResult(
                text=text.strip(),
                raw={
                    "choices": [{"message": {"content": text}, "finish_reason": finish_reason}],
                    "usage": usage,
                },
                tokens_sent=tokens_sent,
                tokens_received=tokens_received,
                tokens_cached_in=tokens_cached_in,
                finish_reason=finish_reason,
            )

        try:
            raw = json.loads(proc.stdout)
        except json.JSONDecodeError as e:
            raise RuntimeError(
                f"OpenAI returned non-JSON response: {proc.stdout[:200]}"
            ) from e

        text, finish_reason = parse_chat_response(raw)

        # Extract token usage. This adapter's own non-streamed path has
        # always cast with a lenient `int(x) if x is not None` (truncating
        # a float, raising on a bad string) rather than `usage_tokens`'s
        # own strict `isinstance(x, int)` check its streamed path uses --
        # kept exactly as it was; only `tokens_cached_in` is new.
        usage = raw.get("usage", {})
        tokens_sent = usage.get("prompt_tokens")
        tokens_received = usage.get("completion_tokens")
        _, _, tokens_cached_in = usage_tokens(usage)

        return GenerateResult(
            text=text,
            raw=raw,
            tokens_sent=int(tokens_sent) if tokens_sent is not None else None,
            tokens_received=int(tokens_received)
            if tokens_received is not None
            else None,
            tokens_cached_in=tokens_cached_in,
            finish_reason=finish_reason,
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if not os.environ.get(self.api_key_env):
            missing.append(f"env:{self.api_key_env}")
        if shutil.which("curl") is None:
            missing.append("binary:curl")
        return CheckResult(ok=not missing, missing=missing)
