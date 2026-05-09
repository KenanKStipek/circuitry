"""Ping tool plugin. Pass-through to the ``ping`` binary.

Args via ``params['args']`` (e.g. ``["-c", "4", "8.8.8.8"]``). Set
``params['allow_nonzero']=True`` so unreachable hosts don't raise
RuntimeError — exit_code lets the orchestration route on the result.
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(name="ping", binary_candidates=("ping",))
