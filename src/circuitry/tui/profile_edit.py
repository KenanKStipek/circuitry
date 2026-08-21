"""Everything the Profile view needs that is not a widget.

The effect tree an orchestration exposes to a profile, the editable draft of
a profile file, and the rules that decide what a draft is allowed to say.
No Textual imports live here, so "what a profile is" stays testable — and
reusable — without booting an app.

Two invariants this module exists to hold:

*Byte-compatibility.* :meth:`ProfileDraft.to_yaml` is the only thing that
writes a profile, and what it writes is a plain document the engine's
:func:`~circuitry.cli.profiles.load_profile` reads back unchanged. The editor
never invents a key the profile schema does not have.

*One source of truth for refusals.* A condition path cannot carry an
override; the editor blocks it up front, in the validator's own words, by
calling :func:`~circuitry.cli.profiles.condition_target_message` rather than
paraphrasing it.
"""

from __future__ import annotations

import re
from collections.abc import Iterable, Iterator
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Any

import yaml  # type: ignore[import-untyped]

from ..cli.profiles import (
    ProfileError,
    collect_orchestration_condition_paths,
    condition_target_message,
    discover_profile_path,
    parse_profile_document,
)

__all__ = [
    "BACKENDS",
    "CUSTOM",
    "DEFAULT_PROFILE_NAME",
    "NO_OVERRIDE",
    "BackendField",
    "BackendSpec",
    "EffectNode",
    "EffectOverride",
    "PersistenceDraft",
    "ProfileDraft",
    "backend_by_name",
    "build_effect_tree",
    "condition_refusal",
    "discover_profiles",
    "load_draft",
    "orphan_paths",
    "profile_dir_for",
    "profile_path_for",
    "valid_profile_name",
]

#: Dropdown sentinel: leave this field out of the profile entirely.
NO_OVERRIDE = "—"

#: Dropdown sentinel: the enumerated options do not cover it, type it instead.
CUSTOM = "custom…"

#: What a fresh, unnamed draft is called until the user renames it.
DEFAULT_PROFILE_NAME = "new-profile"

#: Profile names become filenames and a CLI argument, so keep them boring.
_NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")

#: Effect types that hold other effects. A profile override on one of these
#: reaches its whole subtree, which is worth saying in the row.
_CONTAINERS = frozenset({"dynamic", "reflector", "conditional", "if", "loop"})


# -- the effect tree ----------------------------------------------------------


@dataclass(frozen=True)
class EffectNode:
    """One row of the rendered effect tree.

    ``path`` is the dotted address a profile keys its overrides by — the same
    convention runtime state uses, minus the ``prime.`` root. ``kind`` says
    what the row is allowed to do:

    ``effect``
        Overridable: model, provider, and the enabled toggle all apply.
    ``condition``
        A conditional's ``if`` or a loop's ``while``. Rendered so the tree
        matches the file you are looking at, but refused on any edit.
    ``group``
        An anonymous container. It contributes no path segment, so it has
        nothing to override; it exists to keep its children's nesting honest.
    """

    path: str
    label: str
    effect_type: str
    depth: int = 0
    kind: str = "effect"

    @property
    def overridable(self) -> bool:
        return self.kind == "effect"

    @property
    def is_container(self) -> bool:
        return self.effect_type in _CONTAINERS

    @property
    def is_reflector(self) -> bool:
        return self.effect_type == "reflector"

    def row_label(self, *, indent: str = "  ") -> str:
        """The tree row as text: nesting, name, type, and any flag."""
        badge = f"[{self.effect_type}]" if self.effect_type else ""
        flags = []
        if self.is_reflector:
            flags.append("reflector — plans its own subtree")
        elif self.kind == "condition":
            flags.append("condition — not overridable")
        elif self.is_container:
            flags.append("container — override reaches its subtree")
        suffix = f"  ({'; '.join(flags)})" if flags else ""
        return f"{indent * self.depth}{self.label} {badge}{suffix}".rstrip()


