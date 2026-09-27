from __future__ import annotations

import copy
import json
import logging
import os
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Any, Literal

Environment = Literal["dev", "prod", "test"]
_VALID_ENVIRONMENTS: tuple[str, ...] = ("dev", "prod", "test")

logger = logging.getLogger(__name__)


DEFAULT_CONFIG_FILENAMES = ("circuitry.config.json", "config.json")


class ConfigError(ValueError):
    """A config file the user pointed at cannot be used.

    Message text is user-facing: CLI commands print it verbatim after
    ``Error:`` and exit 1, so it must read as a complete sentence.

    Subclasses ``ValueError`` on purpose — the auto-discovery layers in
    :func:`resolve_config` already catch ``ValueError`` and degrade to a
    warning, so a broken *discovered* config keeps its softer behaviour
    while an explicitly requested one is fatal.
    """

GLOBAL_CONFIG_DIR = Path.home() / ".config" / "circuitry"
GLOBAL_CONFIG_PATH = GLOBAL_CONFIG_DIR / "config.json"

SANE_DEFAULTS: dict[str, Any] = {
    "default_model": "llama3.1:8b",
    "default_adapter": "ollama",
    "enabled_adapters": None,
    "enabled_plugins": None,
    "enabled_tools": None,
    "environment": "dev",
    "runtime": {
        "adapters": {
            "ollama": {
                "base_url": "http://localhost:11434",
            },
        },
        "plugins": {
            "comfyui": {
                "base_url": "http://localhost:8188",
            },
        },
    },
}


_ALLOWLIST_KEYS: tuple[str, ...] = ("enabled_adapters", "enabled_plugins", "enabled_tools")

#: Environment variables :func:`_apply_env_vars` overlays onto a resolved config.
CONFIG_ENV_VARS: tuple[str, ...] = (
    "CIRCUITRY_MODEL",
    "CIRCUITRY_ADAPTER",
    "CIRCUITRY_ADAPTER_URL",
    "CIRCUITRY_COMFYUI_URL",
    "CIRCUITRY_ENABLED_ADAPTERS",
    "CIRCUITRY_ENABLED_PLUGINS",
    "CIRCUITRY_ENABLED_TOOLS",
    "CIRCUITRY_ENVIRONMENT",
)

ConfigSourceKind = Literal["global", "project", "CIRCUITRY_CONFIG", "--config", "env"]


@dataclass(frozen=True)
class ConfigSource:
    """One layer :func:`resolve_config` applied: a config file or an env var."""

    kind: ConfigSourceKind
    #: The file path, or the environment variable's name for ``kind == "env"``.
    location: str

    def describe(self) -> str:
        return f"{self.location} ({self.kind})"


def describe_config_sources(sources: tuple[ConfigSource, ...]) -> str:
    """One line naming every layer a config came from, for run/check headers."""
    if not sources:
        return "— (built-in defaults)"
    return ", ".join(source.describe() for source in sources)


@dataclass(frozen=True)
class CircuitryConfig:
    """
    CLI-facing configuration. Keep this intentionally narrow and stable.
    Anything runtime-specific can live under `runtime` and pass through.
    """

    # Common defaults users will want
    default_model: str | None = None
    default_adapter: str | None = None

    # Plugins: keep as a list of dotted paths or simple identifiers
    plugins: list[str] = field(default_factory=list)

    # Allowlist gates per extension category. None = default-open (all
    # compiled-in extensions allowed). [] = locked down. ["x", "y"] = strict.
    enabled_adapters: list[str] | None = None
    enabled_plugins: list[str] | None = None  # RuntimePlugin allowlist
    enabled_tools: list[str] | None = None  # ToolPlugin allowlist

    # Deployment environment; controls store_raw default for SQL persistence.
    environment: Environment = "dev"

    # Any additional runtime config to pass through untouched
    runtime: dict[str, Any] = field(default_factory=dict)

    # Where this config came from, in the order resolve_config applied the
    # layers. Reporting only — excluded from equality.
    sources: tuple[ConfigSource, ...] = field(default=(), compare=False)

    @staticmethod
    def from_dict(d: dict[str, Any]) -> CircuitryConfig:
        env = d.get("environment") or "dev"
        if env not in _VALID_ENVIRONMENTS:
            logger.warning(
                "Unknown environment %r; falling back to 'dev'. Valid: %s",
                env, _VALID_ENVIRONMENTS,
            )
            env = "dev"
        return CircuitryConfig(
            default_model=d.get("default_model"),
            default_adapter=d.get("default_adapter"),
            plugins=list(d.get("plugins") or []),
            enabled_adapters=_normalize_allowlist(d.get("enabled_adapters")),
            enabled_plugins=_normalize_allowlist(d.get("enabled_plugins")),
            enabled_tools=_normalize_allowlist(d.get("enabled_tools")),
            environment=env,  # type: ignore[arg-type]
            runtime=dict(d.get("runtime") or {}),
        )


def _normalize_allowlist(value: Any) -> list[str] | None:
    """Coerce config-loaded allowlist values into Optional[list[str]].

    None → None (default-open).
    list → list[str] (filtered to truthy strings).
    Anything else → None (treated as unset).
    """
    if value is None:
        return None
    if isinstance(value, list):
        return [str(item).strip() for item in value if str(item).strip()]
    return None


