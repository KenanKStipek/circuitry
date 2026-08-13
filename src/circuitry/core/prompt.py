from __future__ import annotations

import json
import logging
import time
from contextlib import nullcontext
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Callable, Literal, Optional, Sequence

from ..adapters import Adapter, build_adapter
from ..adapters.base import GenerateResult
from ..output import console as _console
from .store import Store

logger = logging.getLogger(__name__)


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _elapsed_str(seconds: float) -> str:
    if seconds >= 1:
        return f"{seconds:.2f}s"
    return f"{seconds * 1000:.0f}ms"


def _adapter_target(adapter: Any, model: str) -> str:
    """Return a human-readable 'adapter · model @ host' string."""
    from urllib.parse import urlparse

    adapter_name = getattr(adapter, "name", "unknown")
    base_url = getattr(adapter, "base_url", None) or getattr(adapter, "api_base", None)
    if base_url:
        host = urlparse(str(base_url)).hostname or str(base_url)
        return f"{adapter_name} · {model} @ {host}"
    return f"{adapter_name} · {model}"


class _PromptSpinner:
    """Animated single-line spinner for a prompt running in chain/sequential mode."""

    _SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"

    def __init__(
        self,
        name: str,
        target: str = "",
        token_hint: str = "",
        indent: str = "",
        ancestors: list | None = None,
    ) -> None:
        self._name = name
        self._target = target  # e.g. "ollama · model @ host"
        self._token_hint = token_hint  # e.g. "~374tok ↑"
        self._indent = indent
        self._start = time.monotonic()
        self._ancestors = ancestors or []

    def __rich__(self) -> str:
        from .dynamic import _render_ancestors

        elapsed = time.monotonic() - self._start
        char = self._SPINNER[int(elapsed * 8) % len(self._SPINNER)]
        parts: list[str] = []
        if self._target:
            parts.append(self._target)
        parts.append(_elapsed_str(elapsed))
        if self._token_hint:
            parts.append(self._token_hint)
        suffix = " | ".join(parts)
        lines = _render_ancestors(self._ancestors, self._SPINNER)
        lines.append(
            f"{self._indent}[info]{char}[/info] [cyan]◆[/cyan]"
            f" {self._name} [dim]{suffix}[/dim]"
        )
        return "\n".join(lines)


def _render(template: str, ctx: dict[str, Any]) -> str:
    try:
        import chevron  # type: ignore

        return chevron.render(template, ctx)
    except Exception:
        logger.warning("Chevron template rendering failed; returning raw template", exc_info=True)
        return template


# Prompt types per the spec
PromptType = Literal["text", "json", "boolean", "tool", "number", "array", "object"]


@dataclass(frozen=True)
class MessageDef:
    """A single message in a messages-based prompt."""

    role: Literal["system", "user", "assistant", "tool"]
    content: str


@dataclass(frozen=True)
class AssetRefDef:
    """Reference to a non-text asset (image, file, audio)."""

    kind: str  # e.g. "image", "file", "audio"
    ref: str  # resolvable id/path/uri


@dataclass(frozen=True)
class RetryPolicyDef:
    """Retry configuration for prompts."""

    max_attempts: int = 1
    backoff_ms: int = 1000


@dataclass(frozen=True)
class PromptDefinition:
    """
    A Prompt is the atomic execution unit in Circuitry.

    Per the spec:
    - Exactly one of 'template' or 'messages' must be provided
    - prompt_type defines the expected output shape
    - schema provides JSON Schema for validation (if applicable)
    """

    name: str

    # Primary input form (exactly one must be provided)
    template: Optional[str] = None
    messages: Optional[Sequence[MessageDef]] = None

    # Typing and decoding
    prompt_type: PromptType = "text"
    schema: Optional[dict[str, Any]] = None

    # Model configuration
    model: Optional[str] = None
    provider: Optional[str] = None
    provider_fallbacks: Optional[Sequence[str]] = None

    # Execution parameters
    params: Optional[dict[str, Any]] = None
    timeout_ms: Optional[int] = None
    deterministic: bool = False

    # Prompt-local structured values
    inputs: Optional[dict[str, Any]] = None

    # Non-text inputs
    assets: Optional[Sequence[AssetRefDef]] = None

    # Reliability
    retries: Optional[RetryPolicyDef] = None
    on_error: Literal["fail", "skip", "continue"] = "fail"

    # Description (for documentation/LLM guidance)
    description: Optional[str] = None

    # False = skip execution and write a disabled node (see core.disabled).
    enabled: bool = True


