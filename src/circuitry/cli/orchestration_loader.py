from __future__ import annotations

from pathlib import Path
from typing import Any

import yaml  # type: ignore[import-untyped]


def load_orchestration_file(path: Path) -> dict[str, Any]:
    suffix = path.suffix.lower()
    if suffix not in {".yml", ".yaml"}:
        raise ValueError(f"Unsupported orchestration format: {suffix}")

    data = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    if not isinstance(data, dict):
        raise ValueError("Orchestration YAML must be a mapping/object at the root.")
    return data
