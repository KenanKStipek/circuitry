from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

from ..adapters import build_adapter
from ..core.compiler import compile_orchestration
from ..core.dynamic import DynamicRuntime
from ..core.store import Store
from .config import CircuitryConfig
from .effective_settings import resolve_effective_settings
from .orchestration_loader import load_orchestration_file


@dataclass(frozen=True)
class RunRequest:
    orchestration_path: Path
    state_path: Optional[Path]
    out_path: Optional[Path]
    dry_run: bool
    validate_only: bool
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


def _load_state(path: Optional[Path]) -> dict[str, Any]:
    if not path or not path.exists():
        return {}
    import json

    return json.loads(path.read_text(encoding="utf-8"))


def run(req: RunRequest) -> RunResult:
    state = _load_state(req.state_path)
    warnings: list[str] = []

    try:
        cfg = req.config or CircuitryConfig()
        orch = load_orchestration_file(req.orchestration_path)

        effective = resolve_effective_settings(cfg=cfg, orch=orch)

        state.setdefault("runtime", {})
        state["runtime"]["last_run"] = {
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

        if req.validate_only:
            state["runtime"]["last_run"]["completed_at"] = _now_iso()
            return RunResult(ok=True, state=state, warnings=warnings)

        if not effective.adapter:
            raise ValueError(
                "No adapter resolved (set default_adapter or adapter in orchestration)."
            )
        if not effective.model:
            raise ValueError(
                "No model resolved (set default_model or model in orchestration)."
            )

        adapter = build_adapter(
            adapter_name=effective.adapter, runtime=effective.runtime or {}
        )

        # Compile YAML -> core monads
        root_def = compile_orchestration(orch=orch, root_name="prime")

        # Execute using core runtime against Store
        store = Store(state)

        # Pick a timeout for v0: prefer runtime.adapters.<name>.timeout_seconds, else 120
        timeout_seconds = 120
        try:
            adapters_cfg = (effective.runtime or {}).get("adapters") or {}
            this_cfg = adapters_cfg.get(effective.adapter) or {}
            timeout_seconds = int(this_cfg.get("timeout_seconds") or 120)
        except Exception:
            timeout_seconds = 120

        runtime = DynamicRuntime(
            root_def,
            adapter=adapter,
            model=effective.model,
            dry_run=req.dry_run,
            timeout_seconds=timeout_seconds,
        )
        runtime.execute(store=store)

        state["runtime"]["last_run"]["completed_at"] = _now_iso()
        return RunResult(ok=True, state=state, warnings=warnings)

    except Exception as e:
        try:
            state.setdefault("runtime", {}).setdefault("last_run", {})[
                "completed_at"
            ] = _now_iso()
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
        steps = data.get("steps") or []
        summary["model"] = data.get("model")
        summary["adapter"] = data.get("adapter")
        summary["steps_count"] = len(steps) if isinstance(steps, list) else 0
        summary["step_names"] = [
            s.get("name") for s in steps if isinstance(s, dict) and s.get("name")
        ]
    else:
        summary["note"] = "Non-YAML inspection is currently shallow."

    return summary
