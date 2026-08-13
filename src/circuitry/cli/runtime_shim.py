from __future__ import annotations

import json
import logging
from copy import deepcopy
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Optional
from uuid import uuid4

from ..adapters import Adapter, build_adapter
from ..core.compiler import apply_effect_overrides, compile_orchestration
from ..core.dynamic import DynamicRuntime
from ..core.runtime_plugins import (
    PLUGIN_CONTRACT_VERSION,
    PluginContext,
    RuntimePlugin,
    invoke_plugins,
    load_plugins,
)
from ..core.store import Store, build_persistence_backend
from ..plugins.factory import build_plugin
from ..preflight import CheckResult, call_check
from .allowlist import check_allowlist, walk_orchestration_refs
from .config import CircuitryConfig
from .effective_settings import EffectiveSettings, resolve_effective_settings
from .orchestration_loader import ORCHESTRATION_SUFFIXES, load_orchestration_file
from .profiles import ProfileSettings, load_profile
from .redaction import redact

logger = logging.getLogger(__name__)

try:
    import jsonschema as _jsonschema
except ImportError:
    _jsonschema = None  # type: ignore[assignment]

_SCHEMA_PATH = Path(__file__).parent.parent / "schema" / "orchestration.schema.json"


def _load_schema() -> dict[str, Any] | None:
    if _jsonschema is None:
        return None
    try:
        return json.loads(_SCHEMA_PATH.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None


def load_schema() -> dict[str, Any] | None:
    """The orchestration JSON schema, or ``None`` when it cannot be used.

    Public entry point for callers that validate against the same schema
    ``validate`` does (the TUI's Validate view) without reaching for a private.
    """
    return _load_schema()


@dataclass(frozen=True)
class RunRequest:
    orchestration_path: Path
    state_path: Optional[Path]
    out_path: Optional[Path]
    dry_run: bool
    validate_only: bool
    initial_state: dict[str, Any] | None = None
    shared_library_metadata: dict[str, Any] | None = None
    verbose: bool = False
    config: CircuitryConfig | None = None
    live_state_path: Optional[Path] = None
    adapter: Optional[Adapter] = None
    state_observer: Optional[Callable[[dict[str, Any]], None]] = None
    skip_preflight: bool = False
    # Caller-level overrides, ranked above the orchestration's own
    # ``adapter``/``model`` (the ``cli`` tier of resolve_effective_settings).
    # ``adapter_override`` is ignored when ``adapter`` supplies an instance —
    # that already pins the transport.
    adapter_override: Optional[str] = None
    model_override: Optional[str] = None
    profile_name: Optional[str] = None


@dataclass(frozen=True)
class RunResult:
    ok: bool
    state: dict[str, Any]
    warnings: list[str]
    error: Optional[str] = None


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _load_state(
    path: Optional[Path], initial_state: dict[str, Any] | None = None
) -> dict[str, Any]:
    if initial_state is not None:
        # Isolate runtime mutations from caller-owned dictionaries.
        return deepcopy(initial_state)
    if not path or not path.exists():
        return {}
    return json.loads(path.read_text(encoding="utf-8"))


def run(req: RunRequest) -> RunResult:
    state = _load_state(req.state_path, req.initial_state)
    warnings: list[str] = []
    plugins: list[RuntimePlugin] = []
    run_id: str | None = None
    runtime_config: dict[str, Any] = {}

    try:
        cfg = req.config or CircuitryConfig()
        orch = load_orchestration_file(req.orchestration_path)

        allowlist_errors = check_allowlist(orch=orch, config=cfg)
        if allowlist_errors:
            raise ValueError(
                "Allowlist enforcement failed: " + "; ".join(allowlist_errors)
            )

        profile: ProfileSettings | None = None
        if req.profile_name:
            profile = load_profile(
                name=req.profile_name,
                orchestration_path=req.orchestration_path,
                orch=orch,
            )
            if profile.inputs:
                # Profile inputs are a lower-priority base layer under
                # whatever the caller already resolved from --state/-e (CLI
                # values win — see cli.app.run_cmd).
                merged = dict(profile.inputs)
                merged.update(state)
                state = merged

        effective = resolve_effective_settings(
            cfg=cfg,
            orch=orch,
            cli_model=req.model_override,
            cli_adapter=req.adapter_override,
            profile=profile,
        )
        # One shared dict for the whole run: `use` effects append their library
        # pins to it as they resolve, at any nesting depth.
        runtime_config = effective.runtime if effective.runtime is not None else {}
        persistence = build_persistence_backend(effective.runtime)
        plugins, plugin_events = _initialize_plugins(
            effective.plugins, allowed=cfg.enabled_plugins
        )

        loaded_from_persistence = False
        if (
            persistence is not None
            and req.initial_state is None
            and req.state_path is None
        ):
            try:
                persisted = persistence.load_latest_state(
                    orchestration_path=str(req.orchestration_path)
                )
                if isinstance(persisted, dict):
                    hydrated = deepcopy(persisted)
                    if profile is not None and profile.inputs:
                        # Profile inputs stay the lowest layer: they fill
                        # keys the persisted snapshot doesn't carry rather
                        # than overwriting resumed values.
                        for key, value in profile.inputs.items():
                            hydrated.setdefault(key, value)
                    state = hydrated
                    loaded_from_persistence = True
            except Exception as e:
                state.setdefault("runtime", {})
                state["runtime"]["persistence"] = {
                    "enabled": True,
                    "status": "load_failed",
                    "error": str(e),
                }
                raise RuntimeError(f"Failed to load persisted state: {e}") from e

        state.setdefault("runtime", {})
        run_id = str(uuid4())
        state["runtime"]["last_run"] = {
            "run_id": run_id,
            "orchestration_path": str(req.orchestration_path),
            "dry_run": req.dry_run,
            "validate_only": req.validate_only,
            "verbose": req.verbose,
            "started_at": _now_iso(),
            "completed_at": None,
        }

        # Redact credential-bearing fields before embedding in state, since
        # state is serialized to --out, --json, --live-state, and last-run.json.
        # Live adapter calls keep using the un-redacted `effective.runtime`.
        state["runtime"]["effective_settings"] = {
            "model": effective.model,
            "adapter": effective.adapter,
            "plugins": effective.plugins,
            "runtime": redact(effective.runtime),
            "sources": effective.sources,
        }
        if profile is not None:
            state["runtime"]["effective_settings"]["profile"] = {
                "name": profile.name,
                "content": redact(profile.raw),
            }
        if req.shared_library_metadata is not None:
            state["runtime"]["shared_library"] = req.shared_library_metadata
        state["runtime"]["plugins"] = {
            "contract_version": PLUGIN_CONTRACT_VERSION,
            "configured": list(effective.plugins),
            "loaded": [getattr(p, "name", type(p).__name__) for p in plugins],
            "events": plugin_events,
        }
        if persistence is not None:
            state["runtime"]["persistence"] = {
                "enabled": True,
                "status": "ready",
                "error": None,
                "loaded_from_persistence": loaded_from_persistence,
                "persisted": False,
                "run_id": run_id,
                **persistence.describe(),
            }

        if req.validate_only:
            state["runtime"]["last_run"]["completed_at"] = _now_iso()
            return RunResult(ok=True, state=state, warnings=warnings)

        start_events = invoke_plugins(
            plugins=plugins,
            hook_name="on_run_start",
            state=state,
            context=PluginContext(
                run_id=run_id,
                orchestration_path=req.orchestration_path,
                dry_run=req.dry_run,
                validate_only=req.validate_only,
                runtime_config=effective.runtime,
            ),
        )
        state["runtime"]["plugins"]["events"].extend(start_events)

        # Compile YAML -> core definitions before adapter/model initialization so
        # structural orchestration errors are surfaced deterministically.
        root_def = compile_orchestration(orch=orch, root_name="prime")
        if profile is not None and profile.effects:
            effect_overrides = {
                path: {
                    k: v
                    for k, v in override.items()
                    if k in ("model", "provider", "enabled")
                }
                for path, override in profile.effects.items()
            }
            effect_overrides = {
                path: override for path, override in effect_overrides.items() if override
            }
            if effect_overrides:
                root_def, _ = apply_effect_overrides(root_def, effect_overrides)

        adapter: Adapter
        timeout_seconds = 120
        if _has_prompt_effects(root_def):
            if req.adapter is not None:
                # Caller supplied a fully-constructed adapter (e.g. circuitry-mcp
                # injecting a HostClaudeAdapter wired to per-prompt queues).
                # Skip factory dispatch but still resolve a model — the adapter
                # may pin its own, in which case effective.model can be empty.
                adapter = req.adapter
                resolved_adapter = req.adapter.name
                resolved_model = effective.model or ""
            else:
                resolved_adapter, resolved_model = _require_resolved_settings(
                    effective=effective, orchestration_path=req.orchestration_path
                )
                adapter = build_adapter(
                    adapter_name=resolved_adapter, runtime=effective.runtime or {}
                )
            try:
                adapters_cfg = (effective.runtime or {}).get("adapters") or {}
                this_cfg = adapters_cfg.get(resolved_adapter) or {}
                timeout_seconds = int(this_cfg.get("timeout_seconds") or 120)
            except (ValueError, TypeError) as exc:
                logger.warning("Failed to parse timeout_seconds, defaulting to 120: %s", exc)
                timeout_seconds = 120
        else:
            resolved_adapter = "_noop"
            resolved_model = effective.model or ""
            adapter = _NoOpAdapter()

        # Preflight (Story 1). After adapter resolution, before any LLM /
        # tool effect runs. ``--skip-preflight`` opts out for advanced use;
        # ``--dry-run`` also skips it since the whole point of dry-run is
        # to avoid touching the world. Only runs when a config was
        # supplied (programmatic callers without a config keep default-
        # open behavior). When the caller injected an adapter via
        # RunRequest.adapter we trust them — the adapter may not be
        # buildable from config (host_claude).
        if (
            not req.skip_preflight
            and not req.dry_run
            and req.config is not None
            and req.adapter is None
        ):
            preflight_results = preflight(req.orchestration_path, req.config)
            preflight_errors = format_preflight_errors(preflight_results)
            if preflight_errors:
                raise RuntimeError(
                    "Preflight failed: "
                    + "; ".join(preflight_errors)
                    + ". Re-run with --skip-preflight to bypass."
                )

        # Inject built-in template variables available in all orchestrations.
        state.setdefault("_run_id", run_id)
        state.setdefault("_timestamp", datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S"))

        # Execute using core runtime against Store
        callbacks: list[Callable[[dict[str, Any]], None]] = []
        if req.live_state_path is not None:
            from .live_state import make_live_state_callback

            callbacks.append(make_live_state_callback(req.live_state_path))
        if req.state_observer is not None:
            callbacks.append(req.state_observer)

        on_write: Optional[Callable[[dict[str, Any]], None]]
        if not callbacks:
            on_write = None
        elif len(callbacks) == 1:
            on_write = callbacks[0]
        else:
            def on_write(s: dict[str, Any]) -> None:
                for cb in callbacks:
                    cb(s)

        if on_write is not None:
            # Write initial snapshot so watchers see the pending state
            on_write(state)

        # Story 2: per-effect lifecycle hook. Fires after each effect's
        # node["value"] is finalized. Skipped silently for plugins that
        # don't implement on_effect_complete.
        effect_complete_cb: Optional[Callable[[str, dict[str, Any]], None]] = None
        if plugins:
            _plugin_ctx = PluginContext(
                run_id=run_id,
                orchestration_path=req.orchestration_path,
                dry_run=req.dry_run,
                validate_only=req.validate_only,
                runtime_config=effective.runtime,
            )

            def effect_complete_cb(  # type: ignore[no-redef]
                effect_path: str, effect_result: dict[str, Any]
            ) -> None:
                invoke_plugins(
                    plugins=plugins,
                    hook_name="on_effect_complete",
                    state=state,
                    context=_plugin_ctx,
                    effect_path=effect_path,
                    effect_result=effect_result,
                )

        store = Store(state, on_write=on_write, effect_complete=effect_complete_cb)

        runtime = DynamicRuntime(
            root_def,
            adapter=adapter,
            model=resolved_model,
            runtime_config=runtime_config,
            dry_run=req.dry_run,
            timeout_seconds=timeout_seconds,
            verbose=req.verbose,
        )
        runtime.execute(store=store)

        _record_library_pins(state, runtime_config)
        state["runtime"]["last_run"]["completed_at"] = _now_iso()

        success_events = invoke_plugins(
            plugins=plugins,
            hook_name="on_run_success",
            state=state,
            context=PluginContext(
                run_id=run_id,
                orchestration_path=req.orchestration_path,
                dry_run=req.dry_run,
                validate_only=req.validate_only,
                runtime_config=effective.runtime,
            ),
        )
        state["runtime"]["plugins"]["events"].extend(success_events)

        if persistence is not None:
            try:
                persistence.save_run_snapshot(
                    orchestration_path=str(req.orchestration_path),
                    run_id=run_id,
                    ok=True,
                    error=None,
                    state=state,
                )
                state["runtime"]["persistence"]["status"] = "persisted"
                state["runtime"]["persistence"]["persisted"] = True
            except Exception as e:
                state["runtime"]["persistence"]["status"] = "save_failed"
                state["runtime"]["persistence"]["error"] = str(e)
                raise RuntimeError(f"Failed to persist runtime state: {e}") from e

        return RunResult(ok=True, state=state, warnings=warnings)

    except Exception as e:
        try:
            # Pins resolved before the failure still describe what this run
            # reached for — keep them for the post-mortem.
            _record_library_pins(state, runtime_config)
            if run_id is None:
                run_id = str(uuid4())
            plugins_meta = state.setdefault("runtime", {}).setdefault("plugins", {})
            if isinstance(plugins_meta, dict):
                configured = plugins_meta.get("configured")
                if not isinstance(configured, list):
                    plugins_meta["configured"] = []
                loaded = plugins_meta.get("loaded")
                if not isinstance(loaded, list):
                    plugins_meta["loaded"] = []
                events = plugins_meta.get("events")
                if not isinstance(events, list):
                    plugins_meta["events"] = []
                failure_events = invoke_plugins(
                    plugins=plugins,
                    hook_name="on_run_failure",
                    state=state,
                    context=PluginContext(
                        run_id=run_id,
                        orchestration_path=req.orchestration_path,
                        dry_run=req.dry_run,
                        validate_only=req.validate_only,
                        runtime_config=runtime_config,
                    ),
                    error=str(e),
                )
                plugins_meta["events"].extend(failure_events)
            state.setdefault("runtime", {}).setdefault("last_run", {})[
                "completed_at"
            ] = _now_iso()
            persistence_node = state.setdefault("runtime", {}).get("persistence")
            if isinstance(persistence_node, dict):
                if not persistence_node.get("status"):
                    persistence_node["status"] = "failed"
                persistence_node["error"] = str(e)
        except Exception:
            logger.error("Error during error-handling cleanup", exc_info=True)
        return RunResult(ok=False, state=state, warnings=warnings, error=str(e))


def _record_library_pins(
    state: dict[str, Any], runtime_config: dict[str, Any] | None
) -> None:
    """Copy the run's `use ref:` pins into `runtime.library_refs`.

    Each pin carries source, ref, resolved path, and — for SHA-pinned remote
    sources — the commit and cache directory it was served from, which is what
    a later offline re-run reproduces.
    """
    from ..core.library_ref import STATE_KEY, collect_pins

    pins = collect_pins(runtime_config)
    if not pins:
        return
    state.setdefault("runtime", {})[STATE_KEY] = pins


def validate(
    orchestration_path: Path,
    *,
    config: CircuitryConfig | None = None,
    skip_preflight: bool = False,
) -> dict[str, Any]:
    text = orchestration_path.read_text(encoding="utf-8").strip()
    if not text:
        return {"ok": False, "errors": ["Orchestration file is empty."]}

    try:
        orch = load_orchestration_file(orchestration_path)

        schema = _load_schema()
        if schema is not None:
            validator = _jsonschema.Draft7Validator(schema)
            schema_errors = sorted(validator.iter_errors(orch), key=str)
            if schema_errors:
                return {"ok": False, "errors": [e.message for e in schema_errors]}

        # Allowlist gate. Skipped when caller supplies no config — keeps
        # programmatic callers (tests, MCP server, internal scripts)
        # default-open. The CLI resolves a config from disk + env vars and
        # passes it explicitly so AC 0.2 (env-var enforcement) holds.
        if config is not None:
            allowlist_errors = check_allowlist(orch=orch, config=config)
            if allowlist_errors:
                return {"ok": False, "errors": allowlist_errors}

        compile_orchestration(orch=orch, root_name="prime")

        from ..core.cycle_check import detect_cycles
        cycle = detect_cycles(
            orch,
            root_path=orchestration_path,
            runtime=(config.runtime if config is not None else None),
        )
        if cycle is not None:
            return {
                "ok": False,
                "errors": [f"Cycle: {' → '.join(cycle)}"],
            }

        # Preflight gate (Story 1). Same gating policy as allowlist — only
        # runs when the caller supplied a config so programmatic callers
        # without an environment-resolved config don't hit network probes.
        # ``skip_preflight`` lets offline / structure-only contexts (CI
        # smoke tests, ``cof check --skip-preflight``) bypass it.
        if config is not None and not skip_preflight:
            preflight_results = preflight(orchestration_path, config)
            preflight_errors = format_preflight_errors(preflight_results)
            if preflight_errors:
                return {"ok": False, "errors": preflight_errors}

        return {"ok": True, "errors": []}
    except Exception as e:
        return {"ok": False, "errors": [str(e)]}


def preflight(
    orchestration_path: Path,
    config: CircuitryConfig,
) -> list[tuple[str, CheckResult]]:
    """Walk an orchestration's referenced extensions and call ``check()`` on
    each. Returns an ordered list of ``(label, CheckResult)`` tuples.

    Labels are prefixed by category for actionable error messages:
      * ``adapter:<name>``
      * ``tool:<name>``
      * ``runtime_plugin:<name>``
      * ``library_ref:<ref>``

    Adapters that can only be built at runtime (host_claude needs a
    request_handler) are reported as ok with a deferred message; preflight
    cannot exercise them outside the MCP context.
    """
    orch = load_orchestration_file(orchestration_path)
    adapter_refs, tool_refs = walk_orchestration_refs(orch)
    runtime_cfg = config.runtime or {}
    results: list[tuple[str, CheckResult]] = []

    # `use ref:` entries served by a source that was never refreshed fail here,
    # before any effect runs, carrying the `cof library refresh` command that
    # fixes them. Resolvable-but-missing refs are left to the run's own error.
    from ..core.library_ref import check_use_refs

    for ref, message in check_use_refs(
        orch, root_path=orchestration_path, runtime=runtime_cfg
    ):
        results.append(
            (f"library_ref:{ref}", CheckResult(ok=False, missing=[], message=message))
        )

    for adapter_name in sorted(adapter_refs):
        try:
            adapter = build_adapter(adapter_name=adapter_name, runtime=runtime_cfg)
        except RuntimeError as exc:
            # Adapters that require runtime injection (e.g. host_claude) raise
            # RuntimeError at factory time. Treat as deferred — preflight
            # cannot exercise them, but they are not a failure here.
            results.append(
                (
                    f"adapter:{adapter_name}",
                    CheckResult(
                        ok=True,
                        missing=[],
                        message=f"deferred (runtime-injected): {exc}",
                    ),
                )
            )
            continue
        except ValueError as exc:
            results.append(
                (
                    f"adapter:{adapter_name}",
                    CheckResult(ok=False, missing=[], message=str(exc)),
                )
            )
            continue
        results.append((f"adapter:{adapter_name}", call_check(adapter)))

    for tool_name in sorted(tool_refs):
        try:
            tool = build_plugin(plugin_name=tool_name, runtime=runtime_cfg)
        except (RuntimeError, ValueError) as exc:
            results.append(
                (
                    f"tool:{tool_name}",
                    CheckResult(ok=False, missing=[], message=str(exc)),
                )
            )
            continue
        results.append((f"tool:{tool_name}", call_check(tool)))

    if config.plugins:
        # Plugin load failures are non-fatal warnings — surfaced via the
        # ``runtime.plugins.events`` log, not preflight. Preflight only
        # exercises ``check()`` on plugins that loaded cleanly.
        for load_result in load_plugins(
            list(config.plugins), allowed=config.enabled_plugins
        ):
            if load_result.plugin is None:
                continue
            name = getattr(load_result.plugin, "name", load_result.plugin_id)
            results.append(
                (f"runtime_plugin:{name}", call_check(load_result.plugin))
            )

    return results


def format_preflight_errors(
    results: list[tuple[str, CheckResult]],
) -> list[str]:
    """Render preflight failures as one-line strings for CLI / RunResult."""
    errors: list[str] = []
    for label, r in results:
        if r.ok:
            continue
        parts = [f"{label}: not ready"]
        if r.missing:
            parts.append(f"missing {r.missing}")
        if r.message:
            parts.append(r.message)
        errors.append(" — ".join(parts))
    return errors


def inspect_orchestration(orchestration_path: Path) -> dict[str, Any]:
    suffix = orchestration_path.suffix.lower()
    summary: dict[str, Any] = {
        "path": str(orchestration_path),
        "format": suffix.lstrip(".") or "unknown",
        "size_bytes": orchestration_path.stat().st_size,
    }

    if suffix in ORCHESTRATION_SUFFIXES:
        data = load_orchestration_file(orchestration_path)
        effects = data.get("effects") or data.get("steps") or []
        effect_names = [
            s.get("name") for s in effects if isinstance(s, dict) and s.get("name")
        ]
        summary["model"] = data.get("model")
        summary["adapter"] = data.get("adapter")
        summary["effects_count"] = len(effects) if isinstance(effects, list) else 0
        summary["effect_names"] = effect_names
        # Backward-compatible aliases.
        summary["steps_count"] = summary["effects_count"]
        summary["step_names"] = effect_names
    else:
        summary["note"] = f"Unsupported format for deep inspection: {suffix}"

    return summary


@dataclass(frozen=True)
class _NoOpAdapter:
    """Stub adapter for tool-only orchestrations that have no prompt effects."""

    name: str = "_noop"

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> Any:
        raise RuntimeError(
            "Attempted to call generate() on a tool-only orchestration. "
            "No prompt effects should be present."
        )

    def check(self) -> CheckResult:
        # Tool-only orchestrations don't exercise an LLM, so the noop
        # adapter's preflight is trivially ready.
        return CheckResult(
            ok=True,
            missing=[],
            message="no-op adapter; no LLM transport.",
        )


def _has_prompt_effects(defn: Any) -> bool:
    """Recursively check if any effect in the tree requires an LLM adapter."""
    from ..core.conditional import ConditionalDefinition
    from ..core.disabled import is_enabled
    from ..core.dynamic import DynamicDefinition
    from ..core.loop import LoopDefinition
    from ..core.prompt import PromptDefinition
    from ..core.reflector import ReflectorDefinition

    # A disabled effect never runs, so it never needs an adapter — a profile
    # that switches every prompt off makes the run adapter-free.
    if not is_enabled(defn):
        return False

    if isinstance(defn, PromptDefinition):
        return True
    if isinstance(defn, DynamicDefinition):
        return any(_has_prompt_effects(e) for e in defn.effects)
    if isinstance(defn, ConditionalDefinition):
        return any(_has_prompt_effects(e) for e in defn.then_effects) or any(
            _has_prompt_effects(e) for e in defn.else_effects
        )
    if isinstance(defn, LoopDefinition):
        return any(_has_prompt_effects(e) for e in defn.body)
    if isinstance(defn, ReflectorDefinition):
        return _has_prompt_effects(defn.inner)

    from ..core.use import UseDefinition
    if isinstance(defn, UseDefinition):
        return True  # conservatively assume child orchestration has prompts

    return False


def _require_resolved_settings(
    *, effective: EffectiveSettings, orchestration_path: Path
) -> tuple[str, str]:
    if not effective.adapter:
        raise ValueError(
            "No adapter resolved for orchestration "
            f"{orchestration_path}. Set 'adapter' in the orchestration or "
            "'default_adapter' in config.json. "
            f"(adapter source: {effective.sources.get('adapter')})"
        )
    if not effective.model:
        raise ValueError(
            "No model resolved for orchestration "
            f"{orchestration_path}. Set 'model' in the orchestration or "
            "'default_model' in config.json. "
            f"(model source: {effective.sources.get('model')})"
        )
    return (effective.adapter, effective.model)


def _initialize_plugins(
    plugin_ids: list[str],
    *,
    allowed: list[str] | None = None,
) -> tuple[list[RuntimePlugin], list[dict[str, Any]]]:
    loaded_plugins: list[RuntimePlugin] = []
    events: list[dict[str, Any]] = []

    for result in load_plugins(plugin_ids, allowed=allowed):
        if result.plugin is not None:
            loaded_plugins.append(result.plugin)
            events.append(
                {
                    "plugin": result.plugin_id,
                    "hook": "load",
                    "ok": True,
                    "error": None,
                }
            )
            continue
        events.append(
            {
                "plugin": result.plugin_id,
                "hook": "load",
                "ok": False,
                "error": result.error,
            }
        )

    return (loaded_plugins, events)
