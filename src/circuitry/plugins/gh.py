"""GitHub CLI tool plugin. Pass-through to the ``gh`` binary.

Args via ``params['args']`` (e.g. ``["pr", "list", "--json", "number,title"]``).
Authentication is handled by ``gh auth login`` outside of circuitry.
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(name="gh", binary_candidates=("gh",))
