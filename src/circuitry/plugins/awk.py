"""awk tool plugin. Pass-through to the ``awk`` binary.

Args via ``params['args']`` (program + input file paths).  Pipe stdin
via ``params['stdin']`` instead of a path argument.
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(name="awk", binary_candidates=("awk", "gawk", "mawk"))