def _deep_merge(base: dict[str, Any], overlay: dict[str, Any]) -> dict[str, Any]:
    """Deep-merge *overlay* onto *base*. Overlay keys win; nested dicts recurse."""
    merged = dict(base)
    for key, value in overlay.items():
        if (
            key in merged
            and isinstance(merged[key], dict)
            and isinstance(value, dict)
        ):
            merged[key] = _deep_merge(merged[key], value)
        else:
            merged[key] = value
    return merged


def _first_existing(paths: list[Path]) -> Path | None:
    for p in paths:
        if p.exists() and p.is_file():
            return p
    return None


_JSON_ROOT_NAMES = {
    list: "an array",
    str: "a string",
    bool: "a boolean",
    int: "a number",
    float: "a number",
    type(None): "null",
}


def _load_json_file(path: Path) -> dict[str, Any]:
    """Read *path* as a JSON object, raising :class:`ConfigError` on any problem."""
    try:
        text = path.read_text(encoding="utf-8")
    except FileNotFoundError as exc:
        raise ConfigError(f"Config file not found: {path}") from exc
    except IsADirectoryError as exc:
        raise ConfigError(f"Config path is a directory, not a file: {path}") from exc
    except OSError as exc:
        detail = exc.strerror or str(exc)
        raise ConfigError(f"Config file could not be read: {path} ({detail})") from exc
    except UnicodeDecodeError as exc:
        raise ConfigError(f"Config file is not valid UTF-8 text: {path}") from exc

    try:
        raw = json.loads(text)
    except json.JSONDecodeError as exc:
        raise ConfigError(
            f"Config file {path} is not valid JSON: "
            f"{exc.msg} (line {exc.lineno}, column {exc.colno})"
        ) from exc

    if not isinstance(raw, dict):
        found = _JSON_ROOT_NAMES.get(type(raw), "a non-object value")
        raise ConfigError(
            f"Config file {path} must contain a JSON object at the root; found {found}."
        )
    return raw


def find_config_path(
    *,
    explicit_path: Path | None,
    cwd: Path | None = None,
) -> Path | None:
    """
    Resolution order:
      1) explicit_path (if provided)
      2) env var CIRCUITRY_CONFIG (if set)
      3) cwd / default filenames
      4) global ~/.config/circuitry/config.json
    """
    if explicit_path:
        return explicit_path

    env = os.getenv("CIRCUITRY_CONFIG")
    if env:
        return Path(env)

    base = cwd or Path.cwd()
    candidates = [base / name for name in DEFAULT_CONFIG_FILENAMES]
    found = _first_existing(candidates)
    if found:
        return found

    if GLOBAL_CONFIG_PATH.exists() and GLOBAL_CONFIG_PATH.is_file():
        return GLOBAL_CONFIG_PATH

    return None


def load_config(path: Path | None) -> CircuitryConfig:
    if not path:
        return CircuitryConfig()

    return CircuitryConfig.from_dict(_load_json_file(path))


def _apply_env_vars(d: dict[str, Any]) -> dict[str, Any]:
    """Overlay environment variable overrides onto a config dict."""
    result = dict(d)

    env_model = os.getenv("CIRCUITRY_MODEL")
    if env_model:
        result["default_model"] = env_model

    env_adapter = os.getenv("CIRCUITRY_ADAPTER")
    if env_adapter:
        result["default_adapter"] = env_adapter

    env_url = os.getenv("CIRCUITRY_ADAPTER_URL")
    if env_url:
        adapter_name = result.get("default_adapter") or "ollama"
        runtime = dict(result.get("runtime") or {})
        adapters = dict(runtime.get("adapters") or {})
        adapter_cfg = dict(adapters.get(adapter_name) or {})
        adapter_cfg["base_url"] = env_url
        adapters[adapter_name] = adapter_cfg
        runtime["adapters"] = adapters
        result["runtime"] = runtime

    env_comfyui_url = os.getenv("CIRCUITRY_COMFYUI_URL")
    if env_comfyui_url:
        runtime = dict(result.get("runtime") or {})
        plugins = dict(runtime.get("plugins") or {})
        comfyui_cfg = dict(plugins.get("comfyui") or {})
        comfyui_cfg["base_url"] = env_comfyui_url
        plugins["comfyui"] = comfyui_cfg
        runtime["plugins"] = plugins
        result["runtime"] = runtime

    # Allowlist env vars override config.json values. Unset env var → leave
    # whatever the config layer produced. Set env var → overlay (including
    # empty-string lockdown).
    for env_key, cfg_key in (
        ("CIRCUITRY_ENABLED_ADAPTERS", "enabled_adapters"),
        ("CIRCUITRY_ENABLED_PLUGINS", "enabled_plugins"),
        ("CIRCUITRY_ENABLED_TOOLS", "enabled_tools"),
    ):
        raw = os.getenv(env_key)
        if raw is None:
            continue
        result[cfg_key] = [item.strip() for item in raw.split(",") if item.strip()]

    env_environment = os.getenv("CIRCUITRY_ENVIRONMENT")
    if env_environment:
        result["environment"] = env_environment

    return result


