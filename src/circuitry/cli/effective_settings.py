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
    # Host `runtime:` keys and unlisted `plugins:` entries the orchestration
    # set: one line each when they were dropped, or the one "Applied host
    # settings" notice when the document is trusted. For the run's own warning
    # channel (RunResult.warnings / the validate report).
    warnings: tuple[str, ...] = ()


#: The `runtime:` keys a limited orchestration document may set. Everything
#: else in a document's `runtime:` block (adapters, plugins, persistence,
#: library, ...) is host configuration and only comes from config: a document
#: that reached `cof` indirectly can be someone else's (`run-library`, a library
#: name, a github source, an MCP or REST caller), so it must not be able to
#: repoint an adapter or launch a binary. A trusted document — one the caller
#: named by path (`trust_document`), or any document once the host sets
#: `trust_orchestration_runtime` — keeps its whole block.
ORCHESTRATION_RUNTIME_KEYS: frozenset[str] = frozenset({"complexity", "state"})

#: How many levels below `runtime.<key>` the "Applied host settings" notice
#: names: `runtime.adapters.openai.base_url`, never deeper (an `env` map's
#: variable names stay out of it). Keys only, never values.
_NOTICE_KEY_DEPTH = 2


def _split_orchestration_runtime(
    orch_runtime: dict[str, Any], *, trusted: bool
) -> tuple[dict[str, Any], list[str]]:
    """Keep the author-level keys of a document's `runtime:` block.

    Returns the accepted block and one warning per dropped key. A trusted
    document keeps its whole block.
    """
    if trusted:
        return dict(orch_runtime), []
    allowed = ", ".join(f"runtime.{k}" for k in sorted(ORCHESTRATION_RUNTIME_KEYS))
    accepted: dict[str, Any] = {}
    warnings: list[str] = []
    for key, value in orch_runtime.items():
        if key in ORCHESTRATION_RUNTIME_KEYS:
            accepted[key] = value
            continue
        warnings.append(
            f"Ignored runtime.{key} from the orchestration: it is a host "
            f"setting and must go in config.json (an orchestration may only "
            f"set {allowed})."
        )
    return accepted, warnings


def _split_orchestration_plugins(
    orch_plugins: list[Any], *, cfg: CircuitryConfig, trusted: bool
) -> tuple[list[Any], list[str]]:
    """Keep the document `plugins:` entries the host config already lists.

    A runtime plugin is an imported module, so a document may only name one
    that config's `plugins` or `enabled_plugins` already carries. A trusted
    document keeps its whole list.
    """
    if trusted:
        return list(orch_plugins), []
    host_listed = {*cfg.plugins, *(cfg.enabled_plugins or [])}
    accepted: list[Any] = []
    warnings: list[str] = []
    for plugin_id in orch_plugins:
        # Non-strings pass through to the "Plugins must be strings" error.
        if not isinstance(plugin_id, str) or plugin_id in host_listed:
            accepted.append(plugin_id)
            continue
        warnings.append(
            f"Skipped plugin '{plugin_id}' from the orchestration: it is not "
            f"listed in config.json 'plugins' or 'enabled_plugins'."
        )
    return accepted, warnings


def _key_label(key: Any) -> str:
    """A document key as notice text, kept on one line."""
    text = str(key)
    return text if text.isprintable() else repr(text)


def _key_paths(prefix: str, value: Any, depth: int) -> list[str]:
    if depth <= 0 or not isinstance(value, dict) or not value:
        return [prefix]
    return [
        path
        for key, sub in value.items()
        for path in _key_paths(f"{prefix}.{_key_label(key)}", sub, depth - 1)
    ]


def _ceiling_intersected_paths(cfg: CircuitryConfig) -> frozenset[str]:
    """The `_CEILING_LIST_KEYS` notice paths for which the host actually has
    a pin set — those are narrowed to an intersection, never applied
    outright, so the notice says so (#316)."""
    config_runtime = cfg.runtime or {}
    paths: set[str] = set()
    for top_key, name, leaf_key in _CEILING_LIST_KEYS:
        host_block = config_runtime.get(top_key)
        host_cfg = host_block.get(name) if isinstance(host_block, dict) else None
        host_list = host_cfg.get(leaf_key) if isinstance(host_cfg, dict) else None
        if isinstance(host_list, list):
            paths.add(f"runtime.{top_key}.{name}.{leaf_key}")
    return frozenset(paths)


