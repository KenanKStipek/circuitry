"""Git CLI tool plugin. Pass-through to the ``git`` binary.

Pass the subcommand and arguments via ``params['args']`` (e.g.
``["log", "--oneline", "-10"]``). Working directory via ``params['cwd']``.
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(name="git", binary_candidates=("git",))
