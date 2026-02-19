from __future__ import annotations

from copy import deepcopy
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional
from uuid import uuid4

from ..adapters import build_adapter
from ..core.compiler import compile_orchestration
from ..core.dynamic import DynamicRuntime
from ..core.plugins import (
    PLUGIN_CONTRACT_VERSION,
    PluginContext,
    RuntimePlugin,
    invoke_plugins,
    load_plugins,
)
from ..core.store import Store, build_persistence_backend
from .config import CircuitryConfig
from .effective_settings import EffectiveSettings, resolve_effective_settings
from .orchestration_loader import load_orchestration_file


@dataclass(frozen=True)
class RunRequest:
    orchestration_path: Path
    state_path: Optional[Path]
    out_path: Optional[Path]
    dry_run: bool
    validate_only: bool
    initial_state: dict[str, Any] | None = None
    verbose: bool = False
    config: CircuitryConfig | None = None


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
    import json

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

        effective = resolve_effective_settings(cfg=cfg, orch=orch)
        runtime_config = effective.runtime
        persistence = build_persistence_backend(effective.runtime)
        plugins, plugin_events = _initialize_plugins(effective.plugins)

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
                    state = deepcopy(persisted)
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

        state["runtime"]["effective_settings"] = {
            "model": effective.model,
            "adapter": effective.adapter,
            "plugins": effective.plugins,
            "runtime": effective.runtime,
            "sources": effective.sources,
        }
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

        resolved_adapter, resolved_model = _require_resolved_settings(
            effective=effective, orchestration_path=req.orchestration_path
        )

        adapter = build_adapter(
            adapter_name=resolved_adapter, runtime=effective.runtime or {}
        )

        # Execute using core runtime against Store
        store = Store(state)

        # Pick a timeout for v0: prefer runtime.adapters.<name>.timeout_seconds, else 120
        timeout_seconds = 120
        try:
            adapters_cfg = (effective.runtime or {}).get("adapters") or {}
            this_cfg = adapters_cfg.get(resolved_adapter) or {}
            timeout_seconds = int(this_cfg.get("timeout_seconds") or 120)
        except Exception:
            timeout_seconds = 120

        runtime = DynamicRuntime(
            root_def,
            adapter=adapter,
            model=resolved_model,
            runtime_config=effective.runtime or {},
            dry_run=req.dry_run,
            timeout_seconds=timeout_seconds,
        )
        runtime.execute(store=store)

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
            pass
        return RunResult(ok=False, state=state, warnings=warnings, error=str(e))


def validate(orchestration_path: Path) -> dict[str, Any]:
    text = orchestration_path.read_text(encoding="utf-8").strip()
    if not text:
        return {"ok": False, "errors": ["Orchestration file is empty."]}

    try:
        orch = load_orchestration_file(orchestration_path)
        compile_orchestration(orch=orch, root_name="prime")
        return {"ok": True, "errors": []}
    except Exception as e:
        return {"ok": False, "errors": [str(e)]}


def inspect_orchestration(orchestration_path: Path) -> dict[str, Any]:
    suffix = orchestration_path.suffix.lower()
    summary: dict[str, Any] = {
        "path": str(orchestration_path),
        "format": suffix.lstrip(".") or "unknown",
        "size_bytes": orchestration_path.stat().st_size,
    }

    if suffix in {".yml", ".yaml"}:
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
        summary["note"] = "Non-YAML inspection is currently shallow."

    return summary


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
) -> tuple[list[RuntimePlugin], list[dict[str, Any]]]:
    loaded_plugins: list[RuntimePlugin] = []
    events: list[dict[str, Any]] = []

    for result in load_plugins(plugin_ids):
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
