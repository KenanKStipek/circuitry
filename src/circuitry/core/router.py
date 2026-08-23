"""Score to model — the routing decision, and the rule about who wins.

:mod:`circuitry.core.complexity` answers "how hard is this prompt";
:mod:`circuitry.cli.complexity_config` owns what a band table looks like and
which row a score falls in (:func:`~circuitry.cli.complexity_config.band_for`).
This module is the third, smallest piece: given a score, a validated
``routing`` block, and whether the model was already chosen on purpose, it
returns the model to dispatch — or ``None``, meaning "the router has no
opinion, use what you already resolved".

The whole feature is that ``None``. Routing is an *automatic* choice, and an
automatic choice that can quietly overrule a deliberate one is a bug report
waiting to happen, so deferring is the router's default answer in every case
it is not certain: switch off, no table, or a model a human already named.

Precedence, in the order a run resolves it:

``--model`` > per-effect ``model:`` > profile effect override > **router** >
orchestration default > config default

The first three are what *explicit* means below. They are already collapsed
into one boolean by the time they reach here — the caller knows whether the
model it holds was pinned by a human — because the router's rule does not
distinguish between them: all three outrank it, and each stays recorded as
itself (``meta.model_reason``, ``sources["model"]``) at the layer that set it.

Model names pass through untouched. For the ``cyberdiner`` adapter a band's
``model`` is a tier name and expo is the authority; for local adapters it is a
real model name. The router resolves a band to a string and does not interpret
it — the same string an effect could have written in its own ``model:``.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from ..cli.complexity_config import ComplexityBand, RoutingSettings

__all__ = ["RouteDecision", "route_model"]


@dataclass(frozen=True)
class RouteDecision:
    """A model the router chose, and the band row it read it off.

    Both halves are recorded on the effect node: the model as ``meta.model``
    (with ``meta.model_reason == "router"``), the band as
    ``meta.complexity.band``. Keeping them together is what makes a routed
    dispatch explainable after the fact — the model alone does not say *why*.
    """

    model: str
    band: ComplexityBand


def route_model(
    *,
    score: float,
    settings: RoutingSettings,
    explicit: bool,
) -> RouteDecision | None:
    """The model *score* routes to, or ``None`` when the router defers.

    ``None`` — leave the caller's already-resolved model alone — covers every
    case the router is not entitled to decide:

    * ``settings.enabled`` is false, or the table is empty (routing is off, and
      an off switch must leave model selection byte-identical to a build
      without the feature);
    * *explicit* is true and ``settings.respect_explicit`` is true (the
      default): a human named this model, at the CLI, on the effect, or in a
      profile, and the router does not argue with that.

    Setting ``respect_explicit: false`` is the documented opt-out, and it means
    what it says: the router then decides for every scored prompt effect,
    including ones that name their own ``model:``. It is the knob for "this run
    routes, full stop" — a sweep, a cost experiment — and it is off by default
    precisely because overruling a deliberate choice is the surprising
    behaviour.

    A non-empty band table always ends in a catch-all (config resolution
    enforces it), so once the router *is* entitled to decide, it always
    produces a model — there is no "score fell off the end" case for a caller
    to handle.
    """
    if not settings.enabled or not settings.bands:
        return None
    if explicit and settings.respect_explicit:
        return None

    # Imported lazily: ``core`` reaching into ``cli`` at module scope would
    # invert the dependency the rest of this package maintains.
    from ..cli.complexity_config import band_for

    band = band_for(score, settings.bands)
    if band is None or not band.model:
        return None
    return RouteDecision(model=band.model, band=band)
