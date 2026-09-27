"""Git CLI tool plugin. Pass-through to the ``git`` binary.

Pass the subcommand and arguments via ``params['args']`` (e.g.
``["log", "--oneline", "-10"]``). Working directory via ``params['cwd']``.
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
        name="git",
        binary_candidates=("git",),
        binary=plugin_binary_override(cfg),
        env=plugin_env_override(cfg),
    )
