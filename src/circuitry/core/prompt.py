from __future__ import annotations

import json
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any, Literal, Optional, Sequence

from .store import Store
from ..adapters import Adapter


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _render(template: str, ctx: dict[str, Any]) -> str:
    try:
        import chevron  # type: ignore

        return chevron.render(template, ctx)
    except Exception:
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
        dry_run: bool = False,
        timeout_seconds: int = 120,
    ):
        self.defn = definition
        self.adapter = adapter
        self.model = model
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds

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

        if self.dry_run:
            node["value"] = None
            meta["completed_at"] = _now_iso()
            return

        try:
            res = self.adapter.generate(
                model=self.model,
                prompt=prompt_sent,
                timeout_seconds=self.timeout_seconds,
            )

            # Decode and validate output based on prompt_type
            decoded_value = self._decode_output(res.text)

            # Validate against schema if provided
            if self.defn.schema and self.defn.prompt_type in (
                "json",
                "object",
                "array",
            ):
                self._validate_schema(decoded_value)

            node["value"] = decoded_value
            meta["tokens_sent"] = res.tokens_sent
            meta["tokens_received"] = res.tokens_received
            meta["completed_at"] = _now_iso()

        except Exception as e:
            meta["error"] = str(e)
            meta["completed_at"] = _now_iso()
            if self.defn.on_error == "fail":
                raise
            elif self.defn.on_error == "skip":
                node["value"] = None
            # continue: keep going with None value

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
        if not self.defn.schema or value is None:
            return

        try:
            import jsonschema

            jsonschema.validate(value, self.defn.schema)
        except ImportError:
            # jsonschema not installed, skip validation
            pass
        except jsonschema.ValidationError as e:
            raise ValueError(f"Schema validation failed: {e.message}")
