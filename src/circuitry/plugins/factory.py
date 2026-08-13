from __future__ import annotations

from collections.abc import Callable
from typing import Any

from . import (
    awk as _awk_mod,
)
from . import (
    diff_patch as _diff_patch_mod,
)
from . import (
    docker as _docker_mod,
)
from . import (
    exiftool as _exiftool_mod,
)
from . import (
    gh as _gh_mod,
)
from . import (
    git as _git_mod,
)
from . import (
    gpg as _gpg_mod,
)
from . import (
    imagemagick as _imagemagick_mod,
)
from . import (
    kubectl as _kubectl_mod,
)
from . import (
    linter as _linter_mod,
)
from . import (
    mediainfo as _mediainfo_mod,
)
from . import (
    ocr as _ocr_mod,
)
from . import (
    pandoc as _pandoc_mod,
)
from . import (
    pdf_render as _pdf_render_mod,
)
from . import (
    ping as _ping_mod,
)
from . import (
    pytest as _pytest_mod,
)
from . import (
    ripgrep as _ripgrep_mod,
)
from . import (
    sed as _sed_mod,
)
from . import (
    sevenz as _sevenz_mod,
)
from . import (
    traceroute as _traceroute_mod,
)
from . import (
    weather as _weather_mod,
)
from . import (
    web_search as _web_search_mod,
)
from . import (
    yt_dlp as _yt_dlp_mod,
)
from .base import ToolPlugin
from .base64 import Base64Plugin
from .clock import ClockPlugin
from .comfyui import ComfyUIPlugin
from .csv import CsvPlugin
from .discord import DiscordPlugin
from .dns import DnsPlugin
from .email_smtp import EmailSmtpPlugin
from .embed import EmbedPlugin
from .env_vars import EnvVarsPlugin
from .ffmpeg import FfmpegPlugin
from .fs import FsPlugin
from .gcalendar import GCalendarPlugin
from .gdrive import GDrivePlugin
from .github import GitHubPlugin
from .gzip import GzipPlugin
from .hash import HashPlugin
from .hex import HexPlugin
from .html_extract import HtmlExtractPlugin
from .http import HttpPlugin
from .jira import JiraPlugin
from .json import JsonPlugin
from .linear import LinearPlugin
from .math import MathPlugin
from .mcp_client import McpPlugin
from .notion import NotionPlugin
from .pdf_extract import PdfExtractPlugin
from .playwright import PlaywrightPlugin
from .port_check import PortCheckPlugin
from .process_list import ProcessListPlugin
from .python_eval import PythonEvalPlugin
from .regex import RegexPlugin
from .rerank import RerankPlugin
from .rss import RssPlugin
from .s3_tool import S3ToolPlugin
from .screenshot import ScreenshotPlugin
from .shell import ShellPlugin
from .slack import SlackPlugin
from .system_info import SystemInfoPlugin
from .tar import TarPlugin
from .uuid import UuidPlugin
from .validate_yaml import ValidateYamlPlugin
from .vector_search import VectorSearchPlugin
from .web_fetch import WebFetchPlugin
from .webhook import WebhookPlugin
from .whois import WhoisPlugin
from .wikipedia import WikipediaPlugin
from .xml import XmlPlugin
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
    del cfg
    return ClockPlugin()