class PromptRuntime:
    """
    Executes a PromptDefinition against adapter + store.
    Writes:
      <name>.value
      <name>.meta{created_at, completed_at, adapter, model, prompt_sent, tokens_sent, tokens_received, error}
    """

    def __init__(
        self,
        definition: PromptDefinition,
        *,
        adapter: Adapter,
        model: str,
        runtime_config: dict[str, Any] | None = None,
        dry_run: bool = False,
        timeout_seconds: int = 120,
        verbose: bool = False,
        depth: int = 0,
        cb_start: Callable[[], None] | None = None,
        cb_done: Callable[[str], None] | None = None,
        cb_error: Callable[[str], None] | None = None,
        cb_running: Callable[[str, int], None] | None = None,
        display_name: str | None = None,
        ancestors: list | None = None,
    ):
        self.defn = definition
        self.adapter = adapter
        self.model = model
        self.runtime_config = runtime_config or {}
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds
        self.verbose = verbose
        self.depth = depth
        self.cb_start = cb_start
        self.cb_done = cb_done
        self.cb_error = cb_error
        self.cb_running = cb_running
        self.display_name = display_name or definition.name
        self._ancestors = ancestors or []

    def execute(self, *, store: Store, ctx: dict[str, Any]) -> None:
        node = store.ensure_dict(self.defn.name)
        node.setdefault("value", None)
        meta = node.get("meta")
        if not isinstance(meta, dict):
            meta = {}
            node["meta"] = meta

        # Build effective context with prompt-local inputs
        effective_ctx = dict(ctx)
        if self.defn.inputs:
            effective_ctx.update(self.defn.inputs)

        # Materialize prompt input
        prompt_sent = self._materialize_input(effective_ctx)

        # Record metadata
        meta["created_at"] = _now_iso()
        meta["completed_at"] = None
        meta["adapter"] = getattr(self.adapter, "name", "unknown")
        meta["model"] = self.model
        meta["prompt_type"] = self.defn.prompt_type
        meta["prompt_sent"] = prompt_sent
        meta["tokens_sent"] = None
        meta["tokens_received"] = None
        meta["error"] = None
        meta["dry_run"] = self.dry_run
        meta["fallback_attempts"] = []
        meta["fallback_recovered"] = False

        indent = "  " * self.depth
        estimated_out = len(prompt_sent) // 4
        resolved_model = self.defn.model or self.model
        t0 = time.monotonic()
        target = _adapter_target(self.adapter, resolved_model) if self.verbose else ""

        if self.verbose and self.cb_start is not None:
            self.cb_start()
        if self.verbose and self.cb_running is not None:
            self.cb_running(target, estimated_out)

        if self.dry_run:
            node["value"] = None
            meta["completed_at"] = _now_iso()
            if self.verbose:
                elapsed = time.monotonic() - t0
                if self.cb_done is not None:
                    line = (
                        f"{indent}[ok]✓[/ok] [cyan]◆[/cyan] {self.display_name}"
                        f" [dim]{target} | {_elapsed_str(elapsed)}[/dim]"
                    )
                    self.cb_done(line)
                else:
                    _console.print(
                        f"{indent}[ok]✓[/ok] [cyan]◆[/cyan] {self.display_name}"
                        f" [dim]{_elapsed_str(elapsed)}[/dim]"
                    )
            store.fire_effect_complete(self.defn.name, node)
            return

        # Determine retry policy: per-prompt config > runtime default > 1 (no retry)
        if self.defn.retries is not None:
            max_attempts = self.defn.retries.max_attempts
            backoff_ms = self.defn.retries.backoff_ms
        else:
            max_attempts = int(self.runtime_config.get("default_prompt_retries", 1))
            backoff_ms = 1000

        attempts_meta: list[dict[str, Any]] = []
        try:
            resolved_model = self.defn.model or self.model
            attempts = self._build_attempts(default_model=resolved_model)

            for _attempt in range(max_attempts):
                if _attempt > 0:
                    time.sleep(backoff_ms / 1000)
                    t0 = time.monotonic()
                    if self.verbose:
                        retry_line = (
                            f"{indent}[yellow]↺[/yellow] [cyan]◆[/cyan]"
                            f" {self.display_name} [dim]retry {_attempt}/{max_attempts - 1}[/dim]"
                        )
                        if self.cb_done is not None:
                            self.cb_done(retry_line)
                        else:
                            _console.print(retry_line)

                try:
                    if self.verbose and self.cb_start is None:
                        from rich.live import Live

                        live_cm = Live(
                            _PromptSpinner(
                                name=self.display_name,
                                target=target,
                                token_hint=f"~{estimated_out}tok ↑",
                                indent=indent,
                                ancestors=self._ancestors,
                            ),
                            refresh_per_second=10,
                            transient=True,
                            console=_console,
                        )
                    else:
                        live_cm = nullcontext()
                    with live_cm:
                        res, attempts_meta, generation_error = self._generate_with_fallbacks(
                            prompt=prompt_sent, attempts=attempts
                        )
                    if generation_error is not None or res is None:
                        raise RuntimeError(
                            f"All adapter attempts failed: {attempts_meta}"
                        ) from generation_error

                    # Decode and validate output based on prompt_type
                    decoded_value = self._decode_output(res.text)

                    # Validate against schema if provided
                    if self.defn.schema and self.defn.prompt_type in (
                        "json",
                        "object",
                        "array",
                    ):
                        self._validate_schema(decoded_value)

                    # Success
                    node["value"] = decoded_value
                    meta["tokens_sent"] = res.tokens_sent
                    meta["tokens_received"] = res.tokens_received
                    meta["fallback_attempts"] = attempts_meta
                    meta["fallback_recovered"] = len(attempts_meta) > 1
                    meta["completed_at"] = _now_iso()
                    if attempts_meta:
                        last = attempts_meta[-1]
                        meta["adapter"] = last["adapter"]
                        meta["model"] = last["model"]
                    if _attempt > 0:
                        meta["retries_used"] = _attempt

                    if self.verbose:
                        elapsed = time.monotonic() - t0
                        suffix = _elapsed_str(elapsed)
                        sent = res.tokens_sent
                        recv = res.tokens_received
                        if sent is not None or recv is not None:
                            suffix += f" | ↑{sent or 0} ↓{recv or 0} tok"
                        line = (
                            f"{indent}[ok]✓[/ok] [cyan]◆[/cyan] {self.display_name}"
                            f" [dim]{target} | {suffix}[/dim]"
                        )
                        if self.cb_done is not None:
                            self.cb_done(line)
                        else:
                            _console.print(line)

                    store.fire_effect_complete(self.defn.name, node)
                    return

                except Exception:
                    if _attempt < max_attempts - 1:
                        # Show failure for this attempt, then retry
                        if self.verbose:
                            elapsed = time.monotonic() - t0
                            line = (
                                f"{indent}[err]✗[/err] [cyan]◆[/cyan] {self.display_name}"
                                f" [dim]{target} | {_elapsed_str(elapsed)}[/dim]"
                            )
                            if self.cb_error is not None:
                                self.cb_error(line)
                            else:
                                _console.print(line)
                        continue
                    raise  # Last attempt — propagate to outer handler

        except Exception as e:
            if self.verbose:
                elapsed = time.monotonic() - t0
                line = (
                    f"{indent}[err]✗[/err] [cyan]◆[/cyan] {self.display_name}"
                    f" [dim]{target} | {_elapsed_str(elapsed)}[/dim]"
                )
                if self.cb_error is not None:
                    self.cb_error(line)
                else:
                    _console.print(line)
            meta["fallback_attempts"] = attempts_meta
            meta["fallback_recovered"] = False
            meta["error"] = str(e)
            meta["completed_at"] = _now_iso()
            if self.defn.on_error == "fail":
                raise
            elif self.defn.on_error == "skip":
                node["value"] = None
            # continue: keep going with None value
            store.fire_effect_complete(self.defn.name, node)

    def _build_attempts(self, *, default_model: str) -> list[tuple[str, str]]:
        attempts: list[tuple[str, str]] = []

        primary_adapter = getattr(self.adapter, "name", "unknown")
        attempts.append((primary_adapter, default_model))

        if self.defn.provider:
            attempts.insert(
                0, self._parse_provider_token(self.defn.provider, default_model)
            )

        for provider_token in self.defn.provider_fallbacks or ():
            attempts.append(self._parse_provider_token(provider_token, default_model))

        deduped: list[tuple[str, str]] = []
        seen: set[tuple[str, str]] = set()
        for attempt in attempts:
            if attempt not in seen:
                seen.add(attempt)
                deduped.append(attempt)
        return deduped

    def _parse_provider_token(self, token: str, default_model: str) -> tuple[str, str]:
        parsed = (token or "").strip()
        if not parsed:
            return (getattr(self.adapter, "name", "unknown"), default_model)
        if ":" not in parsed:
            return (parsed, default_model)
        adapter_name, model_name = parsed.split(":", 1)
        adapter_name = adapter_name.strip()
        model_name = model_name.strip() or default_model
        return (adapter_name, model_name)

    def _generate_with_fallbacks(
        self, *, prompt: str, attempts: list[tuple[str, str]]
    ) -> tuple[GenerateResult | None, list[dict[str, Any]], Exception | None]:
        attempts_meta: list[dict[str, Any]] = []
        last_error: Exception | None = None

        for adapter_name, model_name in attempts:
            adapter = self._resolve_adapter(adapter_name)
            try:
                res = adapter.generate(
                    model=model_name,
                    prompt=prompt,
                    timeout_seconds=self.timeout_seconds,
                )
                attempts_meta.append(
                    {
                        "adapter": adapter_name,
                        "model": model_name,
                        "status": "succeeded",
                        "error": None,
                    }
                )
                return (res, attempts_meta, None)
            except Exception as e:
                last_error = e
                attempts_meta.append(
                    {
                        "adapter": adapter_name,
                        "model": model_name,
                        "status": "failed",
                        "error": str(e),
                    }
                )

        return (None, attempts_meta, last_error)

    def _resolve_adapter(self, adapter_name: str) -> Adapter:
        default_name = getattr(self.adapter, "name", "")
        if adapter_name == default_name:
            return self.adapter
        return build_adapter(adapter_name=adapter_name, runtime=self.runtime_config)

    def _materialize_input(self, ctx: dict[str, Any]) -> str:
        """Materialize the prompt input from template or messages."""
        if self.defn.template:
            return _render(self.defn.template, ctx)

        if self.defn.messages:
            # Format messages into a prompt string
            # For more sophisticated handling, this would be adapter-specific
            lines = []
            for msg in self.defn.messages:
                content = _render(msg.content, ctx)
                lines.append(f"{msg.role}: {content}")
            return "\n\n".join(lines)

        return ""

    def _decode_output(self, text: str) -> Any:
        """Decode the model output based on prompt_type."""
        if not text:
            return None

        text = text.strip()

        if self.defn.prompt_type == "text":
            return text

        if self.defn.prompt_type == "boolean":
            lower = text.lower()
            if lower in ("true", "yes", "1", "y"):
                return True
            if lower in ("false", "no", "0", "n"):
                return False
            return None

        if self.defn.prompt_type == "number":
            try:
                if "." in text:
                    return float(text)
                return int(text)
            except ValueError:
                return None

        if self.defn.prompt_type in ("json", "object", "array"):
            # Try to extract JSON from the response
            return self._parse_json(text)

        # tool type - return as-is for now
        return text

    def _parse_json(self, text: str) -> Any:
        """Parse JSON from text, handling common model output patterns."""
        # Try direct parse first
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            pass

        # Try to extract JSON from markdown code blocks
        import re

        # Match ```json ... ``` or ``` ... ```
        patterns = [
            r"```json\s*([\s\S]*?)\s*```",
            r"```\s*([\s\S]*?)\s*```",
        ]

        for pattern in patterns:
            match = re.search(pattern, text)
            if match:
                try:
                    return json.loads(match.group(1))
                except json.JSONDecodeError:
                    continue

        # Try to find JSON object or array in text
        for start, end in [("{", "}"), ("[", "]")]:
            idx_start = text.find(start)
            idx_end = text.rfind(end)
            if idx_start != -1 and idx_end > idx_start:
                try:
                    return json.loads(text[idx_start : idx_end + 1])
                except json.JSONDecodeError:
                    continue

        return None

    def _validate_schema(self, value: Any) -> None:
        """Validate value against JSON schema if provided."""
        if not self.defn.schema:
            return
        if value is None:
            raise ValueError(
                "JSON schema validation failed: model returned None (JSON could not be parsed). "
                "Check the model's response format."
            )

        try:
            import jsonschema  # type: ignore[import-untyped]

            jsonschema.validate(value, self.defn.schema)
        except ImportError:
            # jsonschema not installed, skip validation
            pass
        except jsonschema.ValidationError as e:
            raise ValueError(f"Schema validation failed: {e.message}")

