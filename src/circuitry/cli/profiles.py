"""Named profile files: discovery, schema validation, and loading.

A profile is a YAML file (``profiles/<name>.yml``) that supplies run-level
defaults (adapter/model), initial-state inputs, and per-effect model/provider
overrides for a single ``cof run --profile <name>`` invocation. See
``docs/profiles.md`` for the format reference.

Discovery order (first match wins):
  1. ``<orchestration_dir>/profiles/<name>.yml`` (orchestration-scoped)
  2. ``<cwd>/profiles/<name>.yml`` (project-level)

The ``enabled`` (per-effect) and ``persistence`` keys are parsed and schema
validated here, but their runtime behavior is implemented by sibling tasks —
this module only guarantees the data is well-formed and discoverable.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import yaml  # type: ignore[import-untyped]

try:
    import jsonschema as _jsonschema
except ImportError:
    _jsonschema = None  # type: ignore[assignment]

_PROFILE_SUFFIXES = (".yml", ".yaml")
_SCHEMA_PATH = Path(__file__).parent.parent / "schema" / "profile.schema.json"


class ProfileError(ValueError):
    """Base class for profile discovery/validation errors."""


class ProfileNotFoundError(ProfileError):
    pass


class ProfileValidationError(ProfileError):
    pass


@dataclass(frozen=True)
class ProfileSettings:
    name: str
    path: Path
    adapter: str | None
    model: str | None
    inputs: dict[str, Any] = field(default_factory=dict)
    effects: dict[str, dict[str, Any]] = field(default_factory=dict)
    persistence: dict[str, Any] | None = None
    raw: dict[str, Any] = field(default_factory=dict)


def _load_profile_schema() -> dict[str, Any] | None:
    if _jsonschema is None:
        return None
    try:
        return json.loads(_SCHEMA_PATH.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None


def discover_profile_path(
    *, name: str, orchestration_path: Path, cwd: Path | None = None
) -> Path:
    """Resolve a profile name to a file path.

    Orchestration-scoped ``<orch_dir>/profiles/<name>.yml`` wins over the
    project-level ``<cwd>/profiles/<name>.yml``.
    """
    searched: list[Path] = []

    orch_dir = orchestration_path.resolve().parent
    for suffix in _PROFILE_SUFFIXES:
        candidate = orch_dir / "profiles" / f"{name}{suffix}"
        searched.append(candidate)
        if candidate.exists() and candidate.is_file():
            return candidate

    base = (cwd or Path.cwd()).resolve()
    for suffix in _PROFILE_SUFFIXES:
        candidate = base / "profiles" / f"{name}{suffix}"
        searched.append(candidate)
        if candidate.exists() and candidate.is_file():
            return candidate

    searched_str = "; ".join(str(p) for p in searched)
    raise ProfileNotFoundError(
        f"Profile {name!r} not found. Searched: {searched_str}"
    )


def _validate_profile_schema(
    raw: dict[str, Any], *, profile_name: str, path: Path
) -> None:
    schema = _load_profile_schema()
    if schema is None:
        return
    validator = _jsonschema.Draft7Validator(schema)
    errors = sorted(validator.iter_errors(raw), key=str)
    if not errors:
        return
    lines = []
    for e in errors:
        location = "/".join(str(p) for p in e.path) or "<root>"
        lines.append(f"  - {location}: {e.message}")
    raise ProfileValidationError(
        f"Profile {profile_name!r} at {path} failed schema validation:\n"
        + "\n".join(lines)
    )


def _collect_effect_paths(effects: Any, *, scope: str, paths: set[str]) -> None:
    if not isinstance(effects, list):
        return
    for effect in effects:
        if not isinstance(effect, dict):
            continue
        effect_type = str(effect.get("type") or "").strip().lower()
        name_value = effect.get("name")
        name = name_value if isinstance(name_value, str) else ""
        has_name = name.strip() != ""
        own_path = f"{scope}.{name}" if scope and has_name else (name if has_name else scope)

        if has_name:
            paths.add(own_path)

        if effect_type in ("dynamic", "reflector"):
            child_scope = own_path if has_name else scope
            _collect_effect_paths(
                effect.get("effects") or effect.get("steps") or [],
                scope=child_scope,
                paths=paths,
            )
        elif effect_type in ("conditional", "if"):
            branch_scope = own_path if has_name else scope
            _collect_effect_paths(effect.get("then") or [], scope=branch_scope, paths=paths)
            _collect_effect_paths(effect.get("else") or [], scope=branch_scope, paths=paths)
        elif effect_type == "loop":
            body_scope = own_path if has_name else scope
            _collect_effect_paths(effect.get("body") or [], scope=body_scope, paths=paths)


def collect_orchestration_effect_paths(orch: dict[str, Any]) -> set[str]:
    """Collect every dotted effect path an orchestration defines.

    Mirrors the state-path convention built by ``core.compiler`` (dotted
    names relative to the ``prime`` root, with anonymous conditionals/loops
    contributing no path segment of their own).
    """
    paths: set[str] = set()
    effects = orch.get("effects") or orch.get("steps") or []
    _collect_effect_paths(effects, scope="", paths=paths)
    return paths


def _validate_effect_paths(
    effects_map: Any, *, orch: dict[str, Any], profile_name: str
) -> None:
    if not isinstance(effects_map, dict) or not effects_map:
        return
    valid_paths = collect_orchestration_effect_paths(orch)
    unknown = sorted(k for k in effects_map.keys() if k not in valid_paths)
    if not unknown:
        return
    valid_list = ", ".join(sorted(valid_paths)) or "(orchestration defines no named effects)"
    raise ProfileValidationError(
        f"Profile {profile_name!r} references unknown effect path(s): "
        f"{', '.join(unknown)}. Valid effect paths: {valid_list}"
    )


def load_profile(
    *,
    name: str,
    orchestration_path: Path,
    orch: dict[str, Any],
    cwd: Path | None = None,
) -> ProfileSettings:
    """Discover, parse, and validate a named profile file."""
    path = discover_profile_path(name=name, orchestration_path=orchestration_path, cwd=cwd)
    raw = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    if not isinstance(raw, dict):
        raise ProfileValidationError(
            f"Profile {name!r} at {path} must be a mapping/object at the root."
        )

    _validate_profile_schema(raw, profile_name=name, path=path)

    effects_raw = raw.get("effects") or {}
    _validate_effect_paths(effects_raw, orch=orch, profile_name=name)

    adapter = raw.get("adapter")
    model = raw.get("model")
    inputs = raw.get("inputs") or {}
    persistence = raw.get("persistence")

    return ProfileSettings(
        name=name,
        path=path,
        adapter=str(adapter) if adapter is not None else None,
        model=str(model) if model is not None else None,
        inputs=dict(inputs) if isinstance(inputs, dict) else {},
        effects=(
            {str(k): dict(v) for k, v in effects_raw.items() if isinstance(v, dict)}
            if isinstance(effects_raw, dict)
            else {}
        ),
        persistence=dict(persistence) if isinstance(persistence, dict) else None,
        raw=raw,
    )
