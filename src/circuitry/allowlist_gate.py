"""Run-time gate for the ``enabled_adapters`` / ``enabled_tools`` allowlists.

The static check in :func:`circuitry.cli.allowlist.check_allowlist` reads a
document's text; this module guards what a run actually builds. The CLI
installs the resolved allowlists into the run's shared ``runtime_config``
under :data:`ALLOWLISTS_KEY` (see :func:`install_allowlists`), every nested
runtime inherits that dict, and each place that builds an extension for a
run — ``ToolRuntime``, ``PromptRuntime._resolve_adapter`` and the run's own
adapter in ``runtime_shim.run`` — calls :func:`require_tool` /
:func:`require_adapter` first. That covers what the document text cannot
show: ``use`` children, generated plans, ``--adapter``, the config's
``default_adapter`` and profile provider overrides.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

#: ``runtime_config`` key holding ``{"adapters": [...] | None, "tools": [...] | None}``.
#: Underscore-prefixed like ``_orchestration_dir``: set by the CLI, never by a
#: document — :func:`install_allowlists` overwrites whatever a ``runtime:``
#: block carried.
ALLOWLISTS_KEY = "_allowlists"


class AllowlistError(ValueError):
    """An adapter or tool outside the configured allowlist was requested."""


def adapter_denial(name: str, allowed: list[str] | None) -> str | None:
    """The denial message for adapter *name*, or ``None`` when it is allowed."""
    if allowed is None or name in allowed:
        return None
    return f"adapter '{name}' not in enabled_adapters allowlist (enabled: {allowed})"


def tool_denial(name: str, allowed: list[str] | None) -> str | None:
    """The denial message for tool plugin *name*, or ``None`` when it is allowed."""
    if allowed is None or name in allowed:
        return None
    return f"tool '{name}' not in enabled_tools allowlist (enabled: {allowed})"


def require_adapter(name: str, allowed: list[str] | None) -> None:
    """Raise :class:`AllowlistError` unless adapter *name* may be built.

    *name* is canonicalised the way ``build_adapter`` canonicalises it, so
    ``" OpenAI"`` is checked as ``openai``.
    """
    denial = adapter_denial((name or "").strip().lower(), allowed)
    if denial is not None:
        raise AllowlistError(denial)


def require_tool(name: str, allowed: list[str] | None) -> None:
    """Raise :class:`AllowlistError` unless tool plugin *name* may be built.

    *name* is canonicalised the way ``build_plugin`` canonicalises it.
    """
    denial = tool_denial((name or "").strip().lower(), allowed)
    if denial is not None:
        raise AllowlistError(denial)


def install_allowlists(
    runtime_config: dict[str, Any],
    *,
    enabled_adapters: list[str] | None,
    enabled_tools: list[str] | None,
) -> None:
    """Record the run's resolved allowlists in its shared ``runtime_config``."""
    runtime_config[ALLOWLISTS_KEY] = {
        "adapters": None if enabled_adapters is None else list(enabled_adapters),
        "tools": None if enabled_tools is None else list(enabled_tools),
    }


def allowed_adapters(runtime_config: Mapping[str, Any] | None) -> list[str] | None:
    """The run's ``enabled_adapters``, or ``None`` (default-open) when none was installed."""
    return _installed(runtime_config, "adapters")


def allowed_tools(runtime_config: Mapping[str, Any] | None) -> list[str] | None:
    """The run's ``enabled_tools``, or ``None`` (default-open) when none was installed."""
    return _installed(runtime_config, "tools")


def _installed(runtime_config: Mapping[str, Any] | None, kind: str) -> list[str] | None:
    entry = (runtime_config or {}).get(ALLOWLISTS_KEY)
    if not isinstance(entry, Mapping):
        return None
    value = entry.get(kind)
    return list(value) if isinstance(value, list) else None
