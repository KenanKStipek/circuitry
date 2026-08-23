"""What the router actually dispatches, at the one seam the model is resolved.

``test_router.py`` covers the decision in isolation; this file covers the half
that can only be shown by running something: that the chosen model reaches the
adapter, that it becomes the default model for the provider fallback chain,
that it is recorded on the node, and — the criterion with the most surface area
— that it never displaces a model a human named.

Band tables here are built around a *measured* score rather than a guessed one
(:func:`_score_of`), so a test says "route this effect low" or "route it high"
and stays true if the scorer's arithmetic changes. The one thing never asserted
is a specific score: that is ``test_prompt_complexity.py``'s subject.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store

RUN_DEFAULT = "run-default-model"


@dataclass
class RecordingAdapter:
    """Echoes the model it was called with, and keeps the receipts.

    Both halves matter: ``meta.model`` says what the runtime *decided*, and
    ``calls`` says what it actually *sent*. A router that updated the metadata
    without moving the dispatch would pass every assertion about the former.
    """

    name: str = "primary"
    calls: list[tuple[str, str]] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append((self.name, model))
        return GenerateResult(text=f"{self.name}:{model}", raw={})


@dataclass
class FailingAdapter:
    name: str = "primary"
    calls: list[tuple[str, str]] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append((self.name, model))
        raise RuntimeError(f"{self.name} outage")


def _prompt_orch(**overrides: Any) -> dict[str, Any]:
    effect: dict[str, Any] = {
        "type": "prompt",
        "name": "task",
        "template": "Analyze {{topic}} and cross-reference it against {{source}}.",
    }
    effect.update(overrides)
    return {"effects": [effect]}


def _run(
    orch: dict[str, Any],
    *,
    adapter: Any = None,
    runtime_config: dict[str, Any] | None = None,
    model: str = RUN_DEFAULT,
    model_locked: bool = False,
    state: dict[str, Any] | None = None,
) -> Store:
    store = Store(dict(state or {}))
    DynamicRuntime(
        compile_orchestration(orch=orch, root_name="prime"),
        adapter=adapter if adapter is not None else RecordingAdapter(),
        model=model,
        model_locked=model_locked,
        runtime_config=runtime_config,
    ).execute(store=store)
    return store


def _scoring_only(**routing: Any) -> dict[str, Any]:
    block: dict[str, Any] = {"scoring": {"enabled": True}}
    if routing:
        block["routing"] = routing
    return {"complexity": block}


def _score_of(orch: dict[str, Any], path: str = "prime.task.meta") -> float:
    """The score this effect actually gets, so a band table can straddle it."""
    store = _run(orch, runtime_config=_scoring_only())
    return float(store.get(f"{path}.complexity.score"))


def _routes_to(orch: dict[str, Any], *, low: str, high: str) -> dict[str, Any]:
    """A two-band table split just below this effect's score.

    The effect therefore lands in the *high* band, and moving the split above
    its score (``_routes_below``) moves it to the low one — the same table,
    the same effect, a different answer.
    """
    return _scoring_only(
        enabled=True,
        bands=[
            {"name": "low", "max": _score_of(orch) - 1.0, "model": low},
            {"name": "high", "model": high},
        ],
    )


CATCH_ALL_ONLY: dict[str, Any] = _scoring_only(
    enabled=True, bands=[{"name": "everything", "model": "routed-model"}]
)


# --------------------------------------------------------------------------
# the router decides
# --------------------------------------------------------------------------


def test_the_router_replaces_the_run_default_and_says_so() -> None:
    adapter = RecordingAdapter()
    store = _run(_prompt_orch(), adapter=adapter, runtime_config=CATCH_ALL_ONLY)

    assert store.get("prime.task.meta.model") == "routed-model"
    assert store.get("prime.task.meta.model_reason") == "router"
    assert store.get("prime.task.meta.complexity.band") == {
        "name": "everything",
        "model": "routed-model",
    }
    # The decision is not just metadata: it is what was sent.
    assert adapter.calls == [("primary", "routed-model")]
    assert store.get("prime.task.value") == "primary:routed-model"


def test_the_score_is_what_picks_the_band() -> None:
    """Same table, same effect, split moved across its score: the answer
    flips. Without this the router could be ignoring the score entirely and
    every other test here would still pass."""
    orch = _prompt_orch()
    score = _score_of(orch)

    def table(split: float) -> dict[str, Any]:
        return _scoring_only(
            enabled=True,
            bands=[
                {"name": "low", "max": split, "model": "small-model"},
                {"name": "high", "model": "big-model"},
            ],
        )

    below = _run(orch, runtime_config=table(score - 1.0))
    above = _run(orch, runtime_config=table(score + 1.0))

    assert below.get("prime.task.meta.model") == "big-model"
    assert below.get("prime.task.meta.complexity.band")["name"] == "high"
    assert above.get("prime.task.meta.model") == "small-model"
    assert above.get("prime.task.meta.complexity.band")["name"] == "low"


def test_a_score_exactly_on_a_boundary_stays_in_the_lower_band() -> None:
    """The documented inclusive bound, asserted through a real dispatch and
    not just the band walk: `max` is the score's own value here."""
    orch = _prompt_orch()
    store = _run(
        orch,
        runtime_config=_scoring_only(
            enabled=True,
            bands=[
                {"name": "low", "max": _score_of(orch), "model": "small-model"},
                {"name": "high", "model": "big-model"},
            ],
        ),
    )

    assert store.get("prime.task.meta.model") == "small-model"


