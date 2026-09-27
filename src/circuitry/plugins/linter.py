"""Generic linter tool plugin. Tries ``ruff`` (Python) then ``eslint``
(JS/TS); falls back to whichever is on PATH.

Args via ``params['args']``. The plugin doesn't impose a specific
linter contract — it just runs whatever's installed and forwards the
arguments. Set ``params['allow_nonzero']=True`` so the plugin returns
the exit code instead of raising on lint failures.

For projects with a specific linter requirement, prefer running the
binary directly via the shell plugin or a dedicated wrapper.
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
        name="linter",
        binary_candidates=("ruff", "eslint"),
        binary=plugin_binary_override(cfg),
        env=plugin_env_override(cfg),
    )
