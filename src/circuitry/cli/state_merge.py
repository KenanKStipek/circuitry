"""Merge inline ``-e`` overrides into a loaded ``--state`` file.

Shared by `cof run`, `cof run-library`, and the TUI's Runs-view replay
(`cli.app`, `cli.app` again, and `tui.runs_view` respectively) so the three
surfaces can never drift into different merge semantics (#265 part 3).
"""

from __future__ import annotations

from typing import Any

__all__ = ["apply_inline_overrides"]


def apply_inline_overrides(
    loaded_state: dict[str, Any], inline: dict[str, Any]
) -> dict[str, Any]:
    """Merge -e overrides into a loaded --state file, `-e` wins.

    `-e` values are caller inputs by definition. If the loaded file is
    already namespaced (e.g. a prior --out snapshot), `migrate_legacy_state`
    short-circuits on its existing `input` key and never looks at the root
    again, so the overrides must land under `input` here rather than at the
    root or they'd be unreachable via `{{input.<key>}}`. A file that isn't
    namespaced yet is left to the usual root-level lift.
    """
    existing_input = loaded_state.get("input")
    if isinstance(existing_input, dict):
        existing_input.update(inline)
    else:
        loaded_state.update(inline)
    return loaded_state