def test_effects_in_one_run_route_independently() -> None:
    """Routing is per effect, not per run — the whole point of routing rather
    than just picking a model."""
    orch = {
        "effects": [
            {"type": "prompt", "name": "tiny", "template": "hi"},
            {
                "type": "prompt",
                "name": "big",
                "template": (
                    "Analyze {{corpus}} against {{rules}}, reconcile with "
                    "{{prior}}, and justify every decision.\n"
                    + "Context: {{background}}\n" * 40
                ),
            },
        ]
    }
    tiny_score = _score_of(orch, "prime.tiny.meta")
    big_score = _score_of(orch, "prime.big.meta")
    assert tiny_score < big_score, "fixture no longer discriminates"

    split = (tiny_score + big_score) / 2
    adapter = RecordingAdapter()
    store = _run(
        orch,
        adapter=adapter,
        runtime_config=_scoring_only(
            enabled=True,
            bands=[
                {"name": "cheap", "max": split, "model": "small-model"},
                {"name": "capable", "model": "big-model"},
            ],
        ),
    )

    assert store.get("prime.tiny.meta.model") == "small-model"
    assert store.get("prime.big.meta.model") == "big-model"
    assert adapter.calls == [("primary", "small-model"), ("primary", "big-model")]


@pytest.mark.parametrize(
    ("container", "path"),
    [
        pytest.param(
            {
                "type": "dynamic",
                "name": "sub",
                "effects": [{"type": "prompt", "name": "task", "template": "go"}],
            },
            "prime.sub.task",
            id="dynamic",
        ),
        pytest.param(
            {
                "type": "conditional",
                "name": "gate",
                "if": {"mode": "cel", "expr": "true"},
                "then": [{"type": "prompt", "name": "task", "template": "go"}],
            },
            "prime.gate.task",
            id="conditional",
        ),
        pytest.param(
            {
                "type": "loop",
                "name": "spin",
                "each": {"in": "input.items", "as": "item"},
                "body": [{"type": "prompt", "name": "task", "template": "{{item}}"}],
            },
            "prime.spin.iter_0.task",
            id="loop",
        ),
    ],
)
def test_the_router_reaches_prompts_inside_containers(
    container: dict[str, Any], path: str
) -> None:
    """Containers carry the decision down rather than making their own: a
    prompt is routed the same whether it sits at the top level or three
    levels into a loop body."""
    store = _run(
        {"effects": [container]},
        runtime_config=CATCH_ALL_ONLY,
        state={"input": {"items": ["a"]}},
    )

    assert store.get(f"{path}.meta.model") == "routed-model"
    assert store.get(f"{path}.meta.model_reason") == "router"


