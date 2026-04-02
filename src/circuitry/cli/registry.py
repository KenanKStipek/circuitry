"""Bundled orchestration registry — reads index.yml and resolves names to paths."""

from __future__ import annotations

import importlib.resources
from pathlib import Path
from typing import Any

import yaml  # type: ignore[import-untyped]


def _bundled_orchestrations_dir() -> Path:
    """Return the Path to the bundled orchestrations directory."""
    pkg = importlib.resources.files("circuitry") / "bundled" / "orchestrations"
    return Path(str(pkg))


def load_index() -> list[dict[str, Any]]:
    """Load the bundled orchestration index.yml and return the list of entries."""
    index_path = _bundled_orchestrations_dir() / "index.yml"
    if not index_path.exists():
        return []
    data = yaml.safe_load(index_path.read_text(encoding="utf-8"))
    return data.get("orchestrations", []) if isinstance(data, dict) else []


def find_entry(name: str) -> dict[str, Any] | None:
    """Look up a single index entry by name, or None if not found."""
    entries = load_index()
    normalised = name.replace("-", "_")
    for entry in entries:
        entry_name = entry.get("name", "")
        entry_stem = Path(entry.get("file", "")).stem
        if (
            entry_name == name
            or entry_name.replace("-", "_") == normalised
            or entry_stem == normalised
        ):
            return entry
    return None


def resolve_bundled(name: str) -> Path | None:
    """Resolve an orchestration name to its bundled file path, or None if not found.

    Accepts the index name (e.g. 'hello', 'article-summarizer') or the filename
    stem (e.g. 'article_summarizer').
    """
    entries = load_index()
    bundled_dir = _bundled_orchestrations_dir()

    # Normalise: dashes → underscores for matching
    normalised = name.replace("-", "_")

    for entry in entries:
        entry_name = entry.get("name", "")
        entry_file = entry.get("file", "")
        entry_stem = Path(entry_file).stem

        if (
            entry_name == name
            or entry_name.replace("-", "_") == normalised
            or entry_stem == normalised
        ):
            candidate = bundled_dir / entry_file
            if candidate.exists():
                return candidate

    return None
