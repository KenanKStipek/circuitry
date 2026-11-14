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
from ._retry import AdapterCallError, RetryInfo, classify_curl_exit, status_is_retryable
from .base import (
    GenerateOptions,
    GenerateProgress,
    GenerateResult,
    ImageInput,
    last_user_index,
)

#: Anthropic's own ``error.type`` values, mapped to the HTTP status the
#: non-streaming API would have answered with for the same failure — so a
#: stream-level ``error`` event classifies for retry exactly like an HTTP
#: error response does.
_STREAM_ERROR_STATUS: dict[str, int] = {
    "invalid_request_error": 400,
    "authentication_error": 401,
    "permission_error": 403,
    "not_found_error": 404,
    "request_too_large": 413,
    "rate_limit_error": 429,
    "api_error": 500,
    "overloaded_error": 529,
}


def _cache_fields(usage: Any) -> tuple[int | None, int | None]:
    """``(cache_read_input_tokens, cache_creation_input_tokens)`` from an
    Anthropic ``usage`` object, each ``None`` unless the reply's own field
    is an int -- absent, not 0, when the provider didn't report one.
    """
    if not isinstance(usage, dict):
        return None, None
    read = usage.get("cache_read_input_tokens")
    write = usage.get("cache_creation_input_tokens")
    return (
        int(read) if isinstance(read, int) else None,
        int(write) if isinstance(write, int) else None,
    )


class _AnthropicStream:
    """Drives *on_progress* from each SSE delta as it arrives — never the
    source of the final result, which is parsed from the complete body
    once the call finishes (:func:`parse_anthropic_stream`), exactly the
    way the non-streamed path parses its one JSON object. ``saw_event`` is
    ``False`` when the server answered with no recognisable SSE event at
    all (a plain JSON body instead), the same fallback signal
    :class:`~circuitry.adapters._openai_compat.StreamAssembly` has.
    """

    def __init__(self, on_progress: Any) -> None:
        self._on_progress = on_progress
        #: The most recently reported exact input-token count
        #: (``message_start``, cache reads/writes included), carried onto
        #: every later progress call so ``meta.live.tokens_sent`` stays
        #: populated once known.
        self._tokens_sent: int | None = None
        self.saw_event = False

    def on_line(self, line: str) -> None:
        line = line.strip()
        if not line.startswith("data:"):
            return
        payload = line[len("data:") :].strip()
        if not payload:
            return
        try:
            event = json.loads(payload)
        except json.JSONDecodeError:
            return
        if not isinstance(event, dict):
            return
        self.saw_event = True
        etype = event.get("type")
        if etype == "message_start":
            message = event.get("message") or {}
            usage = message.get("usage") if isinstance(message, dict) else None
            input_tokens = usage.get("input_tokens") if isinstance(usage, dict) else None
            if isinstance(input_tokens, int):
                cache_read, cache_write = _cache_fields(usage)
                self._tokens_sent = input_tokens + (cache_read or 0) + (cache_write or 0)
                # Not "a delta" (#674): this carries only the input token
                # count, no reply text, so it never gates the per-second
                # throttle or counts as the first token.
                if self._on_progress is not None:
                    self._on_progress(
                        GenerateProgress(
                            delta_chars=0, delta_count=1, tokens_sent=self._tokens_sent
                        )
                    )
        elif etype == "content_block_delta":
            delta = event.get("delta") or {}
            text = delta.get("text") if isinstance(delta, dict) else None
            if isinstance(text, str) and text and self._on_progress is not None:
                self._on_progress(
                    GenerateProgress(
                        delta_chars=len(text), delta_count=1, tokens_sent=self._tokens_sent
                    )
                )


