"""GitHub CLI tool plugin. Pass-through to the ``gh`` binary.

Args via ``params['args']`` (e.g. ``["pr", "list", "--json", "number,title"]``).
Authentication is handled by ``gh auth login`` outside of circuitry.
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
        name="gh",
        binary_candidates=("gh",),
        binary=plugin_binary_override(cfg),
        env=plugin_env_override(cfg),
    )
