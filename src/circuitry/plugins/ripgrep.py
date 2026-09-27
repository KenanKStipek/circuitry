"""Ripgrep tool plugin. Pass-through to the ``rg`` binary.

Args via ``params['args']``: pattern + paths + flags
(e.g. ``["TODO", "--type=py", "src/"]``).
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
        name="ripgrep",
        binary_candidates=("rg",),
        binary=plugin_binary_override(cfg),
        env=plugin_env_override(cfg, plugin_name="ripgrep"),
    )
