"""OCR tool plugin. Pass-through to the ``tesseract`` binary.

Args via ``params['args']``. The classic invocation is
``["image.png", "-", "-l", "eng"]`` to OCR an image and print to stdout
in English.
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
        name="ocr",
        binary_candidates=("tesseract",),
        binary=plugin_binary_override(cfg),
        env=plugin_env_override(cfg, plugin_name="ocr"),
    )