def _applied_host_settings_notice(
    orch_runtime: dict[str, Any],
    orch_plugins: list[Any],
    *,
    cfg: CircuitryConfig,
    document_name: str | None,
) -> list[str]:
    """The one line naming the host settings a trusted document applies.

    Dotted key paths for the `runtime:` keys outside
    ORCHESTRATION_RUNTIME_KEYS and the `plugins:` entries config does not
    list; no values, so a credential in the document never reaches it. Empty
    when the document sets only author-level keys. A `_CEILING_LIST_KEYS`
    path is tagged to say it was narrowed to an intersection with the host's
    pin, not applied as the document wrote it.
    """
    ceiling_paths = _ceiling_intersected_paths(cfg)
    runtime_paths = [
        f"{path} (intersected with host pin)" if path in ceiling_paths else path
        for key, value in orch_runtime.items()
        if key not in ORCHESTRATION_RUNTIME_KEYS
        for path in _key_paths(f"runtime.{_key_label(key)}", value, _NOTICE_KEY_DEPTH)
    ]
    host_listed = {*cfg.plugins, *(cfg.enabled_plugins or [])}
    plugin_ids = [
        _key_label(p) for p in orch_plugins if isinstance(p, str) and p not in host_listed
    ]
    if not runtime_paths and not plugin_ids:
        return []
    parts = [*runtime_paths]
    if plugin_ids:
        parts.append("plugins: " + ", ".join(plugin_ids))
    source = document_name or "the orchestration"
    return [f"Applied host settings from {source}: {', '.join(parts)}"]


def orchestration_host_setting_warnings(
    orch: dict[str, Any],
    cfg: CircuitryConfig,
    *,
    trust_document: bool = False,
    document_name: str | None = None,
) -> list[str]:
    """The warnings `resolve_effective_settings` would record for *orch*.

    For `cof check`, which reports them without resolving a whole run (and
    without failing on a malformed block — schema validation owns that).
    """
    orch_plugins = orch.get("plugins")
    if not isinstance(orch_plugins, list):
        orch_plugins = []
    orch_runtime = orch.get("runtime")
    if not isinstance(orch_runtime, dict):
        orch_runtime = {}
    if trust_document or cfg.trust_orchestration_runtime:
        return _applied_host_settings_notice(
            orch_runtime, orch_plugins, cfg=cfg, document_name=document_name
        )
    return [
        *_split_orchestration_plugins(orch_plugins, cfg=cfg, trusted=False)[1],
        *_split_orchestration_runtime(orch_runtime, trusted=False)[1],
    ]


#: `runtime` keys merged one level deeper than the rest: a document setting
#: `plugins.sqlite.*` must not drop `plugins.shell.*`, and one setting
#: `adapters.ollama.*` must not drop every other adapter's config (#316).
#: Every other top-level runtime key (`complexity`, `persistence`, `state`,
#: `library`, ...) still replaces the config-level value wholesale — see
#: `complexity_config` and the `persistence`/`complexity` comments below for
#: why that is each key's own deliberate choice, not an oversight shared with
#: this one.
_DEEP_MERGE_RUNTIME_KEYS: frozenset[str] = frozenset({"plugins", "adapters"})

#: Host ceiling that survives a merge as an intersection, never a plain
#: overlay — a document (trusted or not) can only narrow it, never replace
#: it outright (#316, the shell half of #264). `(top_key, name, leaf_key)`.
_CEILING_LIST_KEYS: tuple[tuple[str, str, str], ...] = (
    ("plugins", "shell", "allowed_commands"),
)


def _deep_merge_dicts(base: dict[str, Any], overlay: dict[str, Any]) -> dict[str, Any]:
    """Recursively merge *overlay* onto *base*; a nested dict merges key by
    key, any other value (including a list) replaces the base's value for
    that key outright."""
    merged = dict(base)
    for key, value in overlay.items():
        base_value = merged.get(key)
        if isinstance(value, dict) and isinstance(base_value, dict):
            merged[key] = _deep_merge_dicts(base_value, value)
        else:
            merged[key] = value
    return merged


def _apply_ceiling_intersections(
    merged: dict[str, Any], *, config_runtime: dict[str, Any], orch_runtime: dict[str, Any]
) -> dict[str, Any]:
    """Re-narrow each `_CEILING_LIST_KEYS` entry to the host/document
    intersection after the deep merge, which would otherwise let a
    document's own list — or a `null`/non-dict value that erases the whole
    block the deep merge would otherwise have preserved — drop the host's
    pin instead of narrowing it. Whenever the host has a pin, it survives:
    intersected against the document's own list if it gave one, or
    untouched if the document's value for that leaf (or an ancestor block)
    isn't a list at all. The one leaf the merge must not treat like every
    other overridable key."""
    for top_key, name, leaf_key in _CEILING_LIST_KEYS:
        host_block = (config_runtime or {}).get(top_key)
        host_cfg = host_block.get(name) if isinstance(host_block, dict) else None
        host_list = host_cfg.get(leaf_key) if isinstance(host_cfg, dict) else None
        if not isinstance(host_list, list):
            continue
        orch_block = (orch_runtime or {}).get(top_key)
        orch_cfg = orch_block.get(name) if isinstance(orch_block, dict) else None
        orch_list = orch_cfg.get(leaf_key) if isinstance(orch_cfg, dict) else None
        effective = (
            [c for c in host_list if c in orch_list]
            if isinstance(orch_list, list)
            else host_list
        )
        merged = dict(merged)
        merged_top = merged.get(top_key)
        merged[top_key] = dict(merged_top) if isinstance(merged_top, dict) else {}
        merged_name = merged[top_key].get(name)
        merged[top_key][name] = dict(merged_name) if isinstance(merged_name, dict) else {}
        merged[top_key][name][leaf_key] = effective
    return merged


