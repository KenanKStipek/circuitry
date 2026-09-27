"""Traceroute tool plugin. Pass-through to ``traceroute`` (Linux/macOS)
or ``tracert`` (Windows).

Args via ``params['args']`` (e.g. ``["-n", "8.8.8.8"]``).
"""

from __future__ import annotations

from typing import Any

from ._subprocess import (
    GenericSubprocessTool,
    plugin_binary_override,
    plugin_env_override,
)


def make_plugin(cfg: dict[str, Any] | None = None) -> GenericSubprocessTool:
    cfg = cfg or {}
    return GenericSubprocessTool(
        name="traceroute",
        binary_candidates=("traceroute", "tracert"),
        binary=plugin_binary_override(cfg),
        env=plugin_env_override(cfg),
    )
