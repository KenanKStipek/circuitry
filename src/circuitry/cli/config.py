from __future__ import annotations

import copy
import json
import os
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional


DEFAULT_CONFIG_FILENAMES = ("circuitry.config.json", "config.json")

GLOBAL_CONFIG_DIR = Path.home() / ".config" / "circuitry"
GLOBAL_CONFIG_PATH = GLOBAL_CONFIG_DIR / "config.json"

SANE_DEFAULTS: dict[str, Any] = {
    "default_model": "llama3.1:8b",
    "default_adapter": "ollama",
    "runtime": {
        "adapters": {
            "ollama": {
                "base_url": "http://localhost:11434",
            },
        },
    },
}


@dataclass(frozen=True)
class CircuitryConfig:
    """
    CLI-facing configuration. Keep this intentionally narrow and stable.
    Anything runtime-specific can live under `runtime` and pass through.
    """

    # Common defaults users will want
    default_model: Optional[str] = None
    default_adapter: Optional[str] = None

    # Plugins: keep as a list of dotted paths or simple identifiers
    plugins: list[str] = field(default_factory=list)

    # Any additional runtime config to pass through untouched
    runtime: dict[str, Any] = field(default_factory=dict)

    @staticmethod
    def from_dict(d: dict[str, Any]) -> "CircuitryConfig":
        return CircuitryConfig(
            default_model=d.get("default_model"),
            default_adapter=d.get("default_adapter"),
            plugins=list(d.get("plugins") or []),
            runtime=dict(d.get("runtime") or {}),
        )


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


def _first_existing(paths: list[Path]) -> Optional[Path]:
    for p in paths:
        if p.exists() and p.is_file():
            return p
    return None


def _load_json_file(path: Path) -> dict[str, Any]:
    raw = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(raw, dict):
        raise ValueError(f"{path} must contain a JSON object at the root.")
    return raw


def find_config_path(
    *,
    explicit_path: Optional[Path],
    cwd: Optional[Path] = None,
) -> Optional[Path]:
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


def load_config(path: Optional[Path]) -> CircuitryConfig:
    if not path:
        return CircuitryConfig()

    raw = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(raw, dict):
        raise ValueError("config.json must contain a JSON object at the root.")
    return CircuitryConfig.from_dict(raw)


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

    return result


def resolve_config(
    *,
    explicit_path: Optional[Path] = None,
    cwd: Optional[Path] = None,
) -> CircuitryConfig:
    """
    Build a fully-resolved CircuitryConfig by layering:

    1. Sane defaults (lowest priority)
    2. Global config (~/.config/circuitry/config.json)
    3. Project-local config (circuitry.config.json / config.json in cwd)
    4. Explicit --config path (if provided — replaces #2 and #3)
    5. Environment variables (CIRCUITRY_MODEL, CIRCUITRY_ADAPTER, CIRCUITRY_ADAPTER_URL)

    Each layer deep-merges onto the previous.
    """
    merged = copy.deepcopy(SANE_DEFAULTS)

    if explicit_path:
        # Explicit path skips global/project discovery — use only that file
        file_config = _load_json_file(explicit_path)
        merged = _deep_merge(merged, file_config)
    else:
        # Layer global config
        if GLOBAL_CONFIG_PATH.exists() and GLOBAL_CONFIG_PATH.is_file():
            try:
                global_config = _load_json_file(GLOBAL_CONFIG_PATH)
                merged = _deep_merge(merged, global_config)
            except Exception:
                pass  # Silently skip malformed global config

        # Layer project-local config
        env = os.getenv("CIRCUITRY_CONFIG")
        if env:
            local_path: Optional[Path] = Path(env)
        else:
            base = cwd or Path.cwd()
            candidates = [base / name for name in DEFAULT_CONFIG_FILENAMES]
            local_path = _first_existing(candidates)

        if local_path and local_path.exists():
            try:
                local_config = _load_json_file(local_path)
                merged = _deep_merge(merged, local_config)
            except Exception:
                pass  # Silently skip malformed project config

    # Environment variables always overlay on top
    merged = _apply_env_vars(merged)

    return CircuitryConfig.from_dict(merged)
