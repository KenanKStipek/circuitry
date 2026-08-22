from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import yaml  # type: ignore[import-untyped]

try:
    from toon_format import decode as _toon_decode
    from toon_format import encode as _toon_encode
except ImportError:
    _toon_decode = None  # type: ignore[assignment]
    _toon_encode = None  # type: ignore[assignment]

ORCHESTRATION_SUFFIXES = {".yml", ".yaml", ".json", ".toon"}

FORMAT_LABELS: dict[str, str] = {
    ".yml": "YAML",
    ".yaml": "YAML",
    ".json": "JSON",
    ".toon": "TOON",
}


def load_orchestration_file(path: Path) -> dict[str, Any]:
    suffix = path.suffix.lower()
    if suffix not in ORCHESTRATION_SUFFIXES:
        supported = ", ".join(sorted(ORCHESTRATION_SUFFIXES))
        raise ValueError(
            f"Unsupported orchestration format: {suffix}. "
            f"Supported: {supported}"
        )

    label = FORMAT_LABELS[suffix]
    raw = path.read_text(encoding="utf-8")

    if suffix in {".yml", ".yaml"}:
        data = yaml.safe_load(raw) or {}
    elif suffix == ".json":
        data = json.loads(raw)
    elif suffix == ".toon":
        if _toon_decode is None:
            raise ImportError(
                "The 'toon-format' package is required to load .toon files. "
                "Install it with: pip install git+https://github.com/toon-format/toon-python.git"
            )
        data = _toon_decode(raw)
    else:
        raise ValueError(f"Unsupported orchestration format: {suffix}")

    if not isinstance(data, dict):
        raise ValueError(
            f"Orchestration {label} must be a mapping/object at the root."
        )
    return data


def serialize_orchestration(data: dict[str, Any], fmt: str) -> str:
    if fmt == "yaml":
        return yaml.dump(data, default_flow_style=False, sort_keys=False)
    if fmt == "json":
        return json.dumps(data, indent=2, ensure_ascii=False)
    if fmt == "toon":
        if _toon_encode is None:
            raise ImportError(
                "The 'toon-format' package is required to serialize to TOON. "
                "Install it with: pip install git+https://github.com/toon-format/toon-python.git"
            )
        return _toon_encode(data)
    raise ValueError(f"Unsupported output format: {fmt!r}. Supported: yaml, json, toon")
