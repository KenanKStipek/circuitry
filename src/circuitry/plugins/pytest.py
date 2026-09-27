"""Pytest CLI tool plugin. Pass-through to the ``pytest`` binary.

Args via ``params['args']``: paths + pytest flags. Pytest typically
returns 0 (passed), 1 (failed), 2 (interrupted), 3 (internal error),
4 (usage error), 5 (no tests collected). Set
``params['allow_nonzero']=True`` so the orchestration sees the exit
code rather than getting a RuntimeError on test failure.
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
        name="pytest",
        binary_candidates=("pytest",),
        binary=plugin_binary_override(cfg),
        env=plugin_env_override(cfg),
    )