# --------------------------------------------------------------------------
# the router defers
# --------------------------------------------------------------------------


def test_a_per_effect_model_beats_the_router() -> None:
    """And the band is still recorded — "what would routing have picked" is
    exactly the question you ask before removing the pin."""
    adapter = RecordingAdapter()
    store = _run(
        _prompt_orch(model="pinned-model"),
        adapter=adapter,
        runtime_config=CATCH_ALL_ONLY,
    )

    assert store.get("prime.task.meta.model") == "pinned-model"
    assert store.get("prime.task.meta.model_reason") == "explicit"
    assert adapter.calls == [("primary", "pinned-model")]
    assert store.get("prime.task.meta.complexity.band") == {
        "name": "everything",
        "model": "routed-model",
    }


def test_a_locked_run_default_beats_the_router() -> None:
    """`--model` and a profile's run-level `model:` arrive as
    ``model_locked``; below the CLI they are indistinguishable strings, which
    is why the flag exists at all."""
    adapter = RecordingAdapter()
    store = _run(
        _prompt_orch(),
        adapter=adapter,
        runtime_config=CATCH_ALL_ONLY,
        model="cli-model",
        model_locked=True,
    )

    assert store.get("prime.task.meta.model") == "cli-model"
    assert store.get("prime.task.meta.model_reason") == "default"
    assert adapter.calls == [("primary", "cli-model")]


def test_an_unlocked_run_default_does_not_beat_the_router() -> None:
    """The other half of the flag: an orchestration- or config-supplied
    default is exactly what the router is there to replace."""
    store = _run(
        _prompt_orch(),
        runtime_config=CATCH_ALL_ONLY,
        model="orch-model",
        model_locked=False,
    )

    assert store.get("prime.task.meta.model") == "routed-model"


def test_respect_explicit_false_overrules_both_kinds_of_explicit() -> None:
    routing = _scoring_only(
        enabled=True,
        respect_explicit=False,
        bands=[{"name": "everything", "model": "routed-model"}],
    )

    pinned_effect = _run(_prompt_orch(model="pinned-model"), runtime_config=routing)
    pinned_run = _run(
        _prompt_orch(), runtime_config=routing, model="cli-model", model_locked=True
    )

    assert pinned_effect.get("prime.task.meta.model") == "routed-model"
    assert pinned_effect.get("prime.task.meta.model_reason") == "router"
    assert pinned_run.get("prime.task.meta.model") == "routed-model"
    assert pinned_run.get("prime.task.meta.model_reason") == "router"


