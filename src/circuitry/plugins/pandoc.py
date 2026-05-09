"""Pandoc tool plugin. Pass-through to the ``pandoc`` binary.

Args via ``params['args']`` (input + ``-o output`` + format flags).
Pipe stdin via ``params['stdin']`` to convert text without writing
intermediate files.
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(name="pandoc", binary_candidates=("pandoc",))
