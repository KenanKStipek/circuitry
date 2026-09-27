"""awk tool plugin. Pass-through to the ``awk`` binary.

Args via ``params['args']`` (program + input file paths).  Pipe stdin
via ``params['stdin']`` instead of a path argument.
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
        name="awk",
        binary_candidates=("awk", "gawk", "mawk"),
        binary=plugin_binary_override(cfg),
        env=plugin_env_override(cfg),
    )
