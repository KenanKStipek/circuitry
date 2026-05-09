"""MediaInfo tool plugin. Pass-through to the ``mediainfo`` binary.

Reports metadata for audio/video files. Args via ``params['args']``:
the file path plus any output-format flags
(e.g. ``["--Output=JSON", "video.mp4"]``).
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(
        name="mediainfo", binary_candidates=("mediainfo",)
    )