def test_a_scoring_failure_leaves_the_model_alone(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """No score, no route. A diagnostic that cannot be produced must not
    become a reason the effect runs on some other model than it would have."""

    def boom(*args: Any, **kwargs: Any) -> Any:
        raise RuntimeError("scorer exploded")

    monkeypatch.setattr("circuitry.core.complexity.score", boom)

    adapter = RecordingAdapter()
    store = _run(_prompt_orch(), adapter=adapter, runtime_config=CATCH_ALL_ONLY)

    assert "complexity" not in store.get("prime.task.meta")
    assert store.get("prime.task.meta.model") == RUN_DEFAULT
    assert store.get("prime.task.meta.model_reason") == "default"
    assert adapter.calls == [("primary", RUN_DEFAULT)]


# --------------------------------------------------------------------------
# the fallback chain
# --------------------------------------------------------------------------


def test_the_routed_model_becomes_the_default_for_the_attempt_chain(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A bare `provider:` token carries no model of its own, so it inherits
    the effect's resolved one — which, once routing is on, is the routed one.
    `provider`/`provider_fallbacks` behave exactly as before; the only thing
    that changed underneath them is what "the effect's model" means."""
    secondary = RecordingAdapter(name="secondary")
    monkeypatch.setattr(
        "circuitry.core.prompt.build_adapter",
        lambda *, adapter_name, runtime: secondary,
    )

    primary = FailingAdapter(name="primary")
    store = _run(
        _prompt_orch(provider_fallbacks=["secondary"]),
        adapter=primary,
        runtime_config=CATCH_ALL_ONLY,
    )

    # Both attempts used the routed model, not the run default.
    assert primary.calls == [("primary", "routed-model")]
    assert secondary.calls == [("secondary", "routed-model")]
    attempts = store.get("prime.task.meta.fallback_attempts")
    assert [(a["adapter"], a["model"]) for a in attempts] == [
        ("primary", "routed-model"),
        ("secondary", "routed-model"),
    ]
    assert [a["status"] for a in attempts] == ["failed", "succeeded"]
    assert store.get("prime.task.meta.fallback_recovered") is True


def test_a_fallback_that_pins_its_own_model_is_untouched_by_routing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """`provider:model` names both halves. The router substitutes the default
    model, and a token that supplies its own never asked for one."""
    secondary = RecordingAdapter(name="secondary")
    monkeypatch.setattr(
        "circuitry.core.prompt.build_adapter",
        lambda *, adapter_name, runtime: secondary,
    )

    primary = FailingAdapter(name="primary")
    _run(
        _prompt_orch(provider_fallbacks=["secondary:backup-model"]),
        adapter=primary,
        runtime_config=CATCH_ALL_ONLY,
    )

    assert primary.calls == [("primary", "routed-model")]
    assert secondary.calls == [("secondary", "backup-model")]


def test_a_primary_provider_override_also_takes_the_routed_model(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """`provider:` moves the *transport*, not the model."""
    other = RecordingAdapter(name="other")
    monkeypatch.setattr(
        "circuitry.core.prompt.build_adapter",
        lambda *, adapter_name, runtime: other,
    )

    store = _run(
        _prompt_orch(provider="other"),
        runtime_config=CATCH_ALL_ONLY,
    )

    assert other.calls == [("other", "routed-model")]
    assert store.get("prime.task.meta.model") == "routed-model"


# --------------------------------------------------------------------------
# routing off
# --------------------------------------------------------------------------


def _strip_timestamps(value: Any) -> Any:
    if isinstance(value, dict):
        return {
            key: _strip_timestamps(item)
            for key, item in value.items()
            if key not in ("created_at", "completed_at")
        }
    if isinstance(value, list):
        return [_strip_timestamps(item) for item in value]
    return value


@pytest.mark.parametrize(
    "runtime_config",
    [
        pytest.param(None, id="no-complexity-block"),
        pytest.param({"complexity": {"scoring": {"enabled": False}}}, id="all-off"),
        pytest.param(
            {
                "complexity": {
                    "scoring": {"enabled": False},
                    # A fully-specified table that is simply switched off —
                    # the case a user is in the moment before they flip it.
                    "routing": {
                        "enabled": False,
                        "bands": [
                            {"name": "low", "max": 40, "model": "small-model"},
                            {"name": "high", "model": "big-model"},
                        ],
                    },
                }
            },
            id="table-present-switch-off",
        ),
    ],
)
def test_with_routing_disabled_model_selection_is_identical_to_today(
    runtime_config: dict[str, Any] | None,
) -> None:
    """The acceptance criterion a per-key assertion cannot carry: the whole
    state tree, serialized, is the same as a run with no complexity config at
    all — so nothing about the model was decided differently, and no key
    appeared to say so."""
    orch = {
        "effects": [
            {"type": "prompt", "name": "plain", "template": "go"},
            {"type": "prompt", "name": "pinned", "template": "go", "model": "pin"},
        ]
    }
    reference = _run(orch)
    store = _run(orch, runtime_config=runtime_config)

    assert store.get("prime.plain.meta.model") == RUN_DEFAULT
    assert store.get("prime.plain.meta.model_reason") == "default"
    assert store.get("prime.pinned.meta.model") == "pin"

    serialized = json.dumps(_strip_timestamps(store.state), sort_keys=True)
    assert serialized == json.dumps(
        _strip_timestamps(reference.state), sort_keys=True
    )
    assert "router" not in serialized