def _merge_runtime(
    config_runtime: dict[str, Any], orch_runtime: dict[str, Any]
) -> dict[str, Any]:
    config_runtime = config_runtime or {}
    orch_runtime = orch_runtime or {}
    merged = dict(config_runtime)
    for key, value in orch_runtime.items():
        base_value = merged.get(key)
        if (
            key in _DEEP_MERGE_RUNTIME_KEYS
            and isinstance(value, dict)
            and isinstance(base_value, dict)
        ):
            merged[key] = _deep_merge_dicts(base_value, value)
        else:
            merged[key] = value
    return _apply_ceiling_intersections(
        merged, config_runtime=config_runtime, orch_runtime=orch_runtime
    )


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
    trust_document: bool = False,
    document_name: str | None = None,
) -> EffectiveSettings:
    """Merge cli > profile > orchestration > config > default for one run.

    *trust_document* is True only when the caller named the document by path
    (`cof run ./my.yml`, the SDK's `run_orchestration`); it then keeps the
    document's whole `runtime:` block and `plugins:` list, as does
    `cfg.trust_orchestration_runtime`. *document_name* labels the notice a
    trusted document that applies host settings produces.
    """
    trusted = trust_document or cfg.trust_orchestration_runtime
    sources: dict[str, str] = {}
    warnings: list[str] = []
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

    # plugins: cli replaces if provided; else merge config + the orch entries
    # config already lists (dedupe)
    orch_plugins = orch.get("plugins") or []
    if orch_plugins and not isinstance(orch_plugins, list):
        raise ValueError("Orchestration 'plugins' must be a list if provided.")
    orch_runtime = orch.get("runtime") or {}
    if orch_runtime and not isinstance(orch_runtime, dict):
        raise ValueError("Orchestration 'runtime' must be an object if provided.")
    if trusted:
        warnings.extend(
            _applied_host_settings_notice(
                orch_runtime,
                orch_plugins if cli_plugins is None else [],
                cfg=cfg,
                document_name=document_name,
            )
        )

    if cli_plugins is not None:
        plugins = list(cli_plugins)
        sources["plugins"] = "cli"
    else:
        orch_plugins, plugin_warnings = _split_orchestration_plugins(
            orch_plugins, cfg=cfg, trusted=trusted
        )
        warnings.extend(plugin_warnings)
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

    # runtime: orch overrides config, deep-merged under `plugins`/`adapters`
    # (see _DEEP_MERGE_RUNTIME_KEYS), shallow everywhere else — for the
    # author-level keys only, unless the document is trusted (see
    # ORCHESTRATION_RUNTIME_KEYS)
    orch_runtime, runtime_warnings = _split_orchestration_runtime(
        orch_runtime, trusted=trusted
    )
    warnings.extend(runtime_warnings)

    runtime = _merge_runtime(cfg.runtime, orch_runtime)
    sources["runtime"] = (
        "orchestration" if orch_runtime else ("config" if cfg.runtime else "default")
    )

    # adapter timeout: `runtime.adapters.<adapter>.timeout_seconds` (see
    # cli.runtime_shim, which resolves it generically for whichever adapter
    # the run selected — ollama and every curl-based adapter share the same
    # `generate(timeout_seconds=...)` knob). `adapters` merges one level
    # deeper than the rest of `runtime` (see `_DEEP_MERGE_RUNTIME_KEYS`), so
    # an orchestration setting a *different* adapter's config must not claim
    # this one's timeout — check the document's own `adapters.<adapter>`
    # sub-block, not just whether it touched `adapters` at all.
    # "default" means the key was absent and the 120s fallback in
    # runtime_shim applies.
    if adapter:
        adapters_cfg = runtime.get("adapters")
        this_adapter_cfg = (
            adapters_cfg.get(adapter) if isinstance(adapters_cfg, dict) else None
        )
        if (
            isinstance(this_adapter_cfg, dict)
            and this_adapter_cfg.get("timeout_seconds") is not None
        ):
            orch_adapters_cfg = orch_runtime.get("adapters")
            orch_this_adapter_cfg = (
                orch_adapters_cfg.get(adapter)
                if isinstance(orch_adapters_cfg, dict)
                else None
            )
            sources[f"adapters.{adapter}.timeout_seconds"] = (
                "orchestration"
                if isinstance(orch_this_adapter_cfg, dict)
                and orch_this_adapter_cfg.get("timeout_seconds") is not None
                else "config"
            )
        else:
            sources[f"adapters.{adapter}.timeout_seconds"] = "default"

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
        warnings=tuple(warnings),
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
