"""kubectl CLI tool plugin. Pass-through to the ``kubectl`` binary.

Args via ``params['args']``. Use ``params['env']`` (via the future
sandboxed-shell wrapper if needed) for ``KUBECONFIG`` overrides.
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
        name="kubectl",
        binary_candidates=("kubectl",),
        binary=plugin_binary_override(cfg),
        env=plugin_env_override(cfg),
    )
