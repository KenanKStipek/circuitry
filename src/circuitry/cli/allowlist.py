"""Allowlist enforcement for adapters, tool plugins, and runtime plugins.

Walks an orchestration YAML dict to collect adapter and tool-plugin
references, then compares against the per-category allowlist on
:class:`CircuitryConfig`. Returns a list of human-readable error strings.

This is the static half. The run-time half —
:mod:`circuitry.allowlist_gate` — guards what a run actually builds, which
covers templated ``inline:`` children, generated plans, ``--adapter``, the
config's ``default_adapter`` and profile provider overrides.

Runtime plugins are not referenced by the orchestration YAML — their
allowlist is enforced at plugin load time in
:func:`circuitry.cli.runtime_shim._initialize_plugins` via
:func:`load_plugins(..., allowed=...)`.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..allowlist_gate import adapter_denial, tool_denial
from ..core.cycle_check import iter_use_children
from .config import CircuitryConfig


def walk_orchestration_refs(orch: dict[str, Any]) -> tuple[set[str], set[str]]:
    """Collect (adapter_names, tool_names) referenced in a YAML orchestration.

    Adapter references come from:
      * top-level ``adapter:``
      * prompt-effect ``provider:`` and each item of ``provider_fallbacks``
        (provider tokens follow ``adapter[:model]`` syntax — see
        ``PromptRuntime._parse_provider_token``).

    Tool references come from each ``type: tool`` effect's ``provider:``.

    Walks recursively into dynamic, conditional (if/conditional), loop, and
    reflector effects. Does NOT cross ``use:`` boundaries — each child is
    walked on its own (:func:`check_allowlist` statically, ``UseRuntime`` as
    it loads).
    """
    adapters: set[str] = set()
    tools: set[str] = set()

    if isinstance(orch, dict):
        top_adapter = orch.get("adapter")
        if isinstance(top_adapter, str) and top_adapter.strip():
            adapters.add(top_adapter.strip())
        _walk_effects(orch.get("effects"), adapters, tools)

    return adapters, tools


def _walk_effects(
    effects: Any, adapters: set[str], tools: set[str]
) -> None:
    if not isinstance(effects, list):
        return
    for effect in effects:
        if not isinstance(effect, dict):
            continue
        etype = effect.get("type")

        if etype == "prompt":
            primary = effect.get("provider")
            if isinstance(primary, str):
                name = _provider_token_to_adapter(primary)
                if name:
                    adapters.add(name)
            for tok in effect.get("provider_fallbacks") or []:
                if isinstance(tok, str):
                    name = _provider_token_to_adapter(tok)
                    if name:
                        adapters.add(name)
        elif etype == "tool":
            prov = effect.get("provider")
            if isinstance(prov, str) and prov.strip():
                tools.add(prov.strip())
        elif etype == "dynamic":
            _walk_effects(effect.get("effects"), adapters, tools)
        elif etype in ("if", "conditional"):
            _walk_effects(effect.get("then"), adapters, tools)
            _walk_effects(effect.get("else"), adapters, tools)
        elif etype == "loop":
            _walk_effects(effect.get("body"), adapters, tools)
        elif etype == "reflector":
            _walk_effects(effect.get("effects"), adapters, tools)
        # `use` children are walked as documents of their own — see
        # check_allowlist and UseRuntime.


@dataclass(frozen=True)
class AdapterUsage:
    """One effect's reference to an adapter, plus how it handles failure."""

    effect_name: str | None
    on_error: str  # "fail" | "skip" | "continue"


def collect_adapter_usages(orch: dict[str, Any]) -> dict[str, list[AdapterUsage]]:
    """Collect, for each adapter referenced in the orchestration, every effect
    that uses it and that effect's ``on_error`` handling.

    Mirrors the traversal in :func:`walk_orchestration_refs` but keeps the
    detail needed to classify a dependency as hard or soft: an adapter is a
    soft (skippable) dependency only when every recorded usage tolerates
    failure (``on_error: skip`` or ``on_error: continue``). A top-level
    ``adapter:`` default that no effect ends up using (e.g. a tool-only
    orchestration) has no entry here — callers should treat that as hard,
    since there's no effect to prove it's safe to skip.
    """
    usages: dict[str, list[AdapterUsage]] = {}
    if isinstance(orch, dict):
        top_adapter = orch.get("adapter")
        default_adapter = (
            top_adapter.strip()
            if isinstance(top_adapter, str) and top_adapter.strip()
            else None
        )
        _walk_effects_usages(orch.get("effects"), default_adapter, usages)
    return usages


