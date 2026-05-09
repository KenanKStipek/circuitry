"""Traceroute tool plugin. Pass-through to ``traceroute`` (Linux/macOS)
or ``tracert`` (Windows).

Args via ``params['args']`` (e.g. ``["-n", "8.8.8.8"]``).
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(
        name="traceroute", binary_candidates=("traceroute", "tracert")
    )
