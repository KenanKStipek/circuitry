from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Protocol

if TYPE_CHECKING:
    from ..preflight import CheckResult


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
