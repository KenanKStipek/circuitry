"""Resolve the merged adapter/model/plugin/runtime configuration for a run.

Note: the resulting `EffectiveSettings.runtime` dict is the *live* config used
to build adapters, persistence backends, and tool plugins, so credential
fields are intentionally NOT redacted here. The runtime snapshot embedded in
`state["runtime"]["effective_settings"]` is redacted at the embed site (see
`runtime_shim.run` and `circuitry.cli.redaction`).
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Optional

from .config import CircuitryConfig


@dataclass(frozen=True)
class EffectiveSettings:
    model: Optional[str]
    adapter: Optional[str]
    plugins: list[str]
    runtime: dict[str, Any]
    sources: dict[
        str, str
    ]  # where each value came from (cli/orchestration/config/default)


def _merge_runtime(
    config_runtime: dict[str, Any], orch_runtime: dict[str, Any]
) -> dict[str, Any]:
    merged = dict(config_runtime or {})
    merged.update(orch_runtime or {})
    return merged


def resolve_effective_settings(
    *,
    cfg: CircuitryConfig,
    orch: dict[str, Any],
    cli_model: Optional[str] = None,
    cli_adapter: Optional[str] = None,
    cli_plugins: Optional[list[str]] = None,
) -> EffectiveSettings:
    sources: dict[str, str] = {}
    model: Optional[str]
    adapter: Optional[str]
    plugins: list[str]

    # model precedence: cli > orch > config > default
    if cli_model is not None:
        model = cli_model
        sources["model"] = "cli"
    elif orch.get("model") is not None:
        raw_model = orch.get("model")
        model = str(raw_model) if raw_model is not None else None
        sources["model"] = "orchestration"
    elif cfg.default_model is not None:
        model = cfg.default_model
        sources["model"] = "config"
    else:
        model = None
        sources["model"] = "default"

    # adapter precedence: cli > orch > config > default
    if cli_adapter is not None:
        adapter = cli_adapter
        sources["adapter"] = "cli"
    elif orch.get("adapter") is not None:
        raw_adapter = orch.get("adapter")
        adapter = str(raw_adapter) if raw_adapter is not None else None
        sources["adapter"] = "orchestration"
    elif cfg.default_adapter is not None:
        adapter = cfg.default_adapter
        sources["adapter"] = "config"
    else:
        adapter = None
        sources["adapter"] = "default"

    # plugins: cli replaces if provided; else merge config + orch (dedupe)
    orch_plugins = orch.get("plugins") or []
    if orch_plugins and not isinstance(orch_plugins, list):
        raise ValueError("Orchestration 'plugins' must be a list if provided.")

    if cli_plugins is not None:
        plugins = list(cli_plugins)
        sources["plugins"] = "cli"
    else:
        combined = [*cfg.plugins, *orch_plugins]
        seen: set[str] = set()
        plugins = []
        for p in combined:
            if not isinstance(p, str):
                raise ValueError("Plugins must be strings.")
            if p not in seen:
                seen.add(p)
                plugins.append(p)
        sources["plugins"] = (
            "orchestration"
            if orch_plugins
            else ("config" if cfg.plugins else "default")
        )

    # runtime: shallow merge, orch overrides config
    orch_runtime = orch.get("runtime") or {}
    if orch_runtime and not isinstance(orch_runtime, dict):
        raise ValueError("Orchestration 'runtime' must be an object if provided.")

    runtime = _merge_runtime(cfg.runtime, orch_runtime)
    sources["runtime"] = (
        "orchestration" if orch_runtime else ("config" if cfg.runtime else "default")
    )

    return EffectiveSettings(
        model=model,
        adapter=adapter,
        plugins=plugins,
        runtime=runtime,
        sources=sources,
    )
