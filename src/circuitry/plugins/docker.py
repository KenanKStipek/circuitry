"""Docker CLI tool plugin. Pass-through to the ``docker`` binary.

Args via ``params['args']`` (e.g. ``["ps", "--format", "json"]``).
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
        name="docker",
        binary_candidates=("docker",),
        binary=plugin_binary_override(cfg),
        env=plugin_env_override(cfg),
    )
