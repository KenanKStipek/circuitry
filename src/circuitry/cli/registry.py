"""Curation library registry — reads manifest.json and resolves names to paths.

The curation library lives at `src/circuitry/curation/` and is organised by
category subdirectory (`learn/`, `utilities/`, `patterns/`, `recipes/`,
`agents/`). Slash-delimited names like `utilities/critique` resolve to
`curation/utilities/critique.yml`.
"""

from __future__ import annotations

import importlib.resources
import json
from pathlib import Path
from typing import Any


def _curation_dir() -> Path:
    """Return the Path to the curation library root."""
    pkg = importlib.resources.files("circuitry") / "curation"
    return Path(str(pkg))


# Back-compat alias for any callers still using the old name.
_bundled_orchestrations_dir = _curation_dir


def _manifest_path() -> Path:
    return _curation_dir() / "manifest.json"


def load_index() -> list[dict[str, Any]]:
    """Load the curation manifest and return the list of entries.

    Each entry is normalised to the legacy index shape consumed by `cof list/info/eject`:
    keys `name`, `file`, `description`, `category`, `backends`, `inputs`.
    """
    path = _manifest_path()
    if not path.exists():
        return []
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return []

    if not isinstance(data, dict):
        return []
    raw_entries = data.get("entries") or data.get("orchestrations") or []
    if not isinstance(raw_entries, list):
        return []

    out: list[dict[str, Any]] = []
    for entry in raw_entries:
        if not isinstance(entry, dict):
            continue
        out.append(_normalise_entry(entry))
    return out


def _normalise_entry(entry: dict[str, Any]) -> dict[str, Any]:
    """Adapt a manifest entry to the legacy index shape."""
    name = entry.get("name") or Path(entry.get("file", "")).stem
    inputs_raw = entry.get("inputs")
    inputs: list[dict[str, Any]] = []
    if isinstance(inputs_raw, dict):
        for key, spec in inputs_raw.items():
            if isinstance(spec, dict):
                inputs.append({"name": key, **spec})
            else:
                inputs.append({"name": key})
    elif isinstance(inputs_raw, list):
        inputs = [i for i in inputs_raw if isinstance(i, dict)]

    return {
        "name": name,
        "file": entry.get("file", ""),
        "description": entry.get("description") or entry.get("intent", ""),
        "category": entry.get("category", ""),
        "backends": entry.get("backends", []),
        "inputs": inputs,
        "intent": entry.get("intent"),
        "when_to_use": entry.get("when_to_use"),
        "outputs": entry.get("outputs"),
        "primitives": entry.get("primitives"),
        "tags": entry.get("tags"),
        "difficulty": entry.get("difficulty"),
        "example": entry.get("example"),
    }


def _name_matches(query: str, entry_name: str, entry_stem: str) -> bool:
    """Loose name matching: exact, dash↔underscore, slash-stripped, stem."""
    if query in (entry_name, entry_stem):
        return True
    norm_query = query.replace("-", "_")
    norm_entry = entry_name.replace("-", "_")
    if norm_query == norm_entry:
        return True
    # Last segment match: 'critique' → 'utilities/critique'
    if "/" in entry_name:
        tail = entry_name.split("/")[-1]
        if query == tail or norm_query == tail.replace("-", "_"):
            return True
    return False


def find_entry(name: str) -> dict[str, Any] | None:
    """Look up a single index entry by name (or last-segment of slash name)."""
    for entry in load_index():
        entry_name = str(entry.get("name", ""))
        entry_stem = Path(str(entry.get("file", ""))).stem
        if _name_matches(name, entry_name, entry_stem):
            return entry
    return None


def resolve_bundled(name: str) -> Path | None:
    """Resolve a curation entry name to its file path.

    Resolution strategy:
      1. Manifest lookup (preferred): match against entry `name` (slash-delimited
         like `utilities/critique`) or filename stem.
      2. Direct slash-delimited filesystem walk: `utilities/critique` →
         `curation/utilities/critique.yml`.
    """
    bundled_dir = _curation_dir()
    if not bundled_dir.exists():
        return None

    # 1. Manifest match
    for entry in load_index():
        entry_name = str(entry.get("name", ""))
        entry_file = str(entry.get("file", ""))
        entry_stem = Path(entry_file).stem
        if _name_matches(name, entry_name, entry_stem):
            candidate = bundled_dir / entry_file
            if candidate.exists():
                return candidate

    # 2. Direct slash-delimited path: 'utilities/critique' → utilities/critique.yml
    if "/" in name or "_" in name or "-" in name:
        slashed = name.replace("-", "_")
        for suffix in (".yml", ".yaml"):
            candidate = bundled_dir / f"{slashed}{suffix}"
            if candidate.exists():
                return candidate

    return None
