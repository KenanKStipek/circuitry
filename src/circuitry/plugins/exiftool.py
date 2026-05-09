"""ExifTool tool plugin. Pass-through to the ``exiftool`` binary.

Args via ``params['args']``. Common patterns:
  - ``["-json", "image.jpg"]`` — extract metadata as JSON.
  - ``["-Comment=hi", "image.jpg"]`` — write a tag.
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(name="exiftool", binary_candidates=("exiftool",))
