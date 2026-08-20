from __future__ import annotations

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.cli.complexity_config import (
    DEFAULT_MAX_CHUNKS,
    DEFAULT_MAX_DEPTH,
    DEFAULT_ON_FAILURE,
    DEFAULT_THRESHOLD,
    DEFAULT_WEIGHTS,
    ComplexityConfigError,
    parse_complexity_settings,
    resolve_complexity_settings,
)
from circuitry.cli.config import CircuitryConfig, ConfigError
from circuitry.cli.effective_settings import resolve_effective_settings

BANDS = [
    {"name": "cheap", "max": 40, "model": "small"},
    {"name": "mid", "max": 75, "model": "medium"},
    {"name": "top", "model": "large"},
]


def _complexity(**blocks: object) -> dict[str, object]:
    return {"complexity": dict(blocks)}


# --------------------------------------------------------------------------
# defaults
# --------------------------------------------------------------------------


def test_absent_block_resolves_to_all_switches_off() -> None:
    settings = resolve_complexity_settings(None)

    assert settings.scoring.enabled is False
    assert settings.routing.enabled is False
    assert settings.decomposition.enabled is False
    assert settings.enabled is False


def test_documented_defaults() -> None:
    settings = parse_complexity_settings({})

    assert settings.decomposition.threshold == DEFAULT_THRESHOLD == 80.0
    assert settings.decomposition.max_depth == DEFAULT_MAX_DEPTH == 2
    assert settings.decomposition.max_chunks == DEFAULT_MAX_CHUNKS == 8
    assert settings.decomposition.on_failure == DEFAULT_ON_FAILURE == "route_up"
    assert settings.routing.respect_explicit is True
    assert settings.scoring.weights == DEFAULT_WEIGHTS
    assert settings.scoring.keywords == {}


def test_runtime_without_complexity_key_is_unchanged_from_today() -> None:
    cfg = CircuitryConfig(runtime={"adapters": {"ollama": {"base_url": "x"}}})

    effective = resolve_effective_settings(cfg=cfg, orch={})

    assert "complexity" not in effective.runtime
    assert effective.complexity.enabled is False
    assert effective.sources["complexity"] == "default"


def test_partial_block_only_overrides_what_it_names() -> None:
    settings = parse_complexity_settings(
        {"scoring": {"enabled": True}, "decomposition": {"max_depth": 3}}
    )

    assert settings.scoring.weights == DEFAULT_WEIGHTS
    assert settings.decomposition.max_depth == 3
    assert settings.decomposition.threshold == DEFAULT_THRESHOLD
    assert settings.decomposition.on_failure == DEFAULT_ON_FAILURE


# --------------------------------------------------------------------------
# each switch independently
# --------------------------------------------------------------------------


def test_scoring_only_is_valid() -> None:
    settings = parse_complexity_settings(
        {"scoring": {"enabled": True, "weights": {"prompt_size": 2.5}}}
    )

    assert settings.scoring.enabled is True
    assert settings.scoring.weights["prompt_size"] == 2.5
    # untouched signals keep their documented defaults
    assert settings.scoring.weights["output_size"] == DEFAULT_WEIGHTS["output_size"]
    assert settings.routing.enabled is False
    assert settings.decomposition.enabled is False
    assert settings.enabled is True


def test_scoring_plus_routing_is_valid() -> None:
    settings = parse_complexity_settings(
        {
            "scoring": {"enabled": True},
            "routing": {"enabled": True, "bands": BANDS},
        }
    )

    assert settings.routing.enabled is True
    assert settings.decomposition.enabled is False
    assert [band.model for band in settings.routing.bands] == [
        "small",
        "medium",
        "large",
    ]
    assert settings.routing.bands[0].max == 40.0
    assert settings.routing.bands[-1].is_catch_all is True


