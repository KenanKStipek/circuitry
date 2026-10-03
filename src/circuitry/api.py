from __future__ import annotations

from collections.abc import Iterable
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
from .core.saved_state import dumps_saved_state as _dumps_saved_state


def _coerce_capabilities(allow_capabilities: Iterable[str] | None) -> frozenset[str] | None:
    """``frozenset("shell")`` silently becomes ``{'s', 'h', 'e', 'l'}`` — a
    caller who passes a bare string instead of a list gets a confusing
    refusal rather than the capability they meant. Raise instead.
    """
    if allow_capabilities is None:
        return None
    if isinstance(allow_capabilities, str):
        raise TypeError(
            "allow_capabilities must be an iterable of capability names "
            f"(e.g. ['shell']), not a single string: {allow_capabilities!r}"
        )
    return frozenset(allow_capabilities) if allow_capabilities else None


def _write_out_path(result: RunResult, *, pretty: bool) -> None:
    """Write ``result.out_path`` (the CLI flag or a profile's ``out:``), the
    same as `cof run` writes `--out` — for both a successful and a failed
    run, since a failed run's state still carries runtime metadata. Embedded
    callers otherwise saw `out_path` silently do nothing, unlike the CLI
    (#265, found while rewriting the guidebook/#286).
    """
    if result.out_path is None:
        return
    result.out_path.parent.mkdir(parents=True, exist_ok=True)
    result.out_path.write_text(
        _dumps_saved_state(result.state, pretty=pretty) + "\n", encoding="utf-8"
    )


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
    trust_document: bool = True,
    allow_capabilities: Iterable[str] | None = None,
    pretty: bool = False,
) -> RunResult:
    """
    Execute an orchestration from embedded Python.

    This function intentionally reuses the CLI runtime path so behavior remains
    equivalent across interfaces.

    Set *live_state_path* to enable atomic incremental state file writes
    while the run goes (at most one per
    :data:`~circuitry.cli.live_state.LIVE_STATE_INTERVAL_SECONDS`, plus a final
    write when it ends), suitable for external tools (e.g. Perceptron).

    Pass *adapter* to run against an already-constructed adapter instead of the
    one the config resolves — the seam a host uses to drive an orchestration
    over its own model transport, and the one tests use to script one.

    The file at *orchestration_path* is trusted by default, as ``cof run
    ./file.yml`` trusts it: its whole ``runtime:`` block and ``plugins:`` list
    apply, and ``RunResult.warnings`` carries one notice naming any host
    settings among them (``runtime`` keys other than ``complexity`` and
    ``state``, ``plugins`` config does not list). Pass
    ``trust_document=False`` for a path you did not choose yourself (fetched,
    generated, or picked by a tool or network caller): the document may then
    only set ``runtime.complexity`` and ``runtime.state``, and anything else
    is ignored with a warning.

    *out_path* (the CLI flag, or a profile's own ``out:``) is written to disk
    the same way ``cof run --out`` writes it, for both a successful and a
    failed run — *pretty* matches ``--pretty``.

    *allow_capabilities* pre-approves capabilities (``"shell"``,
    ``"python_eval"``, ``"fs-write"``, ``"network"``) a document that is not
    the user's own may need — a ``use: ref:`` child, reached regardless of
    *trust_document* (#275, #334). Embedding code is the host, so this is the
    SDK's own consent, never persisted to ``cof trust``'s store and never
    read from the document or its input; it approves this one call, the same
    as ``cof run --allow-capabilities shell,network`` approves one invocation.
    Without it, a covered document refuses unless its digest was already
    consented via `cof trust <path>`; nothing here ever prompts.
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
        trust_document=trust_document,
        allow_capabilities=_coerce_capabilities(allow_capabilities),
    )
    result = _run(req)
    _write_out_path(result, pretty=pretty)

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
    allow_capabilities: Iterable[str] | None = None,
    pretty: bool = False,
) -> RunResult:
    """Fetch and run a shared-library orchestration using embedded API.

    A fetched document is someone else's, so it stays limited: it may only set
    ``runtime.complexity`` and ``runtime.state`` (unless config sets
    ``trust_orchestration_runtime``). It is also, unconditionally, a document
    this call did not come from the user's own disk — every run goes through
    the capability consent gate (#275, #334): refused unless its digest was
    already consented via ``cof trust``, or pre-approved here with
    *allow_capabilities* (``"shell"``, ``"python_eval"``, ``"fs-write"``,
    ``"network"``) — the embedding host's own consent for this one call,
    never persisted and never read from the asset or its input.

    *out_path* is written to disk the same way ``cof run --out`` writes it,
    for both a successful and a failed run — *pretty* matches ``--pretty``.
    """
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
        allow_capabilities=_coerce_capabilities(allow_capabilities),
    )
    result = _run(req)
    _write_out_path(result, pretty=pretty)

    if not result.ok and raise_on_error:
        raise CircuitryExecutionError(
            result.error or "Embedded shared-orchestration execution failed.",
            result=result,
        )
    return result


def validate_orchestration(
    *,
    orchestration_path: str | Path,
    trust_document: bool = True,
    config: CircuitryConfig | None = None,
) -> dict[str, Any]:
    """Validate orchestration structure using compiler-backed validation.

    *trust_document* matches :func:`run_orchestration`: by default the report's
    ``warnings`` carry the notice naming the host settings the file would
    apply; with ``False`` they name the ones a run would ignore.

    Pass the same *config* the corresponding :func:`run_orchestration` call
    would use so "valid" reliably predicts "runnable": without one, the
    allowlist and preflight gates are skipped here exactly as they are in
    ``run_orchestration(config=None)``, so a document can validate ok and
    then fail preflight at run time (#265 part 4).
    """
    return _validate(
        Path(orchestration_path), config=config, trust_document=trust_document
    )


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
