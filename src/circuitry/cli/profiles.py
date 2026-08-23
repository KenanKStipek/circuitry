"""Named profile files: discovery, schema validation, and loading.

A profile is a YAML file (``profiles/<name>.yml``) that supplies run-level
defaults (adapter/model), initial-state inputs, and per-effect model/provider
overrides for a single ``cof run --profile <name>`` invocation. See
``docs/profiles.md`` for the format reference.

Discovery order (first match wins):
  1. ``<orchestration_dir>/profiles/<name>.yml`` (orchestration-scoped)
  2. ``<cwd>/profiles/<name>.yml`` (project-level)

Per-effect ``enabled: false`` disables an effect for the run: it is not
executed and its node is written as a skip marker (see ``core.disabled``).
Container effects disable their whole subtree; a container's *condition*
(a conditional's ``if``, a loop's ``while``) cannot be targeted at all and is
rejected here with an actionable error.

``persistence`` is consumed by ``cli.effective_settings``, which layers it over
the orchestration/config ``runtime.persistence`` block.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import yaml  # type: ignore[import-untyped]

from .complexity_config import RoutingSettings, band_named
from .redaction import REDACTED

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


class ProfileReconstructionError(ProfileError):
    """Raised when a recorded profile cannot be rebuilt into a runnable one.

    The only expected cause today is redaction: a profile carrying a secret
    is recorded with that value replaced by ``REDACTED`` (see
    ``circuitry.cli.redaction``), so the record alone cannot reproduce the
    original run. Reconstruction refuses rather than silently running with
    the literal sentinel string as the value.
    """


@dataclass(frozen=True)
class ProfileSettings:
    name: str
    path: Path
    adapter: str | None
    model: str | None
    out: str | None = None
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


def _collect_effect_paths(
    effects: Any, *, scope: str, paths: set[str], conditions: set[str] | None = None
) -> None:
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
                conditions=conditions,
            )
        elif effect_type in ("conditional", "if"):
            branch_scope = own_path if has_name else scope
            if conditions is not None and has_name:
                conditions.update({f"{own_path}.if", f"{own_path}.condition"})
            _collect_effect_paths(
                effect.get("then") or [],
                scope=branch_scope,
                paths=paths,
                conditions=conditions,
            )
            _collect_effect_paths(
                effect.get("else") or [],
                scope=branch_scope,
                paths=paths,
                conditions=conditions,
            )
        elif effect_type == "loop":
            body_scope = own_path if has_name else scope
            if conditions is not None and has_name:
                conditions.update({f"{own_path}.while", f"{own_path}.condition"})
            _collect_effect_paths(
                effect.get("body") or [],
                scope=body_scope,
                paths=paths,
                conditions=conditions,
            )


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


def collect_orchestration_condition_paths(orch: dict[str, Any]) -> set[str]:
    """Collect the pseudo-paths that address a container's *condition*.

    A conditional's ``if:`` and a loop's ``while:`` are ``ConditionDef``
    values, not effects: they select the branch / drive continuation and have
    no state node of their own. They are therefore not overridable — and in
    particular not disableable, since a conditional with no condition has no
    defined branch. These paths exist only so profile validation can say that
    out loud instead of reporting a bare "unknown effect path".
    """
    paths: set[str] = set()
    conditions: set[str] = set()
    effects = orch.get("effects") or orch.get("steps") or []
    _collect_effect_paths(effects, scope="", paths=paths, conditions=conditions)
    return conditions


def condition_target_message(targeted: list[str], *, profile_name: str) -> str:
    """The refusal text for overrides aimed at a container's condition.

    Lives here rather than inline in :func:`_validate_effect_paths` so an
    editor that blocks the same move up front (the TUI's profile view) can
    say it in exactly the validator's words instead of paraphrasing it.
    """
    ordered = sorted(targeted)
    return (
        f"Profile {profile_name!r} targets condition path(s): "
        f"{', '.join(ordered)}. A conditional's 'if' and a "
        "loop's 'while' are conditions, not effects — they cannot be "
        "disabled or overridden, because a container with no condition "
        "has no defined branch. Disable the whole container instead "
        f"(e.g. {ordered[0].rsplit('.', 1)[0]}: {{enabled: false}}), "
        "which disables its entire subtree."
    )


def _validate_effect_paths(
    effects_map: Any, *, orch: dict[str, Any], profile_name: str
) -> None:
    if not isinstance(effects_map, dict) or not effects_map:
        return

    condition_paths = collect_orchestration_condition_paths(orch)
    targeted_conditions = sorted(k for k in effects_map if k in condition_paths)
    if targeted_conditions:
        raise ProfileValidationError(
            condition_target_message(targeted_conditions, profile_name=profile_name)
        )

    valid_paths = collect_orchestration_effect_paths(orch)
    unknown = sorted(k for k in effects_map if k not in valid_paths)
    if not unknown:
        return
    valid_list = ", ".join(sorted(valid_paths)) or "(orchestration defines no named effects)"
    raise ProfileValidationError(
        f"Profile {profile_name!r} references unknown effect path(s): "
        f"{', '.join(unknown)}. Valid effect paths: {valid_list}"
    )


def validate_profile_routing_pins(
    effects_map: Any, *, routing: RoutingSettings, profile_name: str
) -> None:
    """Every profile ``routing: <band-name>`` pin must name a real band.

    Unlike effect-path validation, this can't run at profile-load time: band
    tables live in the orchestration/config's ``runtime.complexity.routing``,
    which is only known once the run's full effective settings are resolved
    (``resolve_effective_settings`` merges config + orchestration). Called
    from ``runtime_shim.run`` after that resolution and before anything
    compiles or dispatches, so a bad pin fails the run the same way an
    unknown effect path does — loudly, up front, not mid-run on the one
    effect that happens to hit it.

    A ``routing: false`` opt-out never reaches here — there is no name to
    check.
    """
    if not isinstance(effects_map, dict) or not effects_map:
        return

    unmatched: list[tuple[str, str]] = []
    for path, override in effects_map.items():
        if not isinstance(override, dict):
            continue
        pin = override.get("routing")
        if not isinstance(pin, str):
            continue
        if band_named(pin, routing.bands) is None:
            unmatched.append((path, pin))

    if not unmatched:
        return

    valid = ", ".join(sorted({b.name for b in routing.bands if b.name}))
    valid_list = valid or "(no named bands configured)"
    pins = ", ".join(f"{path!r} -> {pin!r}" for path, pin in sorted(unmatched))
    raise ProfileValidationError(
        f"Profile {profile_name!r} pins effect(s) to unknown routing band(s): "
        f"{pins}. Valid band names: {valid_list}"
    )


def parse_profile_document(path: Path, *, name: str) -> dict[str, Any]:
    """Read and schema-check a profile file without resolving effect paths.

    The path check needs a target orchestration; an editor loading a profile
    to *show* it does not have one yet — and, more to the point, wants to
    render stale overrides as orphan rows rather than refuse to open the
    file. Schema errors still raise, because a malformed file has nothing
    coherent to render.
    """
    raw = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    if not isinstance(raw, dict):
        raise ProfileValidationError(
            f"Profile {name!r} at {path} must be a mapping/object at the root."
        )
    _validate_profile_schema(raw, profile_name=name, path=path)
    return raw


def _settings_from_raw(raw: dict[str, Any], *, name: str, path: Path) -> ProfileSettings:
    effects_raw = raw.get("effects") or {}
    adapter = raw.get("adapter")
    model = raw.get("model")
    out = raw.get("out")
    inputs = raw.get("inputs") or {}
    persistence = raw.get("persistence")

    return ProfileSettings(
        name=name,
        path=path,
        adapter=str(adapter) if adapter is not None else None,
        model=str(model) if model is not None else None,
        out=str(out) if out is not None else None,
        inputs=dict(inputs) if isinstance(inputs, dict) else {},
        effects=(
            {str(k): dict(v) for k, v in effects_raw.items() if isinstance(v, dict)}
            if isinstance(effects_raw, dict)
            else {}
        ),
        persistence=dict(persistence) if isinstance(persistence, dict) else None,
        raw=raw,
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
    raw = parse_profile_document(path, name=name)

    effects_raw = raw.get("effects") or {}
    _validate_effect_paths(effects_raw, orch=orch, profile_name=name)

    return _settings_from_raw(raw, name=name, path=path)


def _find_redacted_paths(value: Any, *, prefix: str = "") -> list[str]:
    """Dotted paths (in ``value``) whose string carries the redaction sentinel.

    A whole-value redaction (sensitive key like ``api_key``) and a partial
    one (a URL's ``user:pass@`` stripped in place) both leave the sentinel
    substring somewhere in the string, so a substring check catches both.
    """
    found: list[str] = []
    if isinstance(value, dict):
        for key, sub in value.items():
            path = f"{prefix}.{key}" if prefix else str(key)
            if isinstance(sub, str):
                if REDACTED in sub:
                    found.append(path)
            else:
                found.extend(_find_redacted_paths(sub, prefix=path))
    elif isinstance(value, list):
        for index, item in enumerate(value):
            found.extend(_find_redacted_paths(item, prefix=f"{prefix}[{index}]"))
    elif isinstance(value, str) and REDACTED in value:
        found.append(prefix or "<root>")
    return found


def profile_from_record(
    record: dict[str, Any], *, orch: dict[str, Any]
) -> ProfileSettings:
    """Reconstruct a :class:`ProfileSettings` from a recorded profile mapping.

    ``record`` is exactly ``state["runtime"]["effective_settings"]["profile"]``
    as written by ``runtime_shim.run`` — ``{"name": ..., "content": ...}``,
    where ``content`` is the profile's parsed YAML, redacted. There is no file
    on disk to re-read: the record itself is the only input.

    Raises :class:`ProfileReconstructionError` — naming the redacted paths —
    when ``content`` carries the redaction sentinel anywhere, since a
    redacted value cannot be replayed faithfully. Weakening redaction to let
    this succeed is not an option; the caller must supply the missing values
    (e.g. by running from the original profile file instead).
    """
    name = record.get("name")
    if not isinstance(name, str) or not name:
        raise ProfileReconstructionError(
            "Recorded profile is missing a 'name' field; it cannot be reconstructed."
        )
    content = record.get("content")
    if not isinstance(content, dict):
        raise ProfileReconstructionError(
            f"Recorded profile {name!r} is missing its 'content' field; "
            "it cannot be reconstructed."
        )

    redacted_paths = _find_redacted_paths(content)
    if redacted_paths:
        raise ProfileReconstructionError(
            f"Profile {name!r} cannot be reconstructed: its recorded content "
            "was redacted, so replaying it would run with the literal "
            f"'{REDACTED}' sentinel instead of the original value(s). "
            f"Redacted field(s): {', '.join(sorted(redacted_paths))}. "
            "Supply the original profile file (profiles/"
            f"{name}.yml) via --profile instead, or provide these values "
            "explicitly."
        )

    effects_raw = content.get("effects") or {}
    _validate_effect_paths(effects_raw, orch=orch, profile_name=name)

    return _settings_from_raw(
        content, name=name, path=Path(f"<reconstructed:{name}>")
    )