def test_scoring_plus_decomposition_is_valid_without_routing() -> None:
    settings = parse_complexity_settings(
        {
            "scoring": {"enabled": True},
            "decomposition": {
                "enabled": True,
                "threshold": 65,
                "max_depth": 1,
                "max_chunks": 4,
                "on_failure": "fail",
            },
        }
    )

    assert settings.routing.enabled is False
    assert settings.decomposition.enabled is True
    assert settings.decomposition.threshold == 65.0
    assert settings.decomposition.max_depth == 1
    assert settings.decomposition.max_chunks == 4
    assert settings.decomposition.on_failure == "fail"


def test_all_three_switches_together_are_valid() -> None:
    settings = parse_complexity_settings(
        {
            "scoring": {"enabled": True, "keywords": {"refactor": 5}},
            "routing": {"enabled": True, "bands": BANDS},
            "decomposition": {"enabled": True},
        }
    )

    assert settings.enabled is True
    assert settings.scoring.keywords == {"refactor": 5.0}
    assert settings.routing.enabled is True
    assert settings.decomposition.enabled is True


# --------------------------------------------------------------------------
# missing prerequisite
# --------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("switch", "block"),
    [
        ("routing", {"enabled": True, "bands": BANDS}),
        ("decomposition", {"enabled": True}),
    ],
)
def test_switch_without_scoring_names_the_missing_prerequisite(
    switch: str, block: dict[str, object]
) -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings({switch: block})

    message = str(excinfo.value)
    assert f"runtime.complexity.{switch}.enabled is true" in message
    assert "runtime.complexity.scoring.enabled" in message


def test_switch_with_scoring_explicitly_disabled_still_errors() -> None:
    with pytest.raises(ComplexityConfigError):
        parse_complexity_settings(
            {
                "scoring": {"enabled": False},
                "decomposition": {"enabled": True},
            }
        )


def test_routing_and_decomposition_do_not_require_each_other() -> None:
    both_off = parse_complexity_settings({"scoring": {"enabled": True}})
    assert both_off.scoring.enabled is True


# --------------------------------------------------------------------------
# malformed input
# --------------------------------------------------------------------------


def test_complexity_block_must_be_an_object() -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings(["scoring"])

    assert "runtime.complexity must be an object" in str(excinfo.value)


def test_unknown_top_level_key_is_rejected_with_the_valid_set() -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings({"scorring": {"enabled": True}})

    message = str(excinfo.value)
    assert "unknown key 'scorring'" in message
    assert "Valid keys: decomposition, routing, scoring" in message


@pytest.mark.parametrize(
    ("block", "needle"),
    [
        ({"scoring": {"enabld": True}}, "runtime.complexity.scoring: unknown key"),
        (
            {"routing": {"band": []}},
            "runtime.complexity.routing: unknown key 'band'",
        ),
        (
            {"decomposition": {"depth": 3}},
            "runtime.complexity.decomposition: unknown key 'depth'",
        ),
    ],
)
def test_unknown_nested_keys_are_rejected(block: dict[str, object], needle: str) -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings(block)

    assert needle in str(excinfo.value)


def test_unknown_weight_signal_lists_the_valid_signals() -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings({"scoring": {"weights": {"vibes": 1}}})

    message = str(excinfo.value)
    assert "unknown signal 'vibes'" in message
    assert "prompt_size" in message


def test_non_numeric_weight_is_rejected() -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings({"scoring": {"weights": {"prompt_size": "heavy"}}})

    assert "runtime.complexity.scoring.weights.prompt_size must be a number" in str(
        excinfo.value
    )


def test_enabled_must_be_a_boolean() -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings({"scoring": {"enabled": "yes"}})

    assert "runtime.complexity.scoring.enabled must be true or false" in str(
        excinfo.value
    )


@pytest.mark.parametrize(
    ("value", "needle"),
    [
        ("high", "must be a number"),
        (True, "must be a number"),
        (140, "must be between 0 and 100"),
        (-1, "must be between 0 and 100"),
    ],
)
def test_non_numeric_or_out_of_range_threshold_is_rejected(
    value: object, needle: str
) -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings({"decomposition": {"threshold": value}})

    assert "runtime.complexity.decomposition.threshold" in str(excinfo.value)
    assert needle in str(excinfo.value)


