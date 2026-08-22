"""Definition, validation, and resolution of the ``runtime.complexity`` block.

This module owns the *config surface* only: what the block looks like, what a
valid value is, and the typed view later consumers read. Scoring, routing, and
decomposition behaviour live elsewhere — nothing here scores a prompt, picks a
model, or splits an effect.

```yaml
runtime:
  complexity:
    scoring:       {enabled: false, weights: {...}, keywords: {...}}
    routing:       {enabled: false, bands: [...], respect_explicit: true}
    decomposition: {enabled: false, threshold: 80, max_depth: 2,
                    max_chunks: 8, on_failure: route_up}
```

The three switches are independent, with one ordering constraint: scoring is
the substrate both routing and decomposition read, so enabling either of them
without scoring is a config error. ``scoring`` alone, ``scoring`` + one, and
all three are each valid.

The block flows through the normal ``runtime.*`` precedence — an
orchestration-level ``runtime.complexity`` *replaces* the config-level one
wholesale (:func:`circuitry.cli.effective_settings._merge_runtime` is a shallow
merge over top-level runtime keys), so an orchestration that overrides the
block must restate every value it still wants.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import Any

from .config import ConfigError

#: Bounded, documented range every complexity score is normalized into.
#: Band boundaries and the decomposition threshold are validated against it so
#: band tables stay portable between orchestrations.
SCORE_MIN = 0.0
SCORE_MAX = 100.0

#: Signals the scorer weighs, with their default relative weights. A weight is
#: a multiplier on that signal's normalized measurement; the scorer owns how
#: the weighted signals combine into the final score. Unknown signal names are
#: rejected at config resolution so a typo cannot silently do nothing.
DEFAULT_WEIGHTS: dict[str, float] = {
    "prompt_size": 1.0,
    "state_references": 1.5,
    "prompt_type": 0.75,
    "output_schema": 1.25,
    "output_size": 1.0,
    "structural_position": 0.5,
    "keywords": 1.0,
}

DEFAULT_THRESHOLD = 80.0
DEFAULT_MAX_DEPTH = 2
DEFAULT_MAX_CHUNKS = 8
DEFAULT_ON_FAILURE = "route_up"

ON_FAILURE_CHOICES: tuple[str, ...] = ("route_up", "fail")

_COMPLEXITY_KEYS: tuple[str, ...] = ("scoring", "routing", "decomposition")
_SCORING_KEYS: tuple[str, ...] = ("enabled", "weights", "keywords")
_ROUTING_KEYS: tuple[str, ...] = ("enabled", "bands", "respect_explicit")
_DECOMPOSITION_KEYS: tuple[str, ...] = (
    "enabled",
    "threshold",
    "max_depth",
    "max_chunks",
    "on_failure",
)
_BAND_KEYS: tuple[str, ...] = ("max", "model", "name")


class ComplexityConfigError(ConfigError):
    """A ``runtime.complexity`` block that cannot be used.

    Subclasses :class:`~circuitry.cli.config.ConfigError` (and therefore
    ``ValueError``): the message is user-facing and is printed verbatim by the
    CLI, so it must read as a complete, actionable sentence.
    """


@dataclass(frozen=True)
class ComplexityBand:
    """One row of the routing band table.

    ``max`` is the inclusive upper bound of the band: a score matches the first
    band whose ``max`` is greater than or equal to it. ``max is None`` marks the
    catch-all, which is required and must be last.
    """

    model: str
    max: float | None = None
    name: str | None = None

    @property
    def is_catch_all(self) -> bool:
        return self.max is None

    def as_dict(self) -> dict[str, Any]:
        return {"name": self.name, "max": self.max, "model": self.model}


@dataclass(frozen=True)
class ScoringSettings:
    """``runtime.complexity.scoring`` — the substrate switch."""

    enabled: bool = False
    weights: Mapping[str, float] = field(
        default_factory=lambda: dict(DEFAULT_WEIGHTS)
    )
    keywords: Mapping[str, float] = field(default_factory=dict)

    def as_dict(self) -> dict[str, Any]:
        return {
            "enabled": self.enabled,
            "weights": dict(self.weights),
            "keywords": dict(self.keywords),
        }


@dataclass(frozen=True)
class RoutingSettings:
    """``runtime.complexity.routing`` — score to model."""

    enabled: bool = False
    bands: tuple[ComplexityBand, ...] = ()
    respect_explicit: bool = True

    def as_dict(self) -> dict[str, Any]:
        return {
            "enabled": self.enabled,
            "bands": [band.as_dict() for band in self.bands],
            "respect_explicit": self.respect_explicit,
        }


@dataclass(frozen=True)
class DecompositionSettings:
    """``runtime.complexity.decomposition`` — split the over-complex."""

    enabled: bool = False
    threshold: float = DEFAULT_THRESHOLD
    max_depth: int = DEFAULT_MAX_DEPTH
    max_chunks: int = DEFAULT_MAX_CHUNKS
    on_failure: str = DEFAULT_ON_FAILURE

    def as_dict(self) -> dict[str, Any]:
        return {
            "enabled": self.enabled,
            "threshold": self.threshold,
            "max_depth": self.max_depth,
            "max_chunks": self.max_chunks,
            "on_failure": self.on_failure,
        }


@dataclass(frozen=True)
class ComplexitySettings:
    """The resolved ``runtime.complexity`` block.

    Consumers read this instead of re-parsing raw dicts; every field is already
    validated, defaulted, and typed.
    """

    scoring: ScoringSettings = field(default_factory=ScoringSettings)
    routing: RoutingSettings = field(default_factory=RoutingSettings)
    decomposition: DecompositionSettings = field(
        default_factory=DecompositionSettings
    )

    @property
    def enabled(self) -> bool:
        """True when any switch is on — i.e. the feature does anything at all."""
        return (
            self.scoring.enabled
            or self.routing.enabled
            or self.decomposition.enabled
        )

    def as_dict(self) -> dict[str, Any]:
        return {
            "scoring": self.scoring.as_dict(),
            "routing": self.routing.as_dict(),
            "decomposition": self.decomposition.as_dict(),
        }


DEFAULT_COMPLEXITY_SETTINGS = ComplexitySettings()


# --------------------------------------------------------------------------
# scalar coercion helpers — every message names the full config path
# --------------------------------------------------------------------------


def _type_name(value: Any) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "a boolean"
    if isinstance(value, (int, float)):
        return "a number"
    if isinstance(value, str):
        return "a string"
    if isinstance(value, Mapping):
        return "an object"
    if isinstance(value, list):
        return "an array"
    return f"a {type(value).__name__}"


def _as_mapping(value: Any, path: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise ComplexityConfigError(
            f"{path} must be an object; found {_type_name(value)}."
        )
    for key in value:
        if not isinstance(key, str):
            raise ComplexityConfigError(f"{path} keys must be strings; found {key!r}.")
    return value


def _reject_unknown_keys(
    block: Mapping[str, Any], allowed: tuple[str, ...], path: str
) -> None:
    unknown = sorted(key for key in block if key not in allowed)
    if not unknown:
        return
    label = "key" if len(unknown) == 1 else "keys"
    listed = ", ".join(repr(key) for key in unknown)
    raise ComplexityConfigError(
        f"{path}: unknown {label} {listed}. "
        f"Valid keys: {', '.join(sorted(allowed))}."
    )


def _as_bool(block: Mapping[str, Any], key: str, path: str, default: bool) -> bool:
    if key not in block:
        return default
    value = block[key]
    if not isinstance(value, bool):
        raise ComplexityConfigError(
            f"{path}.{key} must be true or false; found {_type_name(value)}."
        )
    return value


def _as_number(value: Any, path: str) -> float:
    # bool is an int subclass; a switch value where a number belongs is a
    # mistake worth naming rather than silently reading as 0/1.
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ComplexityConfigError(
            f"{path} must be a number; found {_type_name(value)}."
        )
    return float(value)


def _as_score(value: Any, path: str) -> float:
    number = _as_number(value, path)
    if not SCORE_MIN <= number <= SCORE_MAX:
        raise ComplexityConfigError(
            f"{path} must be between {SCORE_MIN:g} and {SCORE_MAX:g} "
            f"(the complexity score range); found {number:g}."
        )
    return number


def _as_int(value: Any, path: str, *, minimum: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        # Accept 2.0 from JSON/YAML floats, reject 2.5.
        if isinstance(value, float) and value.is_integer():
            value = int(value)
        else:
            raise ComplexityConfigError(
                f"{path} must be a whole number; found {_type_name(value)}."
            )
    if value < minimum:
        raise ComplexityConfigError(
            f"{path} must be {minimum} or greater; found {value}."
        )
    return int(value)


def _as_non_empty_str(value: Any, path: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ComplexityConfigError(
            f"{path} must be a non-empty string; found {_type_name(value)}."
        )
    return value.strip()


# --------------------------------------------------------------------------
# block parsers
# --------------------------------------------------------------------------


def _parse_scoring(raw: Any, path: str) -> ScoringSettings:
    block = _as_mapping(raw, path)
    _reject_unknown_keys(block, _SCORING_KEYS, path)

    weights = dict(DEFAULT_WEIGHTS)
    if "weights" in block and block["weights"] is not None:
        raw_weights = _as_mapping(block["weights"], f"{path}.weights")
        for name, value in raw_weights.items():
            if name not in DEFAULT_WEIGHTS:
                raise ComplexityConfigError(
                    f"{path}.weights: unknown signal {name!r}. "
                    f"Valid signals: {', '.join(sorted(DEFAULT_WEIGHTS))}."
                )
            weights[name] = _as_number(value, f"{path}.weights.{name}")

    keywords: dict[str, float] = {}
    if "keywords" in block and block["keywords"] is not None:
        raw_keywords = _as_mapping(block["keywords"], f"{path}.keywords")
        for name, value in raw_keywords.items():
            keyword = _as_non_empty_str(name, f"{path}.keywords key")
            keywords[keyword] = _as_number(value, f"{path}.keywords.{name}")

    return ScoringSettings(
        enabled=_as_bool(block, "enabled", path, default=False),
        weights=weights,
        keywords=keywords,
    )


def _parse_band(raw: Any, path: str) -> ComplexityBand:
    block = _as_mapping(raw, path)
    _reject_unknown_keys(block, _BAND_KEYS, path)

    if "model" not in block:
        raise ComplexityConfigError(f"{path} is missing required field 'model'.")
    model = _as_non_empty_str(block["model"], f"{path}.model")

    upper: float | None = None
    if block.get("max") is not None:
        upper = _as_score(block["max"], f"{path}.max")

    name: str | None = None
    if block.get("name") is not None:
        name = _as_non_empty_str(block["name"], f"{path}.name")

    return ComplexityBand(model=model, max=upper, name=name)


def _parse_bands(raw: Any, path: str) -> tuple[ComplexityBand, ...]:
    if not isinstance(raw, list):
        raise ComplexityConfigError(
            f"{path} must be an array of band objects; found {_type_name(raw)}."
        )
    if not raw:
        raise ComplexityConfigError(
            f"{path} must not be empty — a band table needs at least a catch-all "
            "band (one with no 'max')."
        )

    bands = tuple(
        _parse_band(entry, f"{path}[{index}]") for index, entry in enumerate(raw)
    )

    for index, band in enumerate(bands[:-1]):
        if band.is_catch_all:
            raise ComplexityConfigError(
                f"{path}[{index}] is a catch-all band (no 'max') but is not last; "
                "bands after it can never match. Move it to the end of the table."
            )

    if not bands[-1].is_catch_all:
        raise ComplexityConfigError(
            f"{path} must end with a catch-all band — an entry with no 'max' — so "
            f"every score resolves to a model. Drop 'max' from {path}"
            f"[{len(bands) - 1}] or append a new entry."
        )

    previous: float | None = None
    for index, band in enumerate(bands):
        if band.max is None:
            continue
        if previous is not None and band.max <= previous:
            raise ComplexityConfigError(
                f"{path} must be ordered by ascending 'max' with no overlap: "
                f"{path}[{index}] has max {band.max:g}, which is not greater than "
                f"the preceding band's max {previous:g}."
            )
        previous = band.max

    return bands


def _parse_routing(raw: Any, path: str) -> RoutingSettings:
    block = _as_mapping(raw, path)
    _reject_unknown_keys(block, _ROUTING_KEYS, path)

    enabled = _as_bool(block, "enabled", path, default=False)

    bands: tuple[ComplexityBand, ...] = ()
    if block.get("bands") is not None:
        bands = _parse_bands(block["bands"], f"{path}.bands")
    elif enabled:
        raise ComplexityConfigError(
            f"{path}.enabled is true but no bands are defined. Add "
            f"{path}.bands with at least a catch-all band (one with no 'max')."
        )

    return RoutingSettings(
        enabled=enabled,
        bands=bands,
        respect_explicit=_as_bool(block, "respect_explicit", path, default=True),
    )


def _parse_decomposition(raw: Any, path: str) -> DecompositionSettings:
    block = _as_mapping(raw, path)
    _reject_unknown_keys(block, _DECOMPOSITION_KEYS, path)

    threshold = DEFAULT_THRESHOLD
    if block.get("threshold") is not None:
        threshold = _as_score(block["threshold"], f"{path}.threshold")

    max_depth = DEFAULT_MAX_DEPTH
    if block.get("max_depth") is not None:
        max_depth = _as_int(block["max_depth"], f"{path}.max_depth", minimum=0)

    max_chunks = DEFAULT_MAX_CHUNKS
    if block.get("max_chunks") is not None:
        max_chunks = _as_int(block["max_chunks"], f"{path}.max_chunks", minimum=1)

    on_failure = DEFAULT_ON_FAILURE
    if block.get("on_failure") is not None:
        on_failure = _as_non_empty_str(block["on_failure"], f"{path}.on_failure")
        if on_failure not in ON_FAILURE_CHOICES:
            raise ComplexityConfigError(
                f"{path}.on_failure must be one of "
                f"{', '.join(ON_FAILURE_CHOICES)}; found {on_failure!r}."
            )

    return DecompositionSettings(
        enabled=_as_bool(block, "enabled", path, default=False),
        threshold=threshold,
        max_depth=max_depth,
        max_chunks=max_chunks,
        on_failure=on_failure,
    )


def _check_prerequisites(settings: ComplexitySettings, path: str) -> None:
    if settings.scoring.enabled:
        return
    for name, switch in (
        ("routing", settings.routing),
        ("decomposition", settings.decomposition),
    ):
        if switch.enabled:
            raise ComplexityConfigError(
                f"{path}.{name}.enabled is true but {path}.scoring.enabled is "
                f"false. {name.capitalize()} consumes complexity scores, so it "
                f"requires the scorer — set {path}.scoring.enabled to true or "
                f"turn {name} off."
            )


def parse_complexity_settings(
    raw: Any, *, path: str = "runtime.complexity"
) -> ComplexitySettings:
    """Validate and resolve a raw ``runtime.complexity`` value.

    ``None`` (block absent) resolves to the all-off defaults. Anything
    malformed raises :class:`ComplexityConfigError` with a message naming the
    offending path.
    """
    if raw is None:
        return DEFAULT_COMPLEXITY_SETTINGS

    block = _as_mapping(raw, path)
    _reject_unknown_keys(block, _COMPLEXITY_KEYS, path)

    def sub_block(key: str) -> Any:
        # Absent or explicitly null means "defaults"; anything else — including
        # an empty list — still has to be an object.
        value = block.get(key)
        return {} if value is None else value

    settings = ComplexitySettings(
        scoring=_parse_scoring(sub_block("scoring"), f"{path}.scoring"),
        routing=_parse_routing(sub_block("routing"), f"{path}.routing"),
        decomposition=_parse_decomposition(
            sub_block("decomposition"), f"{path}.decomposition"
        ),
    )
    _check_prerequisites(settings, path)
    return settings


def resolve_complexity_settings(
    runtime: Mapping[str, Any] | None,
) -> ComplexitySettings:
    """Resolve ``runtime.complexity`` out of an already-merged runtime dict.

    This is the accessor consumers hold: give it the effective ``runtime``
    mapping and get back typed, validated settings — no raw-dict digging.
    """
    if not runtime:
        return DEFAULT_COMPLEXITY_SETTINGS
    return parse_complexity_settings(runtime.get("complexity"))
