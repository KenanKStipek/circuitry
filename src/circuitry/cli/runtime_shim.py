from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

from .config import CircuitryConfig
from .effective_settings import resolve_effective_settings
from .orchestration_loader import load_orchestration_file
from ..adapters import build_adapter


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


def _render_template(template: str, ctx: dict[str, Any]) -> str:
    """
    Mustache render. Uses chevron if installed; otherwise passthrough.
    """
    try:
        import chevron  # type: ignore

        return chevron.render(template, ctx)
    except Exception:
        return template


def _state_to_ctx(state: dict[str, Any]) -> dict[str, Any]:
    """
    v0: expose full state as render context.
    Later: add more deliberate "effective context" rules.
    """
    return state


def run(req: RunRequest) -> RunResult:
    try:
        state = _load_state(req.state_path)
        warnings: list[str] = []

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

        # v0 Interpreter: only supports YAML root with "steps" list
        steps = orch.get("steps") or []
        if not isinstance(steps, list):
            raise ValueError("Orchestration 'steps' must be a list.")

        # Build adapter
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

        # Execute prompt steps in order (chain)
        for step in steps:
            if not isinstance(step, dict):
                continue

            step_type = step.get("type")
            step_name = step.get("name")

            if step_type != "prompt":
                warnings.append(f"Skipping unsupported step type: {step_type!r}")
                continue
            if not step_name:
                raise ValueError("Prompt step is missing 'name'.")

            template = step.get("template")
            if not isinstance(template, str) or not template.strip():
                raise ValueError(f"Prompt '{step_name}' missing non-empty 'template'.")

            ctx = _state_to_ctx(state)
            prompt_sent = _render_template(template, ctx)

            # record meta early
            state.setdefault(step_name, {})
            state[step_name]["meta"] = {
                "created_at": _now_iso(),
                "prompt_sent": prompt_sent,
                "adapter": effective.adapter,
                "model": effective.model,
                "dry_run": req.dry_run,
            }

            if req.dry_run:
                state[step_name]["value"] = None
                continue

            # call adapter
            res = adapter.generate(
                model=effective.model,
                prompt=prompt_sent,
                timeout_seconds=int(
                    (effective.runtime or {}).get("timeout_seconds") or 120
                ),
            )
            state[step_name]["value"] = res.text
            state[step_name]["meta"]["completed_at"] = _now_iso()

        # Optional: adapter connectivity proof in runtime for debugging
        if effective.adapter == "ollama" and not req.dry_run:
            try:
                tags = adapter.list_models(timeout_seconds=5)  # type: ignore[attr-defined]
                state["runtime"]["ollama_tags"] = tags
            except Exception as e:
                warnings.append(f"Could not fetch ollama tags: {e}")

        state["runtime"]["last_run"]["completed_at"] = _now_iso()
        return RunResult(ok=True, state=state, warnings=warnings)

    except Exception as e:
        return RunResult(
            ok=False, state=_load_state(req.state_path), warnings=[], error=str(e)
        )


def validate(orchestration_path: Path) -> dict[str, Any]:
    text = orchestration_path.read_text(encoding="utf-8").strip()
    ok = len(text) > 0
    return {"ok": ok, "errors": [] if ok else ["Orchestration file is empty."]}


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