@pytest.mark.parametrize(
    ("key", "value", "needle"),
    [
        ("max_depth", 1.5, "must be a whole number"),
        ("max_depth", -1, "must be 0 or greater"),
        ("max_chunks", "eight", "must be a whole number"),
        ("max_chunks", 0, "must be 1 or greater"),
    ],
)
def test_malformed_integer_limits_are_rejected(
    key: str, value: object, needle: str
) -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings({"decomposition": {key: value}})

    assert f"runtime.complexity.decomposition.{key}" in str(excinfo.value)
    assert needle in str(excinfo.value)


def test_whole_number_float_limits_are_accepted() -> None:
    settings = parse_complexity_settings({"decomposition": {"max_depth": 3.0}})

    assert settings.decomposition.max_depth == 3


def test_unknown_on_failure_lists_the_choices() -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings({"decomposition": {"on_failure": "explode"}})

    message = str(excinfo.value)
    assert "on_failure must be one of route_up, fail" in message


def test_bands_must_be_a_list() -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings({"routing": {"bands": {"max": 10}}})

    assert "runtime.complexity.routing.bands must be an array" in str(excinfo.value)


def test_empty_band_table_is_rejected() -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings({"routing": {"bands": []}})

    assert "must not be empty" in str(excinfo.value)


def test_band_table_without_catch_all_is_rejected() -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings(
            {"routing": {"bands": [{"max": 40, "model": "small"}]}}
        )

    assert "must end with a catch-all band" in str(excinfo.value)


def test_catch_all_before_the_end_is_rejected() -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings(
            {
                "routing": {
                    "bands": [{"model": "large"}, {"max": 40, "model": "small"}]
                }
            }
        )

    assert "is a catch-all band (no 'max') but is not last" in str(excinfo.value)


@pytest.mark.parametrize(
    "bands",
    [
        # descending
        [
            {"max": 75, "model": "medium"},
            {"max": 40, "model": "small"},
            {"model": "large"},
        ],
        # overlapping (duplicate boundary)
        [
            {"max": 40, "model": "small"},
            {"max": 40, "model": "medium"},
            {"model": "large"},
        ],
    ],
)
def test_unordered_or_overlapping_bands_are_rejected(
    bands: list[dict[str, object]],
) -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings({"routing": {"bands": bands}})

    assert "must be ordered by ascending 'max'" in str(excinfo.value)


def test_band_missing_model_is_rejected() -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings({"routing": {"bands": [{"max": 40}]}})

    assert "runtime.complexity.routing.bands[0] is missing required field 'model'" in (
        str(excinfo.value)
    )


def test_band_with_non_numeric_max_is_rejected() -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings(
            {"routing": {"bands": [{"max": "forty", "model": "small"}]}}
        )

    assert "runtime.complexity.routing.bands[0].max must be a number" in str(
        excinfo.value
    )


def test_band_with_unknown_key_is_rejected() -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings(
            {"routing": {"bands": [{"max": 40, "model": "s", "tier": "x"}]}}
        )

    assert "runtime.complexity.routing.bands[0]: unknown key 'tier'" in str(
        excinfo.value
    )


def test_routing_enabled_without_bands_is_rejected() -> None:
    with pytest.raises(ComplexityConfigError) as excinfo:
        parse_complexity_settings(
            {"scoring": {"enabled": True}, "routing": {"enabled": True}}
        )

    assert "no bands are defined" in str(excinfo.value)


def test_bands_validate_even_while_routing_is_disabled() -> None:
    with pytest.raises(ComplexityConfigError):
        parse_complexity_settings(
            {"routing": {"enabled": False, "bands": [{"max": 40, "model": "small"}]}}
        )


def test_complexity_config_error_is_a_config_error() -> None:
    assert issubclass(ComplexityConfigError, ConfigError)
    assert issubclass(ComplexityConfigError, ValueError)


