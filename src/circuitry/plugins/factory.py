from __future__ import annotations

from typing import Any, Callable

from .base import ToolPlugin
from .base64 import Base64Plugin
from .clock import ClockPlugin
from .comfyui import ComfyUIPlugin
from .csv import CsvPlugin
from .email_smtp import EmailSmtpPlugin
from .env_vars import EnvVarsPlugin
from .ffmpeg import FfmpegPlugin
from .fs import FsPlugin
from .gzip import GzipPlugin
from .hash import HashPlugin
from .hex import HexPlugin
from .http import HttpPlugin
from .json import JsonPlugin
from .math import MathPlugin
from .port_check import PortCheckPlugin
from .regex import RegexPlugin
from .tar import TarPlugin
from .uuid import UuidPlugin
from .zip import ZipPlugin

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
    del cfg
    return HttpPlugin()


# Stdlib-only tool plugins — none of these consume per-plugin config
# from runtime.plugins.<name>; behaviour is fully controlled per-effect
# via params. Builders ignore cfg.
def _build_clock(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg; return ClockPlugin()


def _build_math(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg; return MathPlugin()


def _build_regex(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg; return RegexPlugin()


def _build_json(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg; return JsonPlugin()


def _build_fs(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg; return FsPlugin()


def _build_csv(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg; return CsvPlugin()


def _build_email_smtp(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg; return EmailSmtpPlugin()


def _build_tar(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg; return TarPlugin()


def _build_zip(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg; return ZipPlugin()


def _build_gzip(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg; return GzipPlugin()


def _build_port_check(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg; return PortCheckPlugin()


def _build_env_vars(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg; return EnvVarsPlugin()


def _build_hash(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg; return HashPlugin()


def _build_base64(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg; return Base64Plugin()


def _build_hex(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg; return HexPlugin()


def _build_uuid(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg; return UuidPlugin()


PLUGIN_REGISTRY: dict[str, PluginBuilder] = {
    "ffmpeg": _build_ffmpeg,
    "comfyui": _build_comfyui,
    "http": _build_http,
    # Stdlib-only catalog
    "clock": _build_clock,
    "math": _build_math,
    "regex": _build_regex,
    "json": _build_json,
    "fs": _build_fs,
    "csv": _build_csv,
    "email_smtp": _build_email_smtp,
    "tar": _build_tar,
    "zip": _build_zip,
    "gzip": _build_gzip,
    "port_check": _build_port_check,
    "env_vars": _build_env_vars,
    "hash": _build_hash,
    "base64": _build_base64,
    "hex": _build_hex,
    "uuid": _build_uuid,
}


def _supported_names() -> tuple[str, ...]:
    return tuple(sorted(PLUGIN_REGISTRY.keys()))


# Back-compat alias.
SUPPORTED_PLUGINS = tuple(_supported_names())


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
