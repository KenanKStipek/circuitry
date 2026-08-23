"""The routing decision itself: which band a score lands in, and who wins.

Two separate contracts live in :func:`circuitry.core.router.route_model`, and
they fail in different ways, so they are tested apart:

* *Which band* — a walk over an ordered table with an inclusive upper bound.
  The failure mode is off-by-one at a boundary, so the boundaries are asserted
  literally rather than sampled from the middle of each band.
* *Whether to decide at all* — the deferral rule. The failure mode here is the
  expensive one: an automatic choice quietly overruling a deliberate one.

Dispatch-level behaviour (what a run actually sends, and what lands on the
state node) is in ``test_prompt_routing.py``; this file never runs an effect.
"""

from __future__ import annotations

import pytest

from circuitry.cli.complexity_config import (
    ComplexityBand,
    RoutingSettings,
    parse_complexity_settings,
)
from circuitry.core.router import route_model

#: Two bounded bands and the required catch-all, with the boundaries at round
#: numbers so an assertion reads as the arithmetic it is checking.
BANDS: tuple[ComplexityBand, ...] = (
    ComplexityBand(name="light", max=40.0, model="small-model"),
    ComplexityBand(name="medium", max=70.0, model="mid-model"),
    ComplexityBand(name="heavy", model="big-model"),
)


def _routing(**overrides: object) -> RoutingSettings:
    defaults: dict[str, object] = {
        "enabled": True,
        "bands": BANDS,
        "respect_explicit": True,
    }
    defaults.update(overrides)
    return RoutingSettings(**defaults)  # type: ignore[arg-type]


def _model_for(score: float, **overrides: object) -> str | None:
    decision = route_model(
        score=score, settings=_routing(**overrides), explicit=False
    )
    return decision.model if decision is not None else None


@pytest.mark.parametrize(
    ("score", "expected"),
    [
        # The documented boundary: `max` is *inclusive*, so a score of exactly
        # 40 is in the band whose max is 40 — not the next one up. Both sides
        # of both boundaries are pinned, because "inclusive" is only a claim
        # about these four numbers.
        (39.999, "small-model"),
        (40.0, "small-model"),
        (40.001, "mid-model"),
        (69.999, "mid-model"),
        (70.0, "mid-model"),
        (70.001, "big-model"),
        # The ends of the score range, which no boundary sits on.
        (0.0, "small-model"),
        (100.0, "big-model"),
    ],
)
def test_inclusive_upper_bound_at_every_boundary(
    score: float, expected: str
) -> None:
    assert _model_for(score) == expected


def test_the_catch_all_takes_everything_above_the_last_bound() -> None:
    """The catch-all is what makes "every score resolves to a model" true.

    Config resolution requires it precisely so this function has no "fell off
    the end" branch — asserted here from the other side, on a score higher than
    any bound in the table.
    """
    decision = route_model(score=99.5, settings=_routing(), explicit=False)
    assert decision is not None
    assert decision.band.is_catch_all
    assert decision.model == "big-model"


def test_the_decision_carries_the_band_it_was_read_off() -> None:
    """The model alone does not say *why*; the band is what explains it."""
    decision = route_model(score=55.0, settings=_routing(), explicit=False)
    assert decision is not None
    assert decision.band.name == "medium"
    assert decision.band.max == 70.0
    assert decision.model == decision.band.model


def test_a_single_catch_all_table_routes_every_score_to_one_model() -> None:
    only = (ComplexityBand(model="only-model"),)
    for score in (0.0, 50.0, 100.0):
        decision = route_model(
            score=score, settings=_routing(bands=only), explicit=False
        )
        assert decision is not None and decision.model == "only-model"


@pytest.mark.parametrize(
    "settings",
    [
        pytest.param(_routing(enabled=False), id="switch-off"),
        pytest.param(_routing(enabled=False, bands=()), id="off-and-empty"),
        pytest.param(_routing(bands=()), id="on-but-no-table"),
    ],
)
def test_the_router_defers_when_it_is_not_configured(
    settings: RoutingSettings,
) -> None:
    """``None`` means "keep the model you already resolved" — the property
    that makes a disabled router byte-identical to no router at all."""
    assert route_model(score=90.0, settings=settings, explicit=False) is None


def test_an_explicit_choice_wins_by_default() -> None:
    """`--model`, a per-effect `model:`, and a profile effect override reach
    here as one boolean, because the rule does not rank them: each outranks
    the router identically."""
    assert route_model(score=90.0, settings=_routing(), explicit=True) is None


def test_respect_explicit_false_is_the_documented_opt_out() -> None:
    """The knob means what it says: the router then decides for every scored
    effect, including ones a human named a model for."""
    decision = route_model(
        score=90.0, settings=_routing(respect_explicit=False), explicit=True
    )
    assert decision is not None
    assert decision.model == "big-model"


def test_respect_explicit_false_still_defers_when_routing_is_off() -> None:
    """`respect_explicit` says who wins *among routers*; it is not a second
    way to turn routing on."""
    assert (
        route_model(
            score=90.0,
            settings=_routing(enabled=False, respect_explicit=False),
            explicit=True,
        )
        is None
    )


def test_bands_parsed_from_config_route_the_same_way() -> None:
    """The table a user writes and the table this function walks are the same
    object — no re-parsing, no second shape, in either direction."""
    settings = parse_complexity_settings(
        {
            "scoring": {"enabled": True},
            "routing": {
                "enabled": True,
                "bands": [
                    {"name": "light", "max": 40, "model": "small-model"},
                    {"name": "medium", "max": 70, "model": "mid-model"},
                    {"name": "heavy", "model": "big-model"},
                ],
            },
        }
    )

    assert settings.routing.bands == BANDS
    decision = route_model(
        score=40.0, settings=settings.routing, explicit=False
    )
    assert decision is not None and decision.model == "small-model"
