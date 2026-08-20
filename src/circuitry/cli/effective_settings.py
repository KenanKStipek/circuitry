"""Resolve the merged adapter/model/plugin/runtime configuration for a run.

Note: the resulting `EffectiveSettings.runtime` dict is the *live* config used
to build adapters, persistence backends, and tool plugins, so credential
fields are intentionally NOT redacted here. The runtime snapshot embedded in
`state["runtime"]["effective_settings"]` is redacted at the embed site (see
`runtime_shim.run` and `circuitry.cli.redaction`).
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Optional

from .complexity_config import (
    ComplexitySettings,
    DEFAULT_COMPLEXITY_SETTINGS,
    resolve_complexity_settings,
)
from .config import CircuitryConfig

if TYPE_CHECKING:
    from .profiles import ProfileSettings


@dataclass(frozen=True)
class EffectiveSettings:
    model: Optional[str]
    adapter: Optional[str]
    plugins: list[str]
    runtime: dict[str, Any]
    sources: dict[
        str, str
    ]  # where each value came from (cli/orchestration/config/default)
    # Typed view of runtime["complexity"], validated at resolution time.
    complexity: ComplexitySettings = DEFAULT_COMPLEXITY_SETTINGS


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
    profile: "Optional[ProfileSettings]" = None,
) -> EffectiveSettings:
    sources: dict[str, str] = {}
    model: Optional[str]
    adapter: Optional[str]
    plugins: list[str]

    # model precedence: cli > profile > orch > config > default
    if cli_model is not None:
        model = cli_model
        sources["model"] = "cli"
    elif profile is not None and profile.model is not None:
        model = profile.model
        sources["model"] = "profile"
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

    # adapter precedence: cli > profile > orch > config > default
    if cli_adapter is not None:
        adapter = cli_adapter
        sources["adapter"] = "cli"
    elif profile is not None and profile.adapter is not None:
        adapter = profile.adapter
        sources["adapter"] = "profile"
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

    # persistence: a profile's `persistence:` block replaces (never merges
    # with) whatever the orchestration/config supplied — backends take
    # disjoint config keys, so a partial overlay would produce a chimera.
    # `enabled` defaults to true for a profile-supplied block: naming a
    # backend in a profile is the opt-in. There is no CLI persistence flag
    # today; if one is added it layers on top of this.
    if profile is not None and profile.persistence is not None:
        profile_persistence = dict(profile.persistence)
        profile_persistence.setdefault("enabled", True)
        runtime = dict(runtime)
        runtime["persistence"] = profile_persistence
        sources["persistence"] = "profile"
    elif isinstance(orch_runtime.get("persistence"), dict):
        sources["persistence"] = "orchestration"
    elif isinstance((cfg.runtime or {}).get("persistence"), dict):
        sources["persistence"] = "config"

    # complexity: no separate plumbing — the block rides the same shallow
    # runtime merge above, so an orchestration-level block replaces the
    # config-level one wholesale. Resolving it here means a malformed block
    # fails at config resolution rather than mid-run, and records provenance
    # alongside model/adapter/persistence.
    complexity = resolve_complexity_settings(runtime)
    _record_complexity_sources(
        sources,
        config_block=(cfg.runtime or {}).get("complexity"),
        orch_block=orch_runtime.get("complexity"),
    )

    return EffectiveSettings(
        model=model,
        adapter=adapter,
        plugins=plugins,
        runtime=runtime,
        sources=sources,
        complexity=complexity,
    )


def _record_complexity_sources(
    sources: dict[str, str],
    *,
    config_block: Any,
    orch_block: Any,
) -> None:
    """Record which layer supplied the complexity block and each sub-block.

    Sub-block provenance is not redundant with the block-level entry: the
    runtime merge replaces the whole `complexity` key, so an orchestration
    block that only defines `routing` leaves `scoring` on its defaults even
    when the config file defined one.
    """
    orch_is_block = isinstance(orch_block, dict)
    config_is_block = isinstance(config_block, dict)

    if orch_is_block:
        winner: dict[str, Any] = orch_block
        sources["complexity"] = "orchestration"
    elif config_is_block:
        winner = config_block
        sources["complexity"] = "config"
    else:
        winner = {}
        sources["complexity"] = "default"

    for key in ("scoring", "routing", "decomposition"):
        sources[f"complexity.{key}"] = (
            sources["complexity"] if key in winner else "default"
        )