def _walk(
    effects: Any, *, scope: str, depth: int, out: list[EffectNode]
) -> None:
    """Mirror ``profiles._collect_effect_paths``, but ordered and typed."""
    if not isinstance(effects, list):
        return
    for effect in effects:
        if not isinstance(effect, dict):
            continue
        effect_type = str(effect.get("type") or "").strip().lower()
        name_value = effect.get("name")
        name = name_value if isinstance(name_value, str) else ""
        has_name = name.strip() != ""
        own_path = (
            f"{scope}.{name}" if scope and has_name else (name if has_name else scope)
        )
        child_scope = own_path if has_name else scope
        child_depth = depth + 1 if has_name else depth

        if has_name:
            out.append(
                EffectNode(
                    path=own_path,
                    label=name,
                    effect_type=effect_type,
                    depth=depth,
                    kind="effect",
                )
            )
        elif effect_type in _CONTAINERS:
            # Anonymous container: transparent to paths, but its children are
            # nested in the file and the tree should not pretend otherwise.
            out.append(
                EffectNode(
                    path="",
                    label=f"<anonymous {effect_type}>",
                    effect_type=effect_type,
                    depth=depth,
                    kind="group",
                )
            )
            child_depth = depth + 1

        if effect_type in ("dynamic", "reflector"):
            _walk(
                effect.get("effects") or effect.get("steps") or [],
                scope=child_scope,
                depth=child_depth,
                out=out,
            )
        elif effect_type in ("conditional", "if"):
            if has_name:
                out.append(
                    EffectNode(
                        path=f"{own_path}.if",
                        label="if",
                        effect_type="condition",
                        depth=child_depth,
                        kind="condition",
                    )
                )
            _walk(effect.get("then") or [], scope=child_scope, depth=child_depth, out=out)
            _walk(effect.get("else") or [], scope=child_scope, depth=child_depth, out=out)
        elif effect_type == "loop":
            if has_name:
                out.append(
                    EffectNode(
                        path=f"{own_path}.while",
                        label="while",
                        effect_type="condition",
                        depth=child_depth,
                        kind="condition",
                    )
                )
            _walk(effect.get("body") or [], scope=child_scope, depth=child_depth, out=out)


def build_effect_tree(orch: dict[str, Any]) -> list[EffectNode]:
    """The orchestration's effects in declaration order, nesting preserved."""
    out: list[EffectNode] = []
    _walk(orch.get("effects") or orch.get("steps") or [], scope="", depth=0, out=out)
    return out


def condition_refusal(path: str, *, profile_name: str) -> str:
    """The validator's own words for "you cannot override a condition"."""
    return condition_target_message([path], profile_name=profile_name)


# -- the draft ----------------------------------------------------------------


@dataclass(frozen=True)
class EffectOverride:
    """What a profile says about one effect. Empty fields are simply absent."""

    model: str | None = None
    provider: str | None = None
    enabled: bool | None = None

    @property
    def is_empty(self) -> bool:
        return self.model is None and self.provider is None and self.enabled is None

    def to_mapping(self) -> dict[str, Any]:
        out: dict[str, Any] = {}
        if self.model is not None:
            out["model"] = self.model
        if self.provider is not None:
            out["provider"] = self.provider
        if self.enabled is not None:
            out["enabled"] = self.enabled
        return out

    @classmethod
    def from_mapping(cls, raw: Any) -> EffectOverride:
        if not isinstance(raw, dict):
            return cls()
        model = raw.get("model")
        provider = raw.get("provider")
        enabled = raw.get("enabled")
        return cls(
            model=str(model) if isinstance(model, str) and model.strip() else None,
            provider=(
                str(provider) if isinstance(provider, str) and provider.strip() else None
            ),
            enabled=bool(enabled) if isinstance(enabled, bool) else None,
        )


