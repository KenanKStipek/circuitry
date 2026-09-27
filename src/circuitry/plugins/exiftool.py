"""ExifTool tool plugin. Pass-through to the ``exiftool`` binary.

Args via ``params['args']``. Common patterns:
  - ``["-json", "image.jpg"]`` — extract metadata as JSON.
  - ``["-Comment=hi", "image.jpg"]`` — write a tag.
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
        name="exiftool",
        binary_candidates=("exiftool",),
        binary=plugin_binary_override(cfg),
        env=plugin_env_override(cfg, plugin_name="exiftool"),
    )
