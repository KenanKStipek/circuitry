"""MediaInfo tool plugin. Pass-through to the ``mediainfo`` binary.

Reports metadata for audio/video files. Args via ``params['args']``:
the file path plus any output-format flags
(e.g. ``["--Output=JSON", "video.mp4"]``).
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
        name="mediainfo",
        binary_candidates=("mediainfo",),
        binary=plugin_binary_override(cfg),
        env=plugin_env_override(cfg, plugin_name="mediainfo"),
    )
