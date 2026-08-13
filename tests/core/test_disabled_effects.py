"""Effect enable/disable semantics (issue #29).

``enabled: false`` comes from a profile's per-effect map and is applied to the
compiled tree by ``apply_effect_overrides``. These tests pin the observable
contract: the effect does not run, its node is a uniform skip marker, the
lifecycle hook still fires, and downstream templates/conditions stay coherent.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

import chevron
import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import apply_effect_overrides, compile_orchestration
from circuitry.core.disabled import is_disabled_node
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class RecordingAdapter:
    """Adapter that records every generate() call it receives."""

    name: str = "rec"
    text: str = "OUT"
    calls: list[tuple[str, str]] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append((model, prompt))
        return GenerateResult(text=self.text, raw={})


def _run(
    orch: dict[str, Any],
    overrides: dict[str, dict[str, Any]],
    *,
    adapter: RecordingAdapter | None = None,
    effect_paths: list[str] | None = None,
) -> tuple[dict[str, Any], RecordingAdapter]:
    """Compile *orch*, apply *overrides*, execute, and return (state, adapter)."""
    adapter = adapter or RecordingAdapter()
    root = compile_orchestration(orch=orch, root_name="prime")
    root, _ = apply_effect_overrides(root, overrides)
    store = Store(
        {},
        effect_complete=(
            (lambda path, node: effect_paths.append(path))
            if effect_paths is not None
            else None
        ),
    )
    DynamicRuntime(root, adapter=adapter, model="run-default").execute(store=store)
    return store.state, adapter


def _assert_disabled_node(node: Any) -> None:
    """Pin the skip-node shape: value None + exactly the disabled meta keys."""
    assert isinstance(node, dict)
    assert node["value"] is None
    assert node["meta"]["disabled"] is True
    assert set(node["meta"]) == {"disabled", "created_at", "completed_at"}
    assert node["meta"]["created_at"] is not None
    assert node["meta"]["completed_at"] is not None
    assert is_disabled_node(node)


# ── prompt ───────────────────────────────────────────────────────────────────


def test_disabled_prompt_is_not_executed_and_writes_skip_node() -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "a", "template": "a-template"},
            {"type": "prompt", "name": "b", "template": "b-template"},
        ]
    }
    state, adapter = _run(orch, {"a": {"enabled": False}})

    _assert_disabled_node(state["prime"]["a"])
    assert [prompt for _, prompt in adapter.calls] == ["b-template"]
    assert state["prime"]["b"]["value"] == "OUT"


def test_disabled_prompt_still_fires_effect_complete() -> None:
    orch = {"effects": [{"type": "prompt", "name": "a", "template": "t"}]}
    paths: list[str] = []
    _run(orch, {"a": {"enabled": False}}, effect_paths=paths)

    # Observability sees the skip, not a gap.
    assert "prime.a" in paths


def test_disabled_prompt_in_tree_flow_skips_only_that_branch() -> None:
    orch = {
        "flow": "tree",
        "effects": [
            {"type": "prompt", "name": "a", "template": "a-template"},
            {"type": "prompt", "name": "b", "template": "b-template"},
        ],
    }
    state, adapter = _run(orch, {"a": {"enabled": False}})

    _assert_disabled_node(state["prime"]["a"])
    assert [prompt for _, prompt in adapter.calls] == ["b-template"]


# ── tool / use ───────────────────────────────────────────────────────────────


def test_disabled_tool_is_not_executed() -> None:
    # An unknown provider would raise if the tool actually ran.
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "fetch",
                "provider": "definitely-not-a-real-provider",
                "params": {},
            }
        ]
    }
    state, _ = _run(orch, {"fetch": {"enabled": False}})
    _assert_disabled_node(state["prime"]["fetch"])


def test_disabled_use_is_not_resolved() -> None:
    # A missing path would raise during resolution if the use actually ran.
    orch = {
        "effects": [
            {"type": "use", "name": "child", "path": "no/such/orchestration.yml"}
        ]
    }
    state, _ = _run(orch, {"child": {"enabled": False}})
    _assert_disabled_node(state["prime"]["child"])


# ── containers: dynamic / loop / conditional / reflector ─────────────────────


def test_disabled_dynamic_disables_whole_subtree() -> None:
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "sub",
                "effects": [
                    {"type": "prompt", "name": "inner_a", "template": "ia"},
                    {"type": "prompt", "name": "inner_b", "template": "ib"},
                ],
            },
            {"type": "prompt", "name": "after", "template": "after"},
        ]
    }
    state, adapter = _run(orch, {"sub": {"enabled": False}})

    _assert_disabled_node(state["prime"]["sub"])
    # Nothing inside ran, so no child nodes exist at all.
    assert "inner_a" not in state["prime"]["sub"]
    assert "inner_b" not in state["prime"]["sub"]
    assert [prompt for _, prompt in adapter.calls] == ["after"]


def test_disabled_dynamic_marks_descendants_disabled_in_compiled_tree() -> None:
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "sub",
                "effects": [{"type": "prompt", "name": "inner", "template": "i"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    disabled, matched = apply_effect_overrides(root, {"sub": {"enabled": False}})

    assert matched == {"sub"}
    assert disabled.effects[0].enabled is False
    assert disabled.effects[0].effects[0].enabled is False
    # Frozen semantics: the original tree is untouched.
    assert root.effects[0].enabled is True
    assert root.effects[0].effects[0].enabled is True


def test_disabled_loop_never_iterates() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "each_item",
                "each": {"in": "items", "as": "item"},
                "body": [{"type": "prompt", "name": "step", "template": "{{item}}"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    root, _ = apply_effect_overrides(root, {"each_item": {"enabled": False}})
    adapter = RecordingAdapter()
    store = Store({"items": ["x", "y", "z"]})
    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    _assert_disabled_node(store.state["prime"]["each_item"])
    assert adapter.calls == []
    assert "iter_0" not in store.state["prime"]["each_item"]


def test_disabled_body_effect_skips_in_every_iteration() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "each_item",
                "each": {"in": "items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "kept", "template": "keep {{item}}"},
                    {"type": "prompt", "name": "dropped", "template": "drop {{item}}"},
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    root, _ = apply_effect_overrides(root, {"each_item.dropped": {"enabled": False}})
    adapter = RecordingAdapter()
    store = Store({"items": ["x", "y"]})
    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    loop_node = store.state["prime"]["each_item"]
    for i in range(2):
        _assert_disabled_node(loop_node[f"iter_{i}"]["dropped"])
        assert loop_node[f"iter_{i}"]["kept"]["value"] == "OUT"
    assert [prompt for _, prompt in adapter.calls] == ["keep x", "keep y"]


def test_disabled_collect_target_yields_empty_collected() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "each_item",
                "each": {"in": "items", "as": "item"},
                "collect": "step",
                "body": [{"type": "prompt", "name": "step", "template": "{{item}}"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    root, _ = apply_effect_overrides(root, {"each_item.step": {"enabled": False}})
    store = Store({"items": ["x", "y"]})
    DynamicRuntime(root, adapter=RecordingAdapter(), model="m").execute(store=store)

    loop_node = store.state["prime"]["each_item"]
    # The slot exists in state (value None, disabled) …
    _assert_disabled_node(loop_node["iter_0"]["step"])
    # … but `collected` reports only values that were actually produced.
    assert loop_node["collected"]["value"] == []


def test_collect_is_unaffected_when_target_stays_enabled() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "each_item",
                "each": {"in": "items", "as": "item"},
                "collect": "step",
                "body": [{"type": "prompt", "name": "step", "template": "{{item}}"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"items": ["x", "y"]})
    DynamicRuntime(root, adapter=RecordingAdapter(), model="m").execute(store=store)

    assert store.state["prime"]["each_item"]["collected"]["value"] == ["OUT", "OUT"]


def test_disabled_conditional_skips_condition_and_both_branches() -> None:
    orch = {
        "effects": [
            {
                "type": "conditional",
                "name": "decide",
                "if": {"mode": "model", "template": "should we?"},
                "then": [{"type": "prompt", "name": "yes_path", "template": "y"}],
                "else": [{"type": "prompt", "name": "no_path", "template": "n"}],
            }
        ]
    }
    state, adapter = _run(orch, {"decide": {"enabled": False}})

    _assert_disabled_node(state["prime"]["decide"])
    # The model-mode condition never reached the adapter.
    assert adapter.calls == []


def test_disabled_effect_inside_a_conditional_branch_is_skipped() -> None:
    orch = {
        "effects": [
            {
                "type": "conditional",
                "name": "decide",
                "if": {"mode": "cel", "expr": "true"},
                "then": [
                    {"type": "prompt", "name": "kept", "template": "keep"},
                    {"type": "prompt", "name": "dropped", "template": "drop"},
                ],
            }
        ]
    }
    state, adapter = _run(orch, {"decide.dropped": {"enabled": False}})

    _assert_disabled_node(state["prime"]["decide"]["dropped"])
    assert state["prime"]["decide"]["kept"]["value"] == "OUT"
    assert [prompt for _, prompt in adapter.calls] == ["keep"]


def test_disabled_reflector_turns_agentic_planning_off_for_the_run() -> None:
    """The flagship use: same orchestration, planning switched off."""
    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
                "effects": [
                    {
                        "type": "prompt",
                        "name": "propose_steps",
                        "template": "plan it",
                    }
                ],
            },
            {"type": "prompt", "name": "after", "template": "after"},
        ]
    }

    on_state, on_adapter = _run(orch, {})
    assert on_state["prime"]["planner"]["value"] is True
    assert len(on_adapter.calls) == 2  # planning prompt + `after`

    off_state, off_adapter = _run(orch, {"planner": {"enabled": False}})
    _assert_disabled_node(off_state["prime"]["planner"])
    assert "inner" not in off_state["prime"]["planner"]
    # Only the downstream effect ran — no planning call at all.
    assert [prompt for _, prompt in off_adapter.calls] == ["after"]


# ── downstream coherence: templates and conditions ───────────────────────────


def test_chevron_template_referencing_a_disabled_node_renders_empty() -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "a", "template": "a"},
            {"type": "prompt", "name": "b", "template": "b saw <{{a.value}}>"},
        ]
    }
    state, adapter = _run(orch, {"a": {"enabled": False}})

    assert state["prime"]["b"]["meta"]["prompt_sent"] == "b saw <>"
    assert adapter.calls[-1][1] == "b saw <>"
    # Pinned directly against the renderer, independent of the runtime.
    assert chevron.render("<{{a.value}}>", state["prime"]) == "<>"


@pytest.mark.parametrize(
    "expr",
    [
        "state.prime.a.value == 'yes'",
        "size(state.prime.a.value) > 0",
        "state.prime.a.value",
    ],
)
def test_downstream_cel_condition_on_a_disabled_node_is_false(expr: str) -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "a", "template": "a"},
            {
                "type": "conditional",
                "name": "gate",
                "if": {"mode": "cel", "expr": expr},
                "then": [{"type": "prompt", "name": "taken", "template": "then"}],
                "else": [{"type": "prompt", "name": "skipped", "template": "else"}],
            },
        ]
    }
    state, _ = _run(orch, {"a": {"enabled": False}})

    assert state["prime"]["gate"]["meta"]["condition_result"] is False
    assert state["prime"]["gate"]["meta"]["branch"] == "else"


# ── regression: nothing changes without a profile ────────────────────────────


def test_compiled_effects_default_to_enabled() -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "p", "template": "p"},
            {"type": "tool", "name": "t", "provider": "http", "params": {}},
            {"type": "use", "name": "u", "path": "x.yml"},
            {
                "type": "dynamic",
                "name": "d",
                "effects": [{"type": "prompt", "name": "inner", "template": "i"}],
            },
            {
                "type": "conditional",
                "name": "c",
                "if": {"mode": "cel", "expr": "true"},
                "then": [],
            },
            {"type": "loop", "name": "loop_it", "body": []},
            {"type": "reflector", "name": "r", "effects": []},
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    assert all(effect.enabled is True for effect in root.effects)
    assert root.effects[3].effects[0].enabled is True


def test_overrides_without_enabled_leave_effects_enabled() -> None:
    orch = {"effects": [{"type": "prompt", "name": "a", "template": "a"}]}
    root = compile_orchestration(orch=orch, root_name="prime")
    overridden, _ = apply_effect_overrides(root, {"a": {"model": "tier-1"}})

    assert overridden.effects[0].enabled is True
    assert overridden.effects[0].model == "tier-1"


def test_enabled_true_is_a_no_op() -> None:
    orch = {"effects": [{"type": "prompt", "name": "a", "template": "a-template"}]}
    state, adapter = _run(orch, {"a": {"enabled": True}})

    assert state["prime"]["a"]["value"] == "OUT"
    assert [prompt for _, prompt in adapter.calls] == ["a-template"]
