from __future__ import annotations

import json
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Protocol

from ..cli.redaction import redact

if TYPE_CHECKING:
    from ..preflight import CheckResult

#: Cap on the response-body excerpt `http_error_excerpt` returns — enough
#: to carry a validation reason without turning a failure message into the
#: whole body (see `_RAW_META_MAX_BYTES` in `core/tool.py` for the same
#: concern on `meta.raw`).
_ERROR_EXCERPT_MAX_CHARS = 500


def http_error_excerpt(body: str, *, max_chars: int = _ERROR_EXCERPT_MAX_CHARS) -> str:
    """A bounded, redacted explanation of an HTTP error response body, for
    the http-family tool plugins (`http`, `web_fetch`, `webhook`, `linear`).

    Prefers the JSON `error`/`message`/`detail` field when the body parses
    as one (ComfyUI-shaped bodies nest it under `error.message`; most APIs
    use one of the three flat), falling back to the first `max_chars`
    characters of the raw body otherwise. Unlike `meta.raw` (redacted
    centrally by `core/tool.py`'s `_capped_raw`), a plugin's own `stderr`/
    error string is never redacted downstream, so this redacts the body
    itself, the same `cli/redaction.redact` deny-list `meta.raw` uses,
    before any of it reaches a failure message.
    """
    text = (body or "").strip()
    if not text:
        return ""
    try:
        parsed: Any = json.loads(text)
    except json.JSONDecodeError:
        redacted_text = redact(text)
        return redacted_text[:max_chars] if isinstance(redacted_text, str) else text[:max_chars]
    if isinstance(parsed, dict):
        redacted = redact(parsed)
        if isinstance(redacted, dict):
            error = redacted.get("error")
            if isinstance(error, dict):
                message = error.get("message")
                if isinstance(message, str) and message:
                    return message[:max_chars]
            if isinstance(error, str) and error:
                return error[:max_chars]
            for key in ("message", "detail"):
                value = redacted.get(key)
                if isinstance(value, str) and value:
                    return value[:max_chars]
        # No recognized reason field (e.g. GraphQL-style `errors: [...]` or a
        # validation body with its own shape): fall back to the whole body,
        # but the *redacted* copy, never the raw `text` — a sibling
        # credential-shaped field (e.g. an echoed `api_key`) must not reach
        # the failure message just because it wasn't under `error`/`message`/
        # `detail`. `redact(text)` on the raw string only matches when the
        # whole string is itself a JWT/key/URL, so it would let this through.
        return json.dumps(redacted, ensure_ascii=False)[:max_chars]
    if isinstance(parsed, list):
        redacted_list = redact(parsed)
        return json.dumps(redacted_list, ensure_ascii=False)[:max_chars]
    redacted_text = redact(text)
    return redacted_text[:max_chars] if isinstance(redacted_text, str) else text[:max_chars]


@dataclass(frozen=True)
class ToolResult:
    value: Any
    raw: dict[str, Any]
    stdout: str | None = None
    stderr: str | None = None
    exit_code: int | None = None
    #: Whether the tool call itself succeeded. ``False`` is the one signal
    #: ToolRuntime treats as a failure without an exception: it sets
    #: ``meta.error`` and applies the effect's ``on_error`` exactly as it
    #: would for a raised exception. Plugins that can detect a genuine
    #: failure without raising (e.g. an HTTP-family plugin surfacing a
    #: 4xx/5xx response instead of raising on it) set this; everything else
    #: defaults to ``True`` and keeps raising for real errors.
    ok: bool = True


def _as_bool(value: Any, *, default: bool = False) -> bool:
    """String-aware boolean coercion for a tool param.

    Mustache renders a templated ``false`` as the literal string
    ``"False"``, and Python's ``bool("False")`` is ``True`` — plain
    ``bool(params.get(...))`` silently inverts a templated-false boolean
    param. ``None`` (the key absent) returns *default*; an actual ``bool``
    passes through; a string is false when it (case-insensitively, after
    stripping whitespace) is one of ``"false"``, ``"0"``, ``"no"``,
    ``"off"``, ``"n"`` or empty, true otherwise; anything else falls back to
    Python's own ``bool()``.
    """
    if value is None:
        return default
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        return value.strip().lower() not in ("false", "0", "no", "off", "n", "")
    return bool(value)


class ToolPlugin(Protocol):
    @property
    def name(self) -> str: ...

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult: ...

    def check(self) -> CheckResult: ...


def validate_tool_result(result: ToolResult, *, plugin_name: str) -> list[str]:
    """
    Validate plugin execute() output against normalized contract.

    Returns a list of diagnostics. Empty list means the contract is satisfied.
    """
    diagnostics: list[str] = []

    if not isinstance(result.raw, dict):
        diagnostics.append(
            f"{plugin_name}: 'raw' must be dict, got {type(result.raw).__name__}"
        )

    if result.stdout is not None and not isinstance(result.stdout, str):
        diagnostics.append(
            f"{plugin_name}: 'stdout' must be str|None, got {type(result.stdout).__name__}"
        )

    if result.stderr is not None and not isinstance(result.stderr, str):
        diagnostics.append(
            f"{plugin_name}: 'stderr' must be str|None, got {type(result.stderr).__name__}"
        )

    if result.exit_code is not None:
        if not isinstance(result.exit_code, int):
            diagnostics.append(
                f"{plugin_name}: 'exit_code' must be int|None, "
                f"got {type(result.exit_code).__name__}"
            )
        elif result.exit_code < 0:
            diagnostics.append(
                f"{plugin_name}: 'exit_code' must be >= 0, got {result.exit_code}"
            )

    if not isinstance(result.ok, bool):
        diagnostics.append(
            f"{plugin_name}: 'ok' must be bool, got {type(result.ok).__name__}"
        )

    return diagnostics