def _build_math(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return MathPlugin()


def _build_regex(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return RegexPlugin()


def _build_json(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return JsonPlugin()


def _build_fs(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return FsPlugin()


def _build_csv(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return CsvPlugin()


def _build_email_smtp(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return EmailSmtpPlugin()


def _build_tar(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return TarPlugin()


def _build_zip(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return ZipPlugin()


def _build_gzip(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return GzipPlugin()


def _build_port_check(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return PortCheckPlugin()


def _build_env_vars(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return EnvVarsPlugin()


def _build_hash(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return HashPlugin()


def _build_base64(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return Base64Plugin()


def _build_hex(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return HexPlugin()


def _build_uuid(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return UuidPlugin()


def _build_validate_yaml(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return ValidateYamlPlugin()


# Subprocess wrapper plugins. Each module exposes ``make_plugin()`` that
# returns a configured GenericSubprocessTool (or a dedicated class for
# the multi-mode / sandboxed cases).
def _build_git(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _git_mod.make_plugin()


def _build_ripgrep(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _ripgrep_mod.make_plugin()


def _build_pytest(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _pytest_mod.make_plugin()


def _build_awk(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _awk_mod.make_plugin()


def _build_sed(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _sed_mod.make_plugin()


def _build_pandoc(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _pandoc_mod.make_plugin()


def _build_mediainfo(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _mediainfo_mod.make_plugin()


def _build_imagemagick(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _imagemagick_mod.make_plugin()


def _build_exiftool(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _exiftool_mod.make_plugin()


def _build_yt_dlp(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _yt_dlp_mod.make_plugin()


def _build_sevenz(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _sevenz_mod.make_plugin()


def _build_ping(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _ping_mod.make_plugin()


def _build_traceroute(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _traceroute_mod.make_plugin()


def _build_docker(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _docker_mod.make_plugin()


def _build_kubectl(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _kubectl_mod.make_plugin()


def _build_gh(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _gh_mod.make_plugin()


def _build_linter(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _linter_mod.make_plugin()


def _build_ocr(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _ocr_mod.make_plugin()


def _build_shell(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return ShellPlugin()


def _build_gpg(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _gpg_mod.GpgPlugin()


def _build_diff_patch(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _diff_patch_mod.DiffPatchPlugin()


def _build_pdf_render(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _pdf_render_mod.PdfRenderPlugin()


def _build_web_search(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _web_search_mod.WebSearchPlugin()


def _build_weather(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return _weather_mod.WeatherPlugin()


# PyPI-dep tool plugins. Lazy imports inside execute() — instantiation
# never fails, so missing deps surface only when actually invoked or
# when check() is called.
def _build_dns(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return DnsPlugin()


def _build_whois(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return WhoisPlugin()


def _build_pdf_extract(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return PdfExtractPlugin()


def _build_xml(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return XmlPlugin()


def _build_html_extract(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return HtmlExtractPlugin()


def _build_system_info(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return SystemInfoPlugin()


def _build_process_list(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return ProcessListPlugin()


def _build_wikipedia(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return WikipediaPlugin()


def _build_rss(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return RssPlugin()


def _build_webhook(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return WebhookPlugin()


def _build_web_fetch(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return WebFetchPlugin()


def _build_python_eval(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return PythonEvalPlugin()


# SDK / cloud / browser / ML tool plugins. Lazy imports inside execute()
# — the dataclass instantiation never fails, the dep error surfaces
# at invocation or via check().
def _build_linear(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return LinearPlugin()


def _build_slack(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return SlackPlugin()


def _build_discord(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return DiscordPlugin()


def _build_github(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return GitHubPlugin()


def _build_jira(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return JiraPlugin()


def _build_notion(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return NotionPlugin()


def _build_gcalendar(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return GCalendarPlugin()


def _build_gdrive(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return GDrivePlugin()


def _build_s3_tool(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return S3ToolPlugin()


def _build_playwright(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return PlaywrightPlugin()


def _build_screenshot(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return ScreenshotPlugin()


def _build_embed(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return EmbedPlugin()


def _build_rerank(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return RerankPlugin()


def _build_vector_search(cfg: dict[str, Any]) -> ToolPlugin:
    del cfg
    return VectorSearchPlugin()


# MCP client — bridges orchestrations to external Model Context Protocol
# servers declared under runtime.plugins.mcp.servers. The only builder
# besides comfyui that consumes per-plugin config.
def _build_mcp(cfg: dict[str, Any]) -> ToolPlugin:
    return McpPlugin(servers=dict(cfg.get("servers") or {}))


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
    "validate_yaml": _build_validate_yaml,
    # Subprocess wrappers (binary on PATH).
    "git": _build_git,
    "ripgrep": _build_ripgrep,
    "pytest": _build_pytest,
    "awk": _build_awk,
    "sed": _build_sed,
    "pandoc": _build_pandoc,
    "mediainfo": _build_mediainfo,
    "imagemagick": _build_imagemagick,
    "exiftool": _build_exiftool,
    "yt_dlp": _build_yt_dlp,
    "7z": _build_sevenz,
    "ping": _build_ping,
    "traceroute": _build_traceroute,
    "docker": _build_docker,
    "kubectl": _build_kubectl,
    "gh": _build_gh,
    "linter": _build_linter,
    "ocr": _build_ocr,
    "shell": _build_shell,
    "gpg": _build_gpg,
    "diff_patch": _build_diff_patch,
    "pdf_render": _build_pdf_render,
    "web_search": _build_web_search,
    "weather": _build_weather,
    # Pure-Python PyPI-dep catalog.
    "dns": _build_dns,
    "whois": _build_whois,
    "pdf_extract": _build_pdf_extract,
    "xml": _build_xml,
    "html_extract": _build_html_extract,
    "system_info": _build_system_info,
    "process_list": _build_process_list,
    "wikipedia": _build_wikipedia,
    "rss": _build_rss,
    "webhook": _build_webhook,
    "web_fetch": _build_web_fetch,
    "python_eval": _build_python_eval,
    # SDK / cloud / browser / ML.
    "linear": _build_linear,
    "slack": _build_slack,
    "discord": _build_discord,
    "github": _build_github,
    "jira": _build_jira,
    "notion": _build_notion,
    "gcalendar": _build_gcalendar,
    "gdrive": _build_gdrive,
    "s3": _build_s3_tool,
    "playwright": _build_playwright,
    "screenshot": _build_screenshot,
    "embed": _build_embed,
    "rerank": _build_rerank,
    "vector_search": _build_vector_search,
    # MCP client.
    "mcp": _build_mcp,
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
