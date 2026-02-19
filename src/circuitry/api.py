from __future__ import annotations

from pathlib import Path
from typing import Any

from .cli.config import CircuitryConfig
from .cli.runtime_shim import (
    RunRequest,
    RunResult,
)
from .cli.runtime_shim import (
    inspect_orchestration as _inspect_orchestration,
)
from .cli.runtime_shim import (
    run as _run,
)
from .cli.runtime_shim import (
    validate as _validate,
)
from .core.diagnostics import find_divergence_paths as _find_divergence_paths


class CircuitryExecutionError(RuntimeError):
    """Raised when embedded orchestration execution fails."""

    def __init__(self, message: str, *, result: RunResult):
        super().__init__(message)
        self.result = result


def run_orchestration(
    *,
    orchestration_path: str | Path,
    state: dict[str, Any] | None = None,
    state_path: str | Path | None = None,
    out_path: str | Path | None = None,
    dry_run: bool = False,
    validate_only: bool = False,
    verbose: bool = False,
    config: CircuitryConfig | None = None,
    raise_on_error: bool = True,
) -> RunResult:
    """
    Execute an orchestration from embedded Python.

    This function intentionally reuses the CLI runtime path so behavior remains
    equivalent across interfaces.
    """
    if state is not None and state_path is not None:
        raise ValueError("Provide either 'state' or 'state_path', not both.")

    req = RunRequest(
        orchestration_path=Path(orchestration_path),
        state_path=Path(state_path) if state_path is not None else None,
        initial_state=state,
        out_path=Path(out_path) if out_path is not None else None,
        dry_run=dry_run,
        validate_only=validate_only,
        verbose=verbose,
        config=config,
    )
    result = _run(req)

    if not result.ok and raise_on_error:
        raise CircuitryExecutionError(
            result.error or "Embedded orchestration execution failed.",
            result=result,
        )

    return result


def validate_orchestration(*, orchestration_path: str | Path) -> dict[str, Any]:
    """Validate orchestration structure using compiler-backed validation."""
    return _validate(Path(orchestration_path))


def inspect_orchestration(*, orchestration_path: str | Path) -> dict[str, Any]:
    """Inspect orchestration metadata (format, model/adapter, effect names/counts)."""
    return _inspect_orchestration(Path(orchestration_path))


def inspect_divergence_paths(
    *,
    state: dict[str, Any],
    root_path: str | None = "prime",
) -> list[dict[str, Any]]:
    """Return deterministic failure-path records discovered from runtime state."""
    return _find_divergence_paths(state, root_path=root_path)
