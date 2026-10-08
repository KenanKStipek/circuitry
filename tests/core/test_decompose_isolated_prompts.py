"""A generated decomposition/reflector plan's own `prompts:`/`{{> name}}`
(#396) are whatever its OWN document compiles to — `root.prompts`/
`root.effect_names` from `compile_orchestration(orch=<the plan's own
dict>)` — never inherited from the parent document that generated it: the
plan is compiled from its own dict alone, so there is structurally nothing
for the parent's declared prompts to leak through. The planner (taught
`prompts:`/`{{> name}}` via `WIZARD_PRIME_V1`) may generate a plan that is
self-consistent — declares its own and only ever references them — which
must run the way it compiled, not fail at run time for a name `cof check`
already accepted.
"""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

from circuitry.core.decompose import _run_isolated
from circuitry.core.prompt_compose import (
    EFFECT_NAMES_RUNTIME_KEY,
    RUNTIME_CONFIG_KEY,
)
from circuitry.core.store import Store


def _mock_adapter() -> MagicMock:
    adapter = MagicMock()
    adapter.name = "mock"
    result = MagicMock()
    result.text = "reply"
    result.raw = {}
    result.tokens_sent = 1
    result.tokens_received = 1
    adapter.generate.return_value = result
    return adapter


def _isolated(orch: dict, *, runtime_config: dict) -> Store:
    parent_store = Store(state={"input": {}, "prime": {}})
    return _run_isolated(
        orch,
        initial_state={"input": {}, "prime": {}},
        parent_store=parent_store,
        node={},
        node_path="decomp",
        adapter=_mock_adapter(),
        model="m",
        runtime_config=runtime_config,
        timeout_seconds=30,
        verbose=False,
        display_depth=0,
    )


def test_a_self_consistent_plans_own_declared_prompts_work() -> None:
    """A plan that declares and uses its own `prompts:` runs the way it
    compiled, instead of losing them to a hardcoded empty override."""
    child_store = _isolated(
        {
            "prompts": {"brief": "Keep it short."},
            "effects": [{"type": "yield", "name": "y", "template": "{{> brief}}!"}],
        },
        runtime_config={},
    )

    assert child_store.get("prime.y.value") == "Keep it short.!"
    assert child_store.get("prime.y.meta.error") is None


def test_an_isolated_plan_never_sees_the_parents_declared_prompts() -> None:
    """The parent's own declared prompts/effect names, carried on
    `runtime_config` for whatever reason, must not be visible to the
    plan's own run: the plan is compiled from its own dict alone, so a
    `{{> name}}` naming the parent's prompt is unknown to it, not merely
    shadowed."""
    runtime_config = {
        RUNTIME_CONFIG_KEY: {"leaked": "should never be visible"},
        EFFECT_NAMES_RUNTIME_KEY: frozenset({"leaked_effect"}),
    }

    child_store = _isolated(
        {"effects": [{"type": "yield", "name": "y", "template": "ok"}]},
        runtime_config=runtime_config,
    )

    assert child_store.get("prime.y.value") == "ok"
    # Confirms the override actually replaced, rather than merged with,
    # whatever the caller passed in.
    assert runtime_config[RUNTIME_CONFIG_KEY] == {"leaked": "should never be visible"}


def test_a_plan_referencing_the_parents_declared_prompt_by_name_is_unknown_to_it() -> None:
    """The plan is compiled from its own dict alone — `{{> leaked}}`, a name
    only the PARENT declares, is simply unknown to the plan, the same
    `cof check` error an author's own typo gets, not a run-time surprise."""
    runtime_config = {RUNTIME_CONFIG_KEY: {"leaked": "parent's own"}}

    with pytest.raises(ValueError, match=r"'\{\{> leaked\}\}' does not name"):
        _isolated(
            {"effects": [{"type": "yield", "name": "y", "template": "{{> leaked}}"}]},
            runtime_config=runtime_config,
        )
