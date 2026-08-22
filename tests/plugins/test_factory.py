from __future__ import annotations

import pytest

from circuitry.plugins.comfyui import ComfyUIPlugin
from circuitry.plugins.factory import build_plugin
from circuitry.plugins.ffmpeg import FfmpegPlugin

# ---------------------------------------------------------------------------
# Unknown plugin name
# ---------------------------------------------------------------------------


def test_unknown_plugin_raises_value_error() -> None:
    with pytest.raises(ValueError, match=r"Unknown plugin.*'nope'"):
        build_plugin(plugin_name="nope", runtime={})


def test_empty_plugin_name_raises_value_error() -> None:
    with pytest.raises(ValueError, match="Unknown plugin"):
        build_plugin(plugin_name="", runtime={})


# ---------------------------------------------------------------------------
# Each supported name returns correct type
# ---------------------------------------------------------------------------


def test_build_ffmpeg_returns_ffmpeg_plugin() -> None:
    plugin = build_plugin(plugin_name="ffmpeg", runtime={})
    assert isinstance(plugin, FfmpegPlugin)
    assert plugin.name == "ffmpeg"


def test_build_comfyui_returns_comfyui_plugin() -> None:
    plugin = build_plugin(plugin_name="comfyui", runtime={})
    assert isinstance(plugin, ComfyUIPlugin)
    assert plugin.name == "comfyui"


# ---------------------------------------------------------------------------
# Config passthrough to ComfyUI
# ---------------------------------------------------------------------------


def test_comfyui_config_passthrough() -> None:
    runtime = {
        "plugins": {
            "comfyui": {
                "base_url": "http://gpu-box:8188",
                "default_model": "flux1-dev-fp8.safetensors",
                "default_image_output": "base64",
                "image_dir": "/tmp/images",
                "poll_interval": 5.0,
            }
        }
    }
    plugin = build_plugin(plugin_name="comfyui", runtime=runtime)
    assert isinstance(plugin, ComfyUIPlugin)
    assert plugin.base_url == "http://gpu-box:8188"
    assert plugin.default_model == "flux1-dev-fp8.safetensors"
    assert plugin.default_image_output == "base64"
    assert plugin.image_dir == "/tmp/images"
    assert plugin.poll_interval == 5.0


def test_comfyui_defaults_when_no_config() -> None:
    plugin = build_plugin(plugin_name="comfyui", runtime={})
    assert isinstance(plugin, ComfyUIPlugin)
    assert plugin.base_url == "http://localhost:8188"
    assert plugin.poll_interval == 2.0


# ---------------------------------------------------------------------------
# Name normalization (whitespace, case)
# ---------------------------------------------------------------------------


def test_plugin_name_is_case_insensitive() -> None:
    plugin = build_plugin(plugin_name="FFmpeg", runtime={})
    assert isinstance(plugin, FfmpegPlugin)


def test_plugin_name_strips_whitespace() -> None:
    plugin = build_plugin(plugin_name="  comfyui  ", runtime={})
    assert isinstance(plugin, ComfyUIPlugin)
