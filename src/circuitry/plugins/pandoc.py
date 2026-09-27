"""Pandoc tool plugin. Pass-through to the ``pandoc`` binary.

Args via ``params['args']`` (input + ``-o output`` + format flags).
Pipe stdin via ``params['stdin']`` to convert text without writing
intermediate files.
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
        name="pandoc",
        binary_candidates=("pandoc",),
        binary=plugin_binary_override(cfg),
        env=plugin_env_override(cfg),
    )
