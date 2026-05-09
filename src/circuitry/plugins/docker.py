"""Docker CLI tool plugin. Pass-through to the ``docker`` binary.

Args via ``params['args']`` (e.g. ``["ps", "--format", "json"]``).
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(name="docker", binary_candidates=("docker",))