@dataclass(frozen=True)
class BackendField:
    """One configurable key of a persistence backend."""

    key: str
    label: str
    required: bool = False
    placeholder: str = ""


@dataclass(frozen=True)
class BackendSpec:
    """A persistence backend as the picker offers it."""

    name: str
    blurb: str
    fields: tuple[BackendField, ...]

    @property
    def required_keys(self) -> tuple[str, ...]:
        return tuple(f.key for f in self.fields if f.required)


#: Backends `core.store.build_persistence_backend` accepts, with the keys each
#: one reads. Mirrors the table in ``docs/profiles.md``.
BACKENDS: tuple[BackendSpec, ...] = (
    BackendSpec(
        "jsonl-file",
        "Appends one record per run to a local file. No server.",
        (BackendField("path", "File path", required=True, placeholder="runs.jsonl"),),
    ),
    BackendSpec(
        "sqlite",
        "Local database file.",
        (
            BackendField(
                "path", "File path", required=True, placeholder=".circuitry/runs.db"
            ),
            BackendField("table", "Table", placeholder="circuitry_runs"),
        ),
    ),
    BackendSpec(
        "mongodb",
        "Needs circuitry-cof[mongodb].",
        (
            BackendField(
                "uri", "URI", required=True, placeholder="mongodb://localhost:27017"
            ),
            BackendField("database", "Database", placeholder="circuitry"),
            BackendField("collection", "Collection", placeholder="circuitry_runs"),
        ),
    ),
    BackendSpec(
        "postgres",
        "Needs psycopg[binary].",
        (
            BackendField(
                "dsn", "DSN", required=True, placeholder="postgresql://…/circuitry"
            ),
            BackendField("table", "Table", placeholder="circuitry_runs"),
            BackendField("sslmode", "SSL mode", placeholder="require"),
        ),
    ),
)


def backend_by_name(name: str) -> BackendSpec | None:
    for spec in BACKENDS:
        if spec.name == name:
            return spec
    return None


@dataclass(frozen=True)
class PersistenceDraft:
    """The profile's ``persistence:`` block while it is being edited."""

    backend: str
    values: dict[str, str] = field(default_factory=dict)
    enabled: bool = True

    def to_mapping(self) -> dict[str, Any]:
        out: dict[str, Any] = {"backend": self.backend}
        if not self.enabled:
            out["enabled"] = False
        for key, value in self.values.items():
            text = value.strip()
            if text:
                out[key] = text
        return out

    def missing_keys(self) -> list[str]:
        """Required keys of the selected backend that are still blank."""
        spec = backend_by_name(self.backend)
        if spec is None:
            return []
        return [
            key for key in spec.required_keys if not str(self.values.get(key, "")).strip()
        ]

    @classmethod
    def from_mapping(cls, raw: Any) -> PersistenceDraft | None:
        if not isinstance(raw, dict):
            return None
        backend = str(raw.get("backend") or "").strip()
        if not backend:
            return None
        values = {
            str(key): _scalar_text(value)
            for key, value in raw.items()
            if key not in ("backend", "enabled")
        }
        enabled = raw.get("enabled")
        return cls(
            backend=backend,
            values=values,
            enabled=True if not isinstance(enabled, bool) else enabled,
        )


