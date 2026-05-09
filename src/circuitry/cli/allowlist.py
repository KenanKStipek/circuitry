"""Allowlist enforcement for adapters, tool plugins, and runtime plugins.

Walks an orchestration YAML dict to collect adapter and tool-plugin
references, then compares against the per-category allowlist on
:class:`CircuitryConfig`. Returns a list of human-readable error strings.

Runtime plugins are not referenced by the orchestration YAML — their
allowlist is enforced at plugin load time in
:func:`circuitry.cli.runtime_shim._initialize_plugins` via
:func:`load_plugins(..., allowed=...)`.
"""

from __future__ import annotations

from typing import Any

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
    reflector effects. Does NOT cross ``use:`` boundaries — sub-orchestrations
    are validated independently.
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
        # `use` effects expand at compile time; their refs are validated
        # when the referenced orchestration is loaded.


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


def check_allowlist(
    *, orch: dict[str, Any], config: CircuitryConfig
) -> list[str]:
    """Return per-violation error strings; empty list if all allowed.

    An ``enabled_*`` value of ``None`` is default-open (no enforcement).
    A list (including ``[]``) is strict — only listed names are allowed.
    """
    errors: list[str] = []
    adapter_refs, tool_refs = walk_orchestration_refs(orch)

    if config.enabled_adapters is not None:
        allowed = config.enabled_adapters
        for name in sorted(adapter_refs):
            if name not in allowed:
                errors.append(
                    f"adapter '{name}' not in enabled_adapters allowlist "
                    f"(enabled: {allowed})"
                )

    if config.enabled_tools is not None:
        allowed = config.enabled_tools
        for name in sorted(tool_refs):
            if name not in allowed:
                errors.append(
                    f"tool '{name}' not in enabled_tools allowlist "
                    f"(enabled: {allowed})"
                )

    return errors
