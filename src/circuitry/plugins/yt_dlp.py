"""yt-dlp tool plugin. Pass-through to the ``yt-dlp`` binary.

Args via ``params['args']``. Common patterns:
  - ``["-f", "bestaudio", "-o", "out.%(ext)s", "URL"]``
  - ``["--dump-json", "URL"]`` — metadata only.
"""

from __future__ import annotations

from typing import Any

from ._subprocess import (
    GenericSubprocessTool,
    plugin_binary_override,
    plugin_env_override,
)


def make_plugin(cfg: dict[str, Any] | None = None) -> GenericSubprocessTool:
    cfg = cfg or {}
    return GenericSubprocessTool(
        name="yt_dlp",
        binary_candidates=("yt-dlp",),
        binary=plugin_binary_override(cfg),
        env=plugin_env_override(cfg, plugin_name="yt_dlp"),
    )
