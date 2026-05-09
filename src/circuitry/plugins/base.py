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

    return diagnostics
