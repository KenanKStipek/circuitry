"""Ripgrep tool plugin. Pass-through to the ``rg`` binary.

Args via ``params['args']``: pattern + paths + flags
(e.g. ``["TODO", "--type=py", "src/"]``).
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(name="ripgrep", binary_candidates=("rg",))
