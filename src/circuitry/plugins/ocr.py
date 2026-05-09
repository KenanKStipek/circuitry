"""OCR tool plugin. Pass-through to the ``tesseract`` binary.

Args via ``params['args']``. The classic invocation is
``["image.png", "-", "-l", "eng"]`` to OCR an image and print to stdout
in English.
"""

from __future__ import annotations

from ._subprocess import GenericSubprocessTool


def make_plugin() -> GenericSubprocessTool:
    return GenericSubprocessTool(name="ocr", binary_candidates=("tesseract",))
