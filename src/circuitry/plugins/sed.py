"""sed tool plugin. Pass-through to the ``sed`` binary.

Args via ``params['args']`` (sed flags + script + input file paths).
Pipe stdin via ``params['stdin']``.
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(name="sed", binary_candidates=("sed", "gsed"))