def _applied_env_vars() -> list[str]:
    """The CONFIG_ENV_VARS :func:`_apply_env_vars` actually overlays right now.

    An allowlist variable applies whenever it is set (empty means lockdown);
    the rest only when non-empty.
    """
    return [
        name
        for name in CONFIG_ENV_VARS
        if (
            os.getenv(name) is not None
            if name.startswith("CIRCUITRY_ENABLED_")
            else os.getenv(name)
        )
    ]


def _narrow_allowlists(
    merged: dict[str, Any],
    *,
    global_config: dict[str, Any],
    project_config: dict[str, Any],
    project_path: Path,
) -> dict[str, Any]:
    """Keep a discovered project config from widening a global allowlist.

    For each ``enabled_*`` list the global config set, the project may only
    narrow it: a project list is intersected with the global one, and a
    project ``null`` (or a missing key) leaves the global list in force.
    Without this, a cloned repo's ``circuitry.config.json`` could re-open
    everything the machine's owner locked down.
    """
    result = dict(merged)
    for key in _ALLOWLIST_KEYS:
        global_list = _normalize_allowlist(global_config.get(key))
        if global_list is None or key not in project_config:
            continue
        project_list = _normalize_allowlist(project_config.get(key))
        if project_list is None:
            logger.warning(
                "Project config %s cannot re-open %s; keeping the global list %s.",
                project_path, key, global_list,
            )
            result[key] = global_list
            continue
        widened = [name for name in project_list if name not in global_list]
        if widened:
            logger.warning(
                "Project config %s cannot widen %s beyond the global list; "
                "ignoring %s.",
                project_path, key, widened,
            )
        result[key] = [name for name in project_list if name in global_list]
    return result


def resolve_config(
    *,
    explicit_path: Path | None = None,
    cwd: Path | None = None,
) -> CircuitryConfig:
    """
    Build a fully-resolved CircuitryConfig by layering:

    1. Sane defaults (lowest priority)
    2. Global config (~/.config/circuitry/config.json)
    3. Project-local config (circuitry.config.json / config.json in cwd, or
       the file CIRCUITRY_CONFIG names)
    4. Explicit --config path (if provided — replaces #2 and #3)
    5. Environment variables (CIRCUITRY_MODEL, CIRCUITRY_ADAPTER,
       CIRCUITRY_ADAPTER_URL, CIRCUITRY_COMFYUI_URL, CIRCUITRY_ENABLED_*,
       CIRCUITRY_ENVIRONMENT)

    Each layer deep-merges onto the previous, with one exception: a project
    config *discovered* in cwd may narrow, but never widen, an allowlist the
    global config set (see :func:`_narrow_allowlists`). A file the caller
    named — ``--config`` or ``CIRCUITRY_CONFIG`` — is trusted as given.

    The layers actually applied are recorded on the result's ``sources``.
    """
    merged = copy.deepcopy(SANE_DEFAULTS)
    sources: list[ConfigSource] = []

    if explicit_path:
        # Explicit path skips global/project discovery — use only that file
        file_config = _load_json_file(explicit_path)
        merged = _deep_merge(merged, file_config)
        sources.append(ConfigSource("--config", str(explicit_path)))
    else:
        # Layer global config
        global_config: dict[str, Any] = {}
        if GLOBAL_CONFIG_PATH.exists() and GLOBAL_CONFIG_PATH.is_file():
            try:
                global_config = _load_json_file(GLOBAL_CONFIG_PATH)
                merged = _deep_merge(merged, global_config)
                sources.append(ConfigSource("global", str(GLOBAL_CONFIG_PATH)))
            except (json.JSONDecodeError, ValueError, OSError) as exc:
                logger.warning("Skipping malformed global config %s: %s", GLOBAL_CONFIG_PATH, exc)

        # Layer project-local config
        env = os.getenv("CIRCUITRY_CONFIG")
        local_kind: ConfigSourceKind
        if env:
            local_path: Path | None = Path(env)
            local_kind = "CIRCUITRY_CONFIG"
        else:
            base = cwd or Path.cwd()
            candidates = [base / name for name in DEFAULT_CONFIG_FILENAMES]
            local_path = _first_existing(candidates)
            local_kind = "project"

        if local_path and local_path.exists():
            try:
                local_config = _load_json_file(local_path)
                merged = _deep_merge(merged, local_config)
                if local_kind == "project":
                    merged = _narrow_allowlists(
                        merged,
                        global_config=global_config,
                        project_config=local_config,
                        project_path=local_path,
                    )
                sources.append(ConfigSource(local_kind, str(local_path)))
            except (json.JSONDecodeError, ValueError, OSError) as exc:
                logger.warning("Skipping malformed project config %s: %s", local_path, exc)

    # Environment variables always overlay on top
    merged = _apply_env_vars(merged)
    sources.extend(ConfigSource("env", name) for name in _applied_env_vars())

    return replace(CircuitryConfig.from_dict(merged), sources=tuple(sources))
