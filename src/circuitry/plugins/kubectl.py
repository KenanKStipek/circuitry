"""kubectl CLI tool plugin. Pass-through to the ``kubectl`` binary.

Args via ``params['args']``. Use ``params['env']`` (via the future
sandboxed-shell wrapper if needed) for ``KUBECONFIG`` overrides.
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(name="kubectl", binary_candidates=("kubectl",))
