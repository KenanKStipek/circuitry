from __future__ import annotations

import json
import os
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional


DEFAULT_CONFIG_FILENAMES = ("circuitry.config.json", "config.json")


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


def _first_existing(paths: list[Path]) -> Optional[Path]:
    for p in paths:
        if p.exists() and p.is_file():
            return p
    return None


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
    """
    if explicit_path:
        return explicit_path

    env = os.getenv("CIRCUITRY_CONFIG")
    if env:
        return Path(env)

    base = cwd or Path.cwd()
    candidates = [base / name for name in DEFAULT_CONFIG_FILENAMES]
    return _first_existing(candidates)


def load_config(path: Optional[Path]) -> CircuitryConfig:
    if not path:
        return CircuitryConfig()

    raw = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(raw, dict):
        raise ValueError("config.json must contain a JSON object at the root.")
    return CircuitryConfig.from_dict(raw)
