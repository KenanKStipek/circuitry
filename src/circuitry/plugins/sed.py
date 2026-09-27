"""sed tool plugin. Pass-through to the ``sed`` binary.

Args via ``params['args']`` (sed flags + script + input file paths).
Pipe stdin via ``params['stdin']``.
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
        name="sed",
        binary_candidates=("sed", "gsed"),
        binary=plugin_binary_override(cfg),
        env=plugin_env_override(cfg, plugin_name="sed"),
    )
