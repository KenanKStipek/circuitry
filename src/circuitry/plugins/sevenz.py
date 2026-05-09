"""7-Zip tool plugin (registered as ``7z``). Pass-through to the ``7z``
or ``7za`` binary.

Args via ``params['args']``: action + archive + paths
(e.g. ``["a", "out.7z", "src/"]`` to create, ``["x", "out.7z"]`` to
extract).
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(name="7z", binary_candidates=("7z", "7za", "7zz"))
