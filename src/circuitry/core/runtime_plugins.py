from __future__ import annotations

import importlib
import logging
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Protocol

from ..preflight import CheckResult

logger = logging.getLogger(__name__)

PLUGIN_CONTRACT_VERSION = "1"


@dataclass(frozen=True)
class PluginContext:
    run_id: str
    orchestration_path: Path
    dry_run: bool
    validate_only: bool
    runtime_config: dict[str, Any]


class RuntimePlugin(Protocol):
    @property
    def name(self) -> str: ...

    def on_run_start(self, *, state: dict[str, Any], context: PluginContext) -> None: ...

    def on_run_success(
        self, *, state: dict[str, Any], context: PluginContext
    ) -> None: ...

    def on_run_failure(
        self, *, state: dict[str, Any], context: PluginContext, error: str
    ) -> None: ...

    # Optional: implement check() to declare runtime dependencies. Plugins
    # that don't implement it default to ok=True (see core.preflight.call_check).
    def check(self) -> CheckResult: ...

    # Optional: fired immediately before each effect dispatches, carrying the
    # effect's state node as it stands at that moment (the complexity score
    # included, when scoring is enabled). The mirror of on_effect_complete —
    # same optionality guard, so a plugin may implement either, both, or
    # neither.
    def on_effect_start(
        self,
        *,
        state: dict[str, Any],
        context: PluginContext,
        effect_path: str,
        effect_node: dict[str, Any],
    ) -> None: ...

    # Optional (Story 2): fired after each effect's node["value"] is written.
    # Plugins missing this method are skipped via hasattr guard in
    # invoke_plugins so external plugins predating this hook keep working.
    def on_effect_complete(
        self,
        *,
        state: dict[str, Any],
        context: PluginContext,
        effect_path: str,
        effect_result: dict[str, Any],
    ) -> None: ...


@dataclass(frozen=True)
class PluginLoadResult:
    plugin_id: str
    plugin: RuntimePlugin | None
    error: str | None = None


def load_plugins(
    plugin_ids: list[str],
    *,
    allowed: list[str] | None = None,
) -> list[PluginLoadResult]:
    """Load runtime plugins by dotted-path identifier.

    When ``allowed`` is non-None, plugin IDs not in the list are skipped
    with a "not in enabled_plugins allowlist" error (the import is not
    attempted). ``allowed`` of ``[]`` locks down all runtime plugins;
    ``None`` is default-open.
    """
    results: list[PluginLoadResult] = []
    for plugin_id in plugin_ids:
        if allowed is not None and plugin_id not in allowed:
            results.append(
                PluginLoadResult(
                    plugin_id=plugin_id,
                    plugin=None,
                    error=(
                        f"plugin '{plugin_id}' not in enabled_plugins "
                        f"allowlist (enabled: {allowed})"
                    ),
                )
            )
            continue
        try:
            plugin = _load_single_plugin(plugin_id)
            results.append(PluginLoadResult(plugin_id=plugin_id, plugin=plugin))
        except Exception as e:
            logger.warning("Failed to load plugin %r: %s", plugin_id, e, exc_info=True)
            results.append(
                PluginLoadResult(plugin_id=plugin_id, plugin=None, error=str(e))
            )
    return results


def invoke_plugins(
    *,
    plugins: list[RuntimePlugin],
    hook_name: str,
    state: dict[str, Any],
    context: PluginContext,
    error: str | None = None,
    effect_path: str | None = None,
    effect_result: dict[str, Any] | None = None,
    effect_node: dict[str, Any] | None = None,
) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []

    for plugin in plugins:
        plugin_name = _plugin_name(plugin)
        try:
            if hook_name == "on_run_start":
                plugin.on_run_start(state=state, context=context)
            elif hook_name == "on_run_success":
                plugin.on_run_success(state=state, context=context)
            elif hook_name == "on_run_failure":
                plugin.on_run_failure(
                    state=state, context=context, error=error or "unknown error"
                )
            elif hook_name == "on_effect_start":
                # Optional hook, same guard as on_effect_complete — a plugin
                # that only implements the completion half is untouched.
                start_handler = getattr(plugin, "on_effect_start", None)
                if start_handler is None or not callable(start_handler):
                    continue
                start_handler(
                    state=state,
                    context=context,
                    effect_path=effect_path or "",
                    effect_node=effect_node or {},
                )
            elif hook_name == "on_effect_complete":
                # Optional hook — plugins that predate Story 2 are skipped
                # silently. effect_path / effect_result must be supplied.
                handler = getattr(plugin, "on_effect_complete", None)
                if handler is None or not callable(handler):
                    continue
                handler(
                    state=state,
                    context=context,
                    effect_path=effect_path or "",
                    effect_result=effect_result or {},
                )
            else:
                raise ValueError(f"Unknown plugin hook: {hook_name}")

            events.append(
                {
                    "plugin": plugin_name,
                    "hook": hook_name,
                    "ok": True,
                    "error": None,
                }
            )
        except Exception as e:
            logger.warning(
                "Plugin %r hook %r failed: %s", plugin_name, hook_name, e, exc_info=True,
            )
            events.append(
                {
                    "plugin": plugin_name,
                    "hook": hook_name,
                    "ok": False,
                    "error": str(e),
                }
            )

    return events


def _load_single_plugin(plugin_id: str) -> RuntimePlugin:
    plugin_id = plugin_id.strip()
    if not plugin_id:
        raise ValueError("Plugin identifier cannot be empty")

    if ":" in plugin_id:
        module_name, attr_name = plugin_id.split(":", 1)
        module = importlib.import_module(module_name)
        attr = getattr(module, attr_name)
    else:
        module = importlib.import_module(plugin_id)
        attr = getattr(module, "plugin", None)
        if attr is None:
            raise ValueError(
                "Plugin module must expose a 'plugin' symbol or use module:attr"
            )

    if callable(attr):
        instance = attr()
    else:
        instance = attr

    _validate_plugin(instance, plugin_id)
    return instance


def _validate_plugin(instance: Any, plugin_id: str) -> None:
    for required in ("on_run_start", "on_run_success", "on_run_failure"):
        if not hasattr(instance, required) or not callable(getattr(instance, required)):
            raise ValueError(
                f"Plugin '{plugin_id}' does not implement required hook: {required}"
            )


def _plugin_name(plugin: RuntimePlugin) -> str:
    name = getattr(plugin, "name", None)
    if isinstance(name, str) and name:
        return name
    return type(plugin).__name__
