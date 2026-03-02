from __future__ import annotations

from typing import Any

from .base import ToolPlugin
from .comfyui import ComfyUIPlugin
from .ffmpeg import FfmpegPlugin

SUPPORTED_PLUGINS = ("ffmpeg", "comfyui")


def build_plugin(*, plugin_name: str, runtime: dict[str, Any]) -> ToolPlugin:
    """
    Build a plugin instance from configuration.

    Supported plugins:
      - ffmpeg: Local ffmpeg binary for video/audio processing
      - comfyui: ComfyUI REST API for image generation

    Config is read from runtime.plugins.<plugin_name>
    """
    plugin_name = (plugin_name or "").strip().lower()
    plugins_cfg = (runtime or {}).get("plugins") or {}

    if plugin_name == "ffmpeg":
        return FfmpegPlugin()

    if plugin_name == "comfyui":
        cfg = plugins_cfg.get("comfyui") or {}
        return ComfyUIPlugin(
            base_url=cfg.get("base_url") or "http://localhost:8188",
            default_model=cfg.get("default_model") or "",
            default_image_output=cfg.get("default_image_output") or "path",
            image_dir=cfg.get("image_dir") or "./output/images",
            poll_interval=float(cfg.get("poll_interval") or 2.0),
        )

    supported = ", ".join(SUPPORTED_PLUGINS)
    raise ValueError(
        f"Unknown plugin: {plugin_name!r}. Supported plugins: {supported}. "
        "Check the 'provider' field on your tool effect."
    )
