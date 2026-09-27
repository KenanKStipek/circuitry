"""Ping tool plugin. Pass-through to the ``ping`` binary.

Args via ``params['args']`` (e.g. ``["-c", "4", "8.8.8.8"]``). Set
``params['allow_nonzero']=True`` so unreachable hosts don't raise
RuntimeError — exit_code lets the orchestration route on the result.
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
        name="ping",
        binary_candidates=("ping",),
        binary=plugin_binary_override(cfg),
        env=plugin_env_override(cfg, plugin_name="ping"),
    )
