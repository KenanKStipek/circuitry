"""ImageMagick tool plugin. Pass-through to ``magick`` (preferred,
ImageMagick 7+) with fallback to ``convert`` (ImageMagick 6).

Args via ``params['args']``. ImageMagick 7 expects a subcommand as
first argument (``["convert", "in.png", "-resize", "50%", "out.png"]``)
when invoked as ``magick``; ImageMagick 6's ``convert`` takes the same
flags but without the leading subcommand. The orchestration author
should match its args to whichever binary is on PATH.
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(
        name="imagemagick", binary_candidates=("magick", "convert")
    )