def _scalar_text(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    return "" if value is None else str(value)


@dataclass
class ProfileDraft:
    """A profile file as the editor holds it, before it is written out.

    Mutable on purpose — it is the view's working copy — but every field it
    can hold maps one-to-one onto the profile schema, so :meth:`to_mapping`
    is a rename-free projection rather than a translation.
    """

    name: str = DEFAULT_PROFILE_NAME
    adapter: str | None = None
    model: str | None = None
    inputs: dict[str, Any] = field(default_factory=dict)
    effects: dict[str, EffectOverride] = field(default_factory=dict)
    persistence: PersistenceDraft | None = None
    #: The YAML this draft had the last time it was saved or loaded. ``None``
    #: for a draft that has never touched disk, which counts as dirty as soon
    #: as it says anything at all.
    baseline: str | None = None

    # -- projection ----------------------------------------------------------

    def to_mapping(self) -> dict[str, Any]:
        """The profile document, in the order ``docs/profiles.md`` shows it."""
        out: dict[str, Any] = {}
        if self.adapter:
            out["adapter"] = self.adapter
        if self.model:
            out["model"] = self.model
        if self.inputs:
            out["inputs"] = dict(self.inputs)
        effects = {
            path: override.to_mapping()
            for path, override in self.effects.items()
            if not override.is_empty
        }
        if effects:
            out["effects"] = {path: effects[path] for path in sorted(effects)}
        if self.persistence is not None:
            out["persistence"] = self.persistence.to_mapping()
        return out

    def to_yaml(self) -> str:
        """The file body. Block style, declaration order, no Python tags."""
        mapping = self.to_mapping()
        if not mapping:
            # An empty profile is legal (it overrides nothing) but `{}` reads
            # like a mistake; a comment says the same thing out loud.
            return f"# {self.name}: no overrides yet.\n{{}}\n"
        return str(
            yaml.safe_dump(mapping, sort_keys=False, default_flow_style=False, indent=2)
        )

    # -- dirty state ---------------------------------------------------------

    @property
    def dirty(self) -> bool:
        """True when the draft says something the file on disk does not."""
        if self.baseline is None:
            return bool(self.to_mapping())
        return self.to_yaml() != self.baseline

    def mark_clean(self) -> None:
        """Adopt the current content as the saved baseline."""
        self.baseline = self.to_yaml()

    # -- effect overrides ----------------------------------------------------

    def override(self, path: str) -> EffectOverride:
        return self.effects.get(path, EffectOverride())

    def set_override(self, path: str, override: EffectOverride) -> None:
        """Store ``override`` for ``path``, dropping it when it says nothing."""
        if override.is_empty:
            self.effects.pop(path, None)
        else:
            self.effects[path] = override

    def set_model(self, path: str, model: str | None) -> None:
        self.set_override(path, replace(self.override(path), model=model or None))

    def set_provider(self, path: str, provider: str | None) -> None:
        self.set_override(path, replace(self.override(path), provider=provider or None))

    def set_enabled(self, path: str, enabled: bool) -> None:
        """Record an enabled toggle. ``True`` is the default, so it is dropped."""
        self.set_override(
            path, replace(self.override(path), enabled=None if enabled else False)
        )

    def is_enabled(self, path: str) -> bool:
        return self.override(path).enabled is not False

    # -- orphans -------------------------------------------------------------

    def orphans(self, tree: Iterable[EffectNode]) -> list[str]:
        """Override paths the orchestration no longer defines."""
        return orphan_paths(self.effects, tree)

    def drop_orphans(self, tree: Iterable[EffectNode]) -> list[str]:
        """Remove every orphan override. Returns what was dropped."""
        dropped = self.orphans(tree)
        for path in dropped:
            self.effects.pop(path, None)
        return dropped

    # -- validation ----------------------------------------------------------

    def problems(self, orch: dict[str, Any]) -> list[str]:
        """Everything that would stop this draft from saving usefully."""
        issues: list[str] = []
        if not valid_profile_name(self.name):
            issues.append(
                f"{self.name!r} is not a usable profile name — it becomes a "
                "filename and a CLI argument. Use letters, digits, '.', '-' or '_'."
            )
        conditions = collect_orchestration_condition_paths(orch)
        for path in sorted(self.effects):
            if path in conditions:
                issues.append(condition_refusal(path, profile_name=self.name))
        if self.persistence is not None:
            missing = self.persistence.missing_keys()
            if missing:
                issues.append(
                    f"persistence backend {self.persistence.backend!r} needs "
                    f"{', '.join(missing)}."
                )
        return issues

    # -- io ------------------------------------------------------------------

    def save(self, directory: Path) -> Path:
        """Write the profile into ``directory``; returns the file written."""
        directory.mkdir(parents=True, exist_ok=True)
        path = directory / f"{self.name}.yml"
        body = self.to_yaml()
        path.write_text(body, encoding="utf-8")
        self.baseline = body
        return path

    def duplicate(self, name: str) -> ProfileDraft:
        """A copy under a new name, unsaved (so it reads as dirty)."""
        return ProfileDraft(
            name=name,
            adapter=self.adapter,
            model=self.model,
            inputs=dict(self.inputs),
            effects=dict(self.effects),
            persistence=self.persistence,
            baseline=None,
        )


def orphan_paths(
    effects: dict[str, EffectOverride], tree: Iterable[EffectNode]
) -> list[str]:
    """Override paths that match no overridable row of ``tree``."""
    known = {node.path for node in tree if node.overridable}
    return sorted(path for path in effects if path not in known)


def valid_profile_name(name: str) -> bool:
    return bool(_NAME_RE.match(name.strip()))


# -- discovery ----------------------------------------------------------------


def profile_dir_for(orchestration_path: Path) -> Path:
    """Where a profile saved for this orchestration lands.

    The orchestration-scoped directory, because that is the one that wins
    discovery — a profile saved here is the one ``--profile`` finds.
    """
    return orchestration_path.resolve().parent / "profiles"


def profile_path_for(orchestration_path: Path, name: str) -> Path:
    return profile_dir_for(orchestration_path) / f"{name}.yml"


def _profiles_in(directory: Path) -> Iterator[tuple[str, Path]]:
    if not directory.is_dir():
        return
    for path in sorted(directory.iterdir()):
        if path.is_file() and path.suffix.lower() in (".yml", ".yaml"):
            yield path.stem, path


def discover_profiles(
    orchestration_path: Path, *, cwd: Path | None = None
) -> list[tuple[str, Path]]:
    """Profile files visible to this orchestration, discovery order.

    Same order ``cof run --profile`` resolves in: orchestration-scoped first,
    then project-level, first name wins.
    """
    found: dict[str, Path] = {}
    base = (cwd or Path.cwd()).resolve()
    for directory in (profile_dir_for(orchestration_path), base / "profiles"):
        for name, path in _profiles_in(directory):
            found.setdefault(name, path)
    return sorted(found.items())


def load_draft(
    name: str, *, orchestration_path: Path, cwd: Path | None = None
) -> ProfileDraft:
    """Open an existing profile for editing.

    Lenient about effect paths on purpose: overrides the orchestration no
    longer defines come back as orphan rows to be cleaned up, not as a load
    failure. Schema errors still raise — there is nothing coherent to show.
    """
    path = discover_profile_path(
        name=name, orchestration_path=orchestration_path, cwd=cwd
    )
    raw = parse_profile_document(path, name=name)
    draft = ProfileDraft(
        name=name,
        adapter=_optional_text(raw.get("adapter")),
        model=_optional_text(raw.get("model")),
        inputs=dict(raw["inputs"]) if isinstance(raw.get("inputs"), dict) else {},
        effects={
            str(key): EffectOverride.from_mapping(value)
            for key, value in (raw.get("effects") or {}).items()
            if isinstance(value, dict)
        },
        persistence=PersistenceDraft.from_mapping(raw.get("persistence")),
    )
    draft.effects = {
        path_key: override
        for path_key, override in draft.effects.items()
        if not override.is_empty
    }
    draft.mark_clean()
    return draft


def _optional_text(value: Any) -> str | None:
    if isinstance(value, str) and value.strip():
        return value.strip()
    return None


# Re-exported so the view can catch one error class from this module alone.
ProfileEditError = ProfileError