# --------------------------------------------------------------------------
# resolution through effective settings (precedence + provenance)
# --------------------------------------------------------------------------


def test_orchestration_complexity_overrides_config_complexity() -> None:
    cfg = CircuitryConfig(
        runtime=_complexity(
            scoring={"enabled": True},
            decomposition={"enabled": True, "threshold": 30},
        )
    )
    orch = {
        "runtime": _complexity(
            scoring={"enabled": True},
            decomposition={"enabled": True, "threshold": 90, "max_chunks": 3},
        )
    }

    effective = resolve_effective_settings(cfg=cfg, orch=orch)

    assert effective.complexity.decomposition.threshold == 90.0
    assert effective.complexity.decomposition.max_chunks == 3
    assert effective.sources["complexity"] == "orchestration"
    assert effective.sources["complexity.decomposition"] == "orchestration"


def test_config_complexity_wins_when_orchestration_is_silent() -> None:
    cfg = CircuitryConfig(runtime=_complexity(scoring={"enabled": True}))

    effective = resolve_effective_settings(cfg=cfg, orch={"runtime": {"plugins": {}}})

    assert effective.complexity.scoring.enabled is True
    assert effective.sources["complexity"] == "config"
    assert effective.sources["complexity.scoring"] == "config"
    assert effective.sources["complexity.routing"] == "default"


def test_orchestration_block_replaces_the_config_block_wholesale() -> None:
    # Documented consequence of the shallow runtime merge: the orchestration
    # block does not inherit sub-blocks the config file defined.
    cfg = CircuitryConfig(
        runtime=_complexity(
            scoring={"enabled": True, "keywords": {"migrate": 3}},
            decomposition={"enabled": True},
        )
    )
    orch = {"runtime": _complexity(scoring={"enabled": True})}

    effective = resolve_effective_settings(cfg=cfg, orch=orch)

    assert effective.complexity.scoring.keywords == {}
    assert effective.complexity.decomposition.enabled is False
    assert effective.sources["complexity.decomposition"] == "default"


def test_complexity_rides_the_existing_runtime_merge() -> None:
    # No parallel plumbing: the block lands in the merged runtime dict itself,
    # next to every other runtime.* key.
    cfg = CircuitryConfig(runtime={"adapters": {"ollama": {"base_url": "x"}}})
    orch = {"runtime": _complexity(scoring={"enabled": True})}

    effective = resolve_effective_settings(cfg=cfg, orch=orch)

    assert effective.runtime["complexity"]["scoring"]["enabled"] is True
    assert effective.runtime["adapters"]["ollama"]["base_url"] == "x"
    assert resolve_complexity_settings(effective.runtime) == effective.complexity


def test_invalid_block_fails_at_effective_settings_resolution() -> None:
    cfg = CircuitryConfig(runtime=_complexity(decomposition={"threshold": "high"}))

    with pytest.raises(ComplexityConfigError):
        resolve_effective_settings(cfg=cfg, orch={})


# --------------------------------------------------------------------------
# CLI surface: a broken block reads like a sentence, not a traceback
# --------------------------------------------------------------------------


def test_invalid_band_table_fails_the_cli_with_a_readable_message(
    tmp_path: Path,
) -> None:
    config_path = tmp_path / "circuitry.config.json"
    config_path.write_text(
        json.dumps(
            {
                "runtime": _complexity(
                    scoring={"enabled": True},
                    routing={
                        "enabled": True,
                        "bands": [
                            {"max": 75, "model": "medium"},
                            {"max": 40, "model": "small"},
                            {"model": "large"},
                        ],
                    },
                )
            }
        ),
        encoding="utf-8",
    )

    result = CliRunner().invoke(app, ["doctor", "-c", str(config_path)])

    assert result.exit_code == 1
    output = result.output + str(getattr(result, "stderr", "") or "")
    assert "runtime.complexity.routing.bands" in output
    assert "ascending" in output
    assert "Traceback" not in output
