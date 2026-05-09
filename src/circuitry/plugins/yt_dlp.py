"""yt-dlp tool plugin. Pass-through to the ``yt-dlp`` binary.

Args via ``params['args']``. Common patterns:
  - ``["-f", "bestaudio", "-o", "out.%(ext)s", "URL"]``
  - ``["--dump-json", "URL"]`` — metadata only.
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(name="yt_dlp", binary_candidates=("yt-dlp",))
