"""Pytest CLI tool plugin. Pass-through to the ``pytest`` binary.

Args via ``params['args']``: paths + pytest flags. Pytest typically
returns 0 (passed), 1 (failed), 2 (interrupted), 3 (internal error),
4 (usage error), 5 (no tests collected). Set
``params['allow_nonzero']=True`` so the orchestration sees the exit
code rather than getting a RuntimeError on test failure.
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(name="pytest", binary_candidates=("pytest",))
