"""Resolve the merged adapter/model/plugin/runtime configuration for a run.

Note: the resulting `EffectiveSettings.runtime` dict is the *live* config used
to build adapters, persistence backends, and tool plugins, so credential
fields are intentionally NOT redacted here. The runtime snapshot embedded in
`state["runtime"]["effective_settings"]` is redacted at the embed site (see
`runtime_shim.run` and `circuitry.cli.redaction`).
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any

from .complexity_config import (
    DEFAULT_COMPLEXITY_SETTINGS,
    ComplexitySettings,
    RoutingSettings,
    resolve_complexity_settings,
)
from .config import CircuitryConfig

if TYPE_CHECKING:
    from .profiles import ProfileSettings


@dataclass(frozen=True)
class EffectiveSettings:
    model: str | None
    adapter: str | None
    out: Path | None
    plugins: list[str]
    runtime: dict[str, Any]
    sources: dict[
        str, str
    ]  # where each value came from (cli/router/orchestration/config/default)
    # Typed view of runtime["complexity"], validated at resolution time.
    complexity: ComplexitySettings = DEFAULT_COMPLEXITY_SETTINGS
    # True when `model` was pinned by a deliberate choice — `--model` or a
    # profile's run-level `model:` — rather than inherited from the
    # orchestration or config. Read by the runtime to tell the complexity
    # router which of the two it is looking at: it outranks the inherited
    # defaults and defers to the pinned ones. Not derivable from `sources`
    # once the router itself wins that entry.
    model_locked: bool = False


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
    cli_model: str | None = None,
    cli_adapter: str | None = None,
    cli_plugins: list[str] | None = None,
    cli_out: Path | None = None,
    cli_scoring: bool | None = None,
    cli_routing: bool | None = None,
    cli_decompose: bool | None = None,
    profile: ProfileSettings | None = None,
) -> EffectiveSettings:
    sources: dict[str, str] = {}
    model: str | None
    adapter: str | None
    out: Path | None
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

    # out precedence: cli > profile > default (no file written). Unlike
    # model/adapter there is no orchestration/config layer — `--out` has
    # never had one, and a profile is orthogonal to the persistence backend
    # selected via the `persistence` block (see docs/profiles.md).
    if cli_out is not None:
        out = cli_out
        sources["out"] = "cli"
    elif profile is not None and profile.out is not None:
        out = Path(profile.out)
        sources["out"] = "profile"
    else:
        out = None
        sources["out"] = "default"

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
    #
    # `--scoring`/`--routing`/`--decompose` are layered onto the merged block
    # *before* it validates, each flipping only its own switch's `enabled`
    # field — so a flag combination that violates the scoring prerequisite
    # raises the identical `ComplexityConfigError` the config path raises,
    # and a flag never has to restate bands/weights/thresholds it isn't
    # touching.
    runtime = _apply_cli_complexity_overrides(
        runtime,
        cli_scoring=cli_scoring,
        cli_routing=cli_routing,
        cli_decompose=cli_decompose,
    )
    complexity = resolve_complexity_settings(runtime)
    _record_complexity_sources(
        sources,
        config_block=(cfg.runtime or {}).get("complexity"),
        orch_block=orch_runtime.get("complexity"),
        cli_scoring=cli_scoring,
        cli_routing=cli_routing,
        cli_decompose=cli_decompose,
    )

    # Resolved after the complexity block because that is what decides it: the
    # router sits between the pinned layers and the inherited ones, so it can
    # only be slotted into the model chain once the routing switch is known.
    model, model_locked = _apply_router_precedence(
        sources, model=model, routing=complexity.routing
    )

    return EffectiveSettings(
        model=model,
        adapter=adapter,
        out=out,
        plugins=plugins,
        runtime=runtime,
        sources=sources,
        complexity=complexity,
        model_locked=model_locked,
    )


def _apply_router_precedence(
    sources: dict[str, str],
    *,
    model: str | None,
    routing: RoutingSettings,
) -> tuple[str | None, bool]:
    """Slot the complexity router into the model precedence chain.

    Full order, once routing is in it:

    ``--model`` > per-effect ``model:`` > profile effect override > **router**
    > orchestration default > config default

    Only the run-level layers are visible here; the two per-effect ones live on
    the compiled definition and are applied at dispatch. What this function
    settles is the boundary either side of the router — which is why it returns
    ``model_locked`` as well as the model: the runtime needs to know whether the
    string it is handed came from above the router or below it, and once the
    router wins ``sources["model"]`` that entry no longer says.

    The value in ``model`` stays the *run default* even when the router wins the
    source entry, because the router's real answer is per-effect: this is what a
    prompt falls back to when there is no score to route on, and what every
    non-prompt effect uses. The one exception is a run that configures a band
    table and no default model at all — a perfectly coherent thing to want,
    which used to fail with "no model resolved". There the catch-all band, whose
    whole job is "the model for anything not otherwise matched", *is* the run
    default.
    """
    locked = sources.get("model") in ("cli", "profile")
    if not routing.enabled or not routing.bands:
        return model, locked
    if locked and routing.respect_explicit:
        return model, locked

    sources["model"] = "router"
    if model is None:
        # Validation guarantees a non-empty table ends in the catch-all.
        model = routing.bands[-1].model
    return model, locked


def _apply_cli_complexity_overrides(
    runtime: dict[str, Any],
    *,
    cli_scoring: bool | None,
    cli_routing: bool | None,
    cli_decompose: bool | None,
) -> dict[str, Any]:
    """Layer `--scoring`/`--routing`/`--decompose` onto the merged runtime.

    Each flag flips only its switch's `enabled` field, leaving the rest of
    that sub-block (weights, bands, threshold, ...) exactly as the
    orchestration/config left it — a flag never has to restate a band table
    just to force routing on for one run.

    A malformed `complexity` (or sub-)block is left untouched rather than
    coerced into a dict here: `resolve_complexity_settings` raises its own
    named-path "must be an object" error for it, and that error is more
    useful than one about an override this function invented.
    """
    if cli_scoring is None and cli_routing is None and cli_decompose is None:
        return runtime

    complexity_raw = runtime.get("complexity")
    if complexity_raw is not None and not isinstance(complexity_raw, dict):
        return runtime
    complexity: dict[str, Any] = dict(complexity_raw) if complexity_raw else {}

    def _set_enabled(key: str, value: bool | None) -> None:
        if value is None:
            return
        sub_raw = complexity.get(key)
        if sub_raw is not None and not isinstance(sub_raw, dict):
            return
        sub: dict[str, Any] = dict(sub_raw) if sub_raw else {}
        sub["enabled"] = value
        complexity[key] = sub

    _set_enabled("scoring", cli_scoring)
    _set_enabled("routing", cli_routing)
    _set_enabled("decomposition", cli_decompose)

    runtime = dict(runtime)
    runtime["complexity"] = complexity
    return runtime


def _record_complexity_sources(
    sources: dict[str, str],
    *,
    config_block: Any,
    orch_block: Any,
    cli_scoring: bool | None = None,
    cli_routing: bool | None = None,
    cli_decompose: bool | None = None,
) -> None:
    """Record which layer supplied the complexity block and each sub-block.

    Sub-block provenance is not redundant with the block-level entry: the
    runtime merge replaces the whole `complexity` key, so an orchestration
    block that only defines `routing` leaves `scoring` on its defaults even
    when the config file defined one. A CLI flag outranks both layers for its
    own switch only — the block-level entry still names whichever of
    orchestration/config/default supplied everything the flags didn't touch.
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

    cli_flags = {
        "scoring": cli_scoring,
        "routing": cli_routing,
        "decomposition": cli_decompose,
    }
    for key in ("scoring", "routing", "decomposition"):
        if cli_flags[key] is not None:
            sources[f"complexity.{key}"] = "cli"
        else:
            sources[f"complexity.{key}"] = (
                sources["complexity"] if key in winner else "default"
            )

    # Fine-grained provenance for the values that have no CLI override of
    # their own — the band table and the three decomposition scalars. Each is
    # sourced from whichever layer's sub-block actually defines that key: the
    # block winner can define `routing.enabled` without `routing.bands` (or
    # vice versa), so this can't just reuse `sources["complexity.routing"]`.
    def _field_source(sub_key: str, field_key: str) -> str:
        sub = winner.get(sub_key)
        if isinstance(sub, dict) and sub.get(field_key) is not None:
            return sources["complexity"]
        return "default"

    sources["complexity.routing.bands"] = _field_source("routing", "bands")
    sources["complexity.decomposition.threshold"] = _field_source(
        "decomposition", "threshold"
    )
    sources["complexity.decomposition.max_depth"] = _field_source(
        "decomposition", "max_depth"
    )
    sources["complexity.decomposition.on_failure"] = _field_source(
        "decomposition", "on_failure"
    )
