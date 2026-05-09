from __future__ import annotations

from typing import Any, Callable

from .base import ToolPlugin
from .comfyui import ComfyUIPlugin
from .ffmpeg import FfmpegPlugin
from .http import HttpPlugin

PluginBuilder = Callable[[dict[str, Any]], ToolPlugin]


def _build_ffmpeg(cfg: dict[str, Any]) -> ToolPlugin:
    return FfmpegPlugin()


def _build_comfyui(cfg: dict[str, Any]) -> ToolPlugin:
    return ComfyUIPlugin(
        base_url=cfg.get("base_url") or "http://localhost:8188",
        default_model=cfg.get("default_model") or "",
        default_image_output=cfg.get("default_image_output") or "path",
        image_dir=cfg.get("image_dir") or "./output/images",
        poll_interval=float(cfg.get("poll_interval") or 2.0),
    )


def _build_http(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg  # http has no per-plugin config; all options come per-effect via params
    return HttpPlugin()


PLUGIN_REGISTRY: dict[str, PluginBuilder] = {
    "ffmpeg": _build_ffmpeg,
    "comfyui": _build_comfyui,
    "http": _build_http,
}


def _supported_names() -> tuple[str, ...]:
    return tuple(sorted(PLUGIN_REGISTRY.keys()))


# Back-compat alias.
SUPPORTED_PLUGINS = ("ffmpeg", "comfyui", "http")


def build_plugin(*, plugin_name: str, runtime: dict[str, Any]) -> ToolPlugin:
    """
    Build a plugin instance from configuration.

    Plugins register themselves in PLUGIN_REGISTRY: a dict from canonical
    lower-case name to a builder callable that takes the per-plugin config
    dict (read from runtime.plugins.<plugin_name>) and returns a ToolPlugin.
    """
    plugin_name = (plugin_name or "").strip().lower()
    plugins_cfg = (runtime or {}).get("plugins") or {}

    builder = PLUGIN_REGISTRY.get(plugin_name)
    if builder is None:
        supported = ", ".join(_supported_names())
        raise ValueError(
            f"Unknown plugin: {plugin_name!r}. Supported plugins: {supported}. "
            "Check the 'provider' field on your tool effect."
        )

    cfg = plugins_cfg.get(plugin_name) or {}
    return builder(cfg)
