from __future__ import annotations

from pathlib import Path
from typing import Any

from .adapters import Adapter
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
from .cli.shared_library import (
    apply_service_profile as _apply_service_profile,
)
from .cli.shared_library import (
    fetch_shared_orchestration as _fetch_shared_orchestration,
)
from .cli.shared_library import (
    resolve_service_profile as _resolve_service_profile,
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
    live_state_path: str | Path | None = None,
    adapter: Adapter | None = None,
) -> RunResult:
    """
    Execute an orchestration from embedded Python.

    This function intentionally reuses the CLI runtime path so behavior remains
    equivalent across interfaces.

    Set *live_state_path* to enable atomic incremental state file writes
    after each effect completes, suitable for external tools (e.g. Perceptron).

    Pass *adapter* to run against an already-constructed adapter instead of the
    one the config resolves — the seam a host uses to drive an orchestration
    over its own model transport, and the one tests use to script one.
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
        live_state_path=Path(live_state_path) if live_state_path is not None else None,
        adapter=adapter,
    )
    result = _run(req)

    if not result.ok and raise_on_error:
        raise CircuitryExecutionError(
            result.error or "Embedded orchestration execution failed.",
            result=result,
        )

    return result


def run_shared_orchestration(
    *,
    asset_id: str,
    config: CircuitryConfig,
    version: str | None = None,
    auth_token: str | None = None,
    service_profile: str | None = None,
    state: dict[str, Any] | None = None,
    state_path: str | Path | None = None,
    out_path: str | Path | None = None,
    dry_run: bool = False,
    validate_only: bool = False,
    verbose: bool = False,
    raise_on_error: bool = True,
    live_state_path: str | Path | None = None,
) -> RunResult:
    """Fetch and run a shared-library orchestration using embedded API."""
    if state is not None and state_path is not None:
        raise ValueError("Provide either 'state' or 'state_path', not both.")

    profile = _resolve_service_profile(cfg=config, profile_name=service_profile)
    effective_config = _apply_service_profile(cfg=config, profile=profile)
    asset = _fetch_shared_orchestration(
        cfg=effective_config,
        asset_id=asset_id,
        version=version,
        auth_token=auth_token,
    )
    if profile is not None:
        asset.metadata["service_profile"] = profile.name

    req = RunRequest(
        orchestration_path=asset.file_path,
        state_path=Path(state_path) if state_path is not None else None,
        initial_state=state,
        out_path=Path(out_path) if out_path is not None else None,
        dry_run=dry_run,
        validate_only=validate_only,
        shared_library_metadata=asset.metadata,
        verbose=verbose,
        config=effective_config,
        live_state_path=Path(live_state_path) if live_state_path is not None else None,
    )
    result = _run(req)

    if not result.ok and raise_on_error:
        raise CircuitryExecutionError(
            result.error or "Embedded shared-orchestration execution failed.",
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