def _walk_effects_usages(
    effects: Any,
    default_adapter: str | None,
    usages: dict[str, list[AdapterUsage]],
) -> None:
    if not isinstance(effects, list):
        return
    for effect in effects:
        if not isinstance(effect, dict):
            continue
        etype = effect.get("type")
        effect_name = effect.get("name") if isinstance(effect.get("name"), str) else None

        if etype == "prompt":
            on_error = effect.get("on_error")
            if on_error not in ("fail", "skip", "continue"):
                on_error = "fail"
            primary = effect.get("provider")
            adapter_name = (
                _provider_token_to_adapter(primary)
                if isinstance(primary, str)
                else None
            )
            if not adapter_name:
                adapter_name = default_adapter
            if adapter_name:
                usages.setdefault(adapter_name, []).append(
                    AdapterUsage(effect_name, on_error)
                )
            for tok in effect.get("provider_fallbacks") or []:
                if isinstance(tok, str):
                    fb_name = _provider_token_to_adapter(tok)
                    if fb_name:
                        usages.setdefault(fb_name, []).append(
                            AdapterUsage(effect_name, on_error)
                        )
        elif etype == "dynamic":
            _walk_effects_usages(effect.get("effects"), default_adapter, usages)
        elif etype in ("if", "conditional"):
            _walk_effects_usages(effect.get("then"), default_adapter, usages)
            _walk_effects_usages(effect.get("else"), default_adapter, usages)
        elif etype == "loop":
            _walk_effects_usages(effect.get("body"), default_adapter, usages)
        elif etype == "reflector":
            _walk_effects_usages(effect.get("effects"), default_adapter, usages)
        # `use` effects expand at compile time; not walked here either.


def is_hard_adapter_dependency(
    adapter_name: str, usages: dict[str, list[AdapterUsage]]
) -> bool:
    """Whether preflight must hard-fail when ``adapter_name`` isn't ready.

    Soft only when there's at least one recorded usage and every one of them
    tolerates failure (``on_error: skip``/``continue``).
    """
    effect_usages = usages.get(adapter_name)
    if not effect_usages:
        return True
    return any(u.on_error == "fail" for u in effect_usages)


def skippable_effect_names(
    adapter_name: str, usages: dict[str, list[AdapterUsage]]
) -> list[str]:
    """Names of effects using ``adapter_name`` that will skip cleanly if it's unavailable."""
    return [
        u.effect_name or "<unnamed>"
        for u in usages.get(adapter_name, [])
        if u.on_error != "fail"
    ]


def hard_effect_names(
    adapter_name: str, usages: dict[str, list[AdapterUsage]]
) -> list[str]:
    """Names of effects using ``adapter_name`` that have no failure tolerance."""
    return [
        u.effect_name or "<unnamed>"
        for u in usages.get(adapter_name, [])
        if u.on_error == "fail"
    ]


def _provider_token_to_adapter(token: str) -> str | None:
    """Extract the adapter portion of a prompt provider token.

    PromptRuntime treats both ``"openai"`` and ``"openai:gpt-4o"`` as
    adapter references (see ``_parse_provider_token``), so we mirror that.
    """
    parsed = (token or "").strip()
    if not parsed:
        return None
    if ":" in parsed:
        head, _ = parsed.split(":", 1)
        head = head.strip()
        return head or None
    return parsed


def orchestration_denials(
    orch: dict[str, Any],
    *,
    enabled_adapters: list[str] | None,
    enabled_tools: list[str] | None,
) -> list[str]:
    """Denial messages for one document's own adapter and tool references.

    ``None`` is default-open; a list (including ``[]``) is strict. Does not
    follow ``use:`` — see :func:`check_allowlist` for the static walk and
    ``UseRuntime`` for the run-time check of each child as it loads.

    A name with a Mustache tag in it (an unrendered ``inline:`` child's
    ``provider: "{{input.tool}}"``) is not judged here: it only becomes a
    name once rendered, and the run-time gate checks it then.
    """
    adapter_refs, tool_refs = walk_orchestration_refs(orch)
    errors = [
        denial
        for name in sorted(adapter_refs)
        if "{{" not in name
        and (denial := adapter_denial(name, enabled_adapters)) is not None
    ]
    errors.extend(
        denial
        for name in sorted(tool_refs)
        if "{{" not in name
        and (denial := tool_denial(name, enabled_tools)) is not None
    )
    return errors


def profile_provider_denials(
    effects: dict[str, dict[str, Any]], *, enabled_adapters: list[str] | None
) -> list[str]:
    """Denial messages for a profile's per-effect ``provider`` overrides.

    A profile swaps an effect's adapter after the document was checked, so its
    overrides are checked on their own before the run starts.
    """
    errors: list[str] = []
    for path in sorted(effects):
        token = effects[path].get("provider")
        name = _provider_token_to_adapter(token) if isinstance(token, str) else None
        denial = adapter_denial(name, enabled_adapters) if name else None
        if denial is not None:
            errors.append(f"profile override for '{path}': {denial}")
    return errors


def check_allowlist(
    *,
    orch: dict[str, Any],
    config: CircuitryConfig,
    root_path: Path | None = None,
) -> list[str]:
    """Return per-violation error strings; empty list if all allowed.

    An ``enabled_*`` value of ``None`` is default-open (no enforcement).
    A list (including ``[]``) is strict — only listed names are allowed.

    Covers *orch* and every ``use`` child reachable from it statically
    (``path:``/``ref:``, resolved relative to *root_path*, and plain
    ``inline:`` text); a child's violations are prefixed with its label.
    """
    allowlists = {
        "enabled_adapters": config.enabled_adapters,
        "enabled_tools": config.enabled_tools,
    }
    errors = orchestration_denials(orch, **allowlists)
    if config.enabled_adapters is None and config.enabled_tools is None:
        return errors

    for label, child in iter_use_children(
        orch, root_path=root_path, runtime=config.runtime
    ):
        errors.extend(
            f"use child {label}: {denial}"
            for denial in orchestration_denials(child, **allowlists)
        )
    return errors