def parse_anthropic_stream(
    stdout: str,
) -> tuple[
    str,
    int | None,
    int | None,
    int | None,
    int | None,
    str | None,
    bool,
    str | None,
    str | None,
]:
    """``(text, tokens_sent, tokens_received, tokens_cached_in,
    tokens_cache_write, stop_reason, saw_message_stop, error_message,
    error_type)`` from a completed Anthropic Messages API SSE stream,
    parsed in full — the authoritative result for a streamed call, built
    the same way the non-streamed path builds its one JSON object's.
    ``message_start`` reports the exact input token count, cache reads and
    writes included (``tokens_sent`` is their sum); ``content_block_delta``
    carries the text; ``message_delta`` carries the stop reason and the
    exact output token count; ``error`` carries the same explanation an
    HTTP error body would; ``saw_message_stop`` is whether the stream's own
    terminal event was seen.
    """
    text_parts: list[str] = []
    tokens_sent: int | None = None
    tokens_received: int | None = None
    tokens_cached_in: int | None = None
    tokens_cache_write: int | None = None
    stop_reason: str | None = None
    saw_message_stop = False
    error_message: str | None = None
    error_type: str | None = None
    for raw_line in stdout.splitlines():
        line = raw_line.strip()
        if not line.startswith("data:"):
            continue
        payload = line[len("data:") :].strip()
        if not payload:
            continue
        try:
            event = json.loads(payload)
        except json.JSONDecodeError:
            continue
        if not isinstance(event, dict):
            continue
        etype = event.get("type")
        if etype == "error":
            if error_message is None:
                error = event.get("error") or {}
                message = error.get("message") if isinstance(error, dict) else None
                error_message = message if isinstance(message, str) else json.dumps(error)
                error_type = error.get("type") if isinstance(error, dict) else None
            continue
        if etype == "message_start":
            message = event.get("message") or {}
            usage = message.get("usage") if isinstance(message, dict) else None
            input_tokens = usage.get("input_tokens") if isinstance(usage, dict) else None
            if isinstance(input_tokens, int):
                tokens_cached_in, tokens_cache_write = _cache_fields(usage)
                tokens_sent = input_tokens + (tokens_cached_in or 0) + (tokens_cache_write or 0)
        elif etype == "content_block_delta":
            delta = event.get("delta") or {}
            text = delta.get("text") if isinstance(delta, dict) else None
            if isinstance(text, str) and text:
                text_parts.append(text)
        elif etype == "message_delta":
            delta = event.get("delta") or {}
            reason = delta.get("stop_reason") if isinstance(delta, dict) else None
            if isinstance(reason, str):
                stop_reason = reason
            usage = event.get("usage")
            output_tokens = usage.get("output_tokens") if isinstance(usage, dict) else None
            if isinstance(output_tokens, int):
                tokens_received = output_tokens
        elif etype == "message_stop":
            saw_message_stop = True
    return (
        "".join(text_parts),
        tokens_sent,
        tokens_received,
        tokens_cached_in,
        tokens_cache_write,
        stop_reason,
        saw_message_stop,
        error_message,
        error_type,
    )


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
      - api_key_env: Name of the environment variable to read the key from
        (defaults to ANTHROPIC_API_KEY)
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
    api_key_env: str = "ANTHROPIC_API_KEY"
    #: Whether this instance sends ``stream: true``; the instance-level
    #: override for ``runtime.adapters.<name>.stream: false``.
    streams: bool = True

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
        model = model or self.default_model
        api_key = os.environ.get(self.api_key_env, "")

        if not api_key:
            raise RuntimeError(
                f"Anthropic API key not found. Set {self.api_key_env} in the environment "
                "(recommended: ~/.config/circuitry/.env). Run `cof doctor` to verify."
            )

        url = f"{self.base_url.rstrip('/')}/v1/messages"

        options = options or GenerateOptions()
        payload = _request_body(
            model=model,
            prompt=prompt,
            max_tokens=self.max_tokens,
            options=options,
        )
        if self.streams:
            payload["stream"] = True

        headers = {
            "Content-Type": "application/json",
            "x-api-key": api_key,
            "anthropic-version": "2023-06-01",
        }
        assembly = _AnthropicStream(options.on_progress) if self.streams else None
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
            # Anthropic had already answered in `proc.stdout` -- only a
            # complete HTTP response (22) has a body worth reporting.
            stdout_for_message = proc.stdout if (proc.returncode == 22 or not self.streams) else ""
            raise AdapterCallError(
                curl_failure_message(
                    source="anthropic",
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

        if assembly is not None and assembly.saw_event:
            (
                text,
                tokens_sent,
                tokens_received,
                tokens_cached_in,
                tokens_cache_write,
                stop_reason,
                saw_message_stop,
                error_message,
                error_type,
            ) = parse_anthropic_stream(proc.stdout)

            if error_message is not None:
                status = _STREAM_ERROR_STATUS.get(error_type or "")
                # Rendered through the same `curl_failure_message` the
                # HTTP error path uses, from a synthesized body carrying
                # the same `error.message`/`error.type` shape Anthropic's
                # own HTTP error response has -- so the two paths give
                # byte-identical text for the same error, masking included.
                error_body = json.dumps(
                    {"error": {"type": error_type, "message": error_message}}
                )
                raise AdapterCallError(
                    curl_failure_message(
                        source="anthropic",
                        model=model,
                        url=url,
                        returncode=22,
                        stdout=error_body,
                        stderr="",
                        secrets=[api_key],
                    ),
                    retry_info=RetryInfo(
                        retryable=status_is_retryable(status) if status is not None else False,
                        status=status,
                    ),
                    error_code=error_type,
                )

            if not saw_message_stop:
                # The stream ended (curl exited 0) without a `message_stop`
                # event -- classified the same way an actual mid-stream
                # disconnect is.
                raise AdapterCallError(
                    curl_failure_message(
                        source="anthropic",
                        model=model,
                        url=url,
                        returncode=18,
                        stdout="",
                        stderr="curl: (18) transfer closed with outstanding read data remaining",
                        secrets=[api_key],
                    ),
                    retry_info=classify_curl_exit(18, ""),
                )

            input_tokens = (
                tokens_sent - (tokens_cached_in or 0) - (tokens_cache_write or 0)
                if tokens_sent is not None
                else None
            )
            return GenerateResult(
                text=text.strip(),
                raw={
                    "content": [{"type": "text", "text": text}],
                    "usage": {
                        "input_tokens": input_tokens,
                        "output_tokens": tokens_received,
                        "cache_read_input_tokens": tokens_cached_in,
                        "cache_creation_input_tokens": tokens_cache_write,
                    },
                    "stop_reason": stop_reason,
                },
                tokens_sent=tokens_sent,
                tokens_received=tokens_received,
                tokens_cached_in=tokens_cached_in,
                tokens_cache_write=tokens_cache_write,
                finish_reason=stop_reason,
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

        # Extract token usage. `tokens_sent` is every input token Anthropic
        # counted, cache reads and writes included -- `input_tokens` alone
        # excludes the cache.
        usage = raw.get("usage", {})
        input_tokens = usage.get("input_tokens")
        tokens_received = usage.get("output_tokens")
        tokens_cached_in, tokens_cache_write = _cache_fields(usage)
        tokens_sent = (
            int(input_tokens) + (tokens_cached_in or 0) + (tokens_cache_write or 0)
            if input_tokens is not None
            else None
        )
        stop_reason = raw.get("stop_reason")

        return GenerateResult(
            text=text.strip() if text else "",
            raw=raw,
            tokens_sent=tokens_sent,
            tokens_received=int(tokens_received)
            if tokens_received is not None
            else None,
            tokens_cached_in=tokens_cached_in,
            tokens_cache_write=tokens_cache_write,
            finish_reason=stop_reason if isinstance(stop_reason, str) else None,
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if not os.environ.get(self.api_key_env):
            missing.append(f"env:{self.api_key_env}")
        if shutil.which("curl") is None:
            missing.append("binary:curl")
        return CheckResult(ok=not missing, missing=missing)
