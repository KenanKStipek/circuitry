"""Tests for the reflector runtime (now delegating to use(inline))."""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest
import yaml

from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.reflector import (
    _extract_done_flag,
)
from circuitry.core.store import Store


def _mock_adapter(response: str = "mock") -> MagicMock:
    adapter = MagicMock()
    adapter.name = "mock"
    result = MagicMock()
    result.text = response
    result.raw = {}
    result.tokens_sent = 10
    result.tokens_received = 5
    adapter.generate.return_value = result
    return adapter


# ── _extract_done_flag ───────────────────────────────────────────────────────


def test_extract_done_false() -> None:
    text = yaml.dump({"done": False, "effects": [{"type": "prompt", "name": "a", "template": "hi"}]})
    done, cleaned = _extract_done_flag(text)
    assert done is False
    parsed = yaml.safe_load(cleaned)
    assert "effects" in parsed
    assert "done" not in parsed


def test_extract_done_true() -> None:
    text = yaml.dump({"done": True, "effects": [{"type": "prompt", "name": "a", "template": "hi"}]})
    done, _cleaned = _extract_done_flag(text)
    assert done is True


def test_extract_done_empty() -> None:
    done, cleaned = _extract_done_flag("")
    assert done is True
    assert cleaned == ""


def test_extract_done_bare_list() -> None:
    text = yaml.dump([{"type": "prompt", "name": "a", "template": "hi"}])
    done, cleaned = _extract_done_flag(text)
    assert done is False
    parsed = yaml.safe_load(cleaned)
    assert "effects" in parsed


def test_extract_done_code_fences() -> None:
    text = "```yaml\ndone: false\neffects:\n  - type: prompt\n    name: a\n    template: hi\n```"
    done, cleaned = _extract_done_flag(text)
    assert done is False
    assert "```" not in cleaned


def test_extract_done_legacy_steps() -> None:
    text = yaml.dump({"done": False, "steps": [{"type": "prompt", "name": "a", "template": "hi"}]})
    done, cleaned = _extract_done_flag(text)
    assert done is False
    parsed = yaml.safe_load(cleaned)
    assert "effects" in parsed


# ── ReflectorRuntime end-to-end ──────────────────────────────────────────────


def test_reflector_single_iteration() -> None:
    """Reflector runs inner prompt, gets plan, and executes generated effects via use(inline)."""
    plan_yaml = yaml.dump({
        "done": True,
        "effects": [{"type": "prompt", "name": "generated_step", "template": "Do the thing"}],
    })

    adapter = _mock_adapter(plan_yaml)

    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
                "max_effects": 5,
                "effects": [
                    {
                        "type": "prompt",
                        "name": "propose_steps",
                        "template": "Generate a plan.",
                    }
                ],
            }
        ]
    }

    root = compile_orchestration(orch=orch)
    store = Store(state={})

    DynamicRuntime(root, adapter=adapter, model="test-model").execute(store=store)

    planner = store.state["prime"]["planner"]
    assert planner["value"] is True
    assert len(planner["meta"]["iterations"]) == 1
    assert planner["meta"]["iterations"][0]["done"] is True


def test_reflector_dry_run() -> None:
    """Dry run stops after inner execution without executing generated effects."""
    adapter = _mock_adapter("dry run response")

    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
                "effects": [
                    {"type": "prompt", "name": "propose_steps", "template": "Plan."},
                ],
            }
        ]
    }

    root = compile_orchestration(orch=orch)
    store = Store(state={})

    DynamicRuntime(root, adapter=adapter, model="test-model", dry_run=True).execute(store=store)

    assert store.state["prime"]["planner"]["value"] is True


def test_reflector_stop_on_empty_effects() -> None:
    """If LLM returns no effects, reflector stops."""
    plan_yaml = yaml.dump({"done": False, "effects": []})
    adapter = _mock_adapter(plan_yaml)

    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
                "effects": [
                    {"type": "prompt", "name": "propose_steps", "template": "Plan."},
                ],
            }
        ]
    }

    root = compile_orchestration(orch=orch)
    store = Store(state={})

    DynamicRuntime(root, adapter=adapter, model="test-model").execute(store=store)

    planner = store.state["prime"]["planner"]
    assert planner["value"] is True
    iteration = planner["meta"]["iterations"][0]
    assert iteration["stop"] is True


def test_reflector_plan_over_max_effects_is_invalid_and_never_truncated() -> None:
    """A generated plan with more top-level effects than max_effects is an
    invalid plan: the reflector fails rather than silently running only the
    first max_effects of them (#251 part 2). ``done: False`` so this plan
    actually reaches the cap check (see the ``done: True`` case below, which
    stops before ever running and so is exempt from it)."""
    plan_yaml = yaml.dump({
        "done": False,
        "effects": [
            {"type": "prompt", "name": "a", "template": "a"},
            {"type": "prompt", "name": "b", "template": "b"},
            {"type": "prompt", "name": "c", "template": "c"},
        ],
    })
    adapter = _mock_adapter(plan_yaml)

    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
                "max_effects": 2,
                "effects": [
                    {"type": "prompt", "name": "propose_steps", "template": "Plan."},
                ],
            }
        ]
    }

    root = compile_orchestration(orch=orch)
    store = Store(state={})

    with pytest.raises(RuntimeError, match=r"3.*exceeds max_effects.*2"):
        DynamicRuntime(root, adapter=adapter, model="test-model").execute(store=store)

    planner = store.state["prime"]["planner"]
    assert planner["value"] is False
    assert "generated" not in planner or planner["generated"] == {}


def test_reflector_done_plan_over_max_effects_stops_cleanly_without_running() -> None:
    """A plan over the cap that also says ``done: True`` is never executed
    at all (``stop_on_done``'s normal early return), so the cap — which only
    matters for a plan that will actually run — does not fail the run."""
    plan_yaml = yaml.dump({
        "done": True,
        "effects": [
            {"type": "prompt", "name": "a", "template": "a"},
            {"type": "prompt", "name": "b", "template": "b"},
            {"type": "prompt", "name": "c", "template": "c"},
        ],
    })
    adapter = _mock_adapter(plan_yaml)

    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
                "max_effects": 2,
                "effects": [
                    {"type": "prompt", "name": "propose_steps", "template": "Plan."},
                ],
            }
        ]
    }

    root = compile_orchestration(orch=orch)
    store = Store(state={})

    DynamicRuntime(root, adapter=adapter, model="test-model").execute(store=store)

    planner = store.state["prime"]["planner"]
    assert planner["value"] is True
    assert planner["meta"]["iterations"][0]["stop"] is True
    assert "generated" not in planner or planner["generated"] == {}


def test_reflector_non_list_effects_does_not_give_a_misleading_count() -> None:
    """``effects`` as a string, not a list, must not be ``len()``-ed into a
    misleading 'exceeds max_effects' count — it is a type error, not a
    count-over-cap one, so it falls through to use(inline)'s own validation."""
    plan_yaml = yaml.dump({"done": False, "effects": "not-a-list"})
    adapter = _mock_adapter(plan_yaml)

    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
                "max_effects": 2,
                "effects": [
                    {"type": "prompt", "name": "propose_steps", "template": "Plan."},
                ],
            }
        ]
    }

    root = compile_orchestration(orch=orch)
    store = Store(state={})

    with pytest.raises(RuntimeError) as exc_info:
        DynamicRuntime(root, adapter=adapter, model="test-model").execute(store=store)

    assert "exceeds max_effects" not in str(exc_info.value)


def test_reflector_renders_root_goal_in_planning_prompt() -> None:
    """{goal} is the root `goal` effect's value, read from the run's root
    state — not the reflector's own (child) store node (#240)."""
    plan_yaml = yaml.dump({
        "done": True,
        "effects": [{"type": "prompt", "name": "generated_step", "template": "Do the thing"}],
    })

    captured_prompts: list[str] = []

    def _generate(*, model: str, prompt: str, timeout_seconds: int) -> MagicMock:
        captured_prompts.append(prompt)
        result = MagicMock()
        result.text = plan_yaml
        result.raw = {}
        result.tokens_sent = 10
        result.tokens_received = 5
        return result

    adapter = MagicMock()
    adapter.name = "mock"
    adapter.generate.side_effect = _generate

    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
                "effects": [
                    {"type": "prompt", "name": "propose_steps", "template": "Plan."},
                ],
            }
        ]
    }

    root = compile_orchestration(orch=orch)
    store = Store(state={"prime": {"goal": {"value": "Ship the thing by Friday."}}})

    DynamicRuntime(root, adapter=adapter, model="test-model").execute(store=store)

    assert captured_prompts, "expected the planning prompt to be rendered"
    assert "Ship the thing by Friday." in captured_prompts[0]


def test_reflector_nested_under_tree_dynamic_still_renders_root_goal() -> None:
    """A reflector that is a direct child of a ``flow: tree`` dynamic — the
    film reflectors' own topology — still reads the run's root ``goal``
    effect, not an empty isolated branch state (#240 review finding 1).

    ``Store.parallel_branches`` (used for every ``flow: tree`` dynamic and
    parallel loop) hands each branch a fresh, isolated state dict with no
    link back to the run root. Without ``Store.true_root_state`` surviving
    that isolation, ``{goal}`` renders empty here even though it works for
    a reflector in an ordinary sequential chain.
    """
    plan_yaml = yaml.dump({
        "done": True,
        "effects": [{"type": "prompt", "name": "generated_step", "template": "Do the thing"}],
    })

    captured_prompts: list[str] = []

    def _generate(*, model: str, prompt: str, timeout_seconds: int) -> MagicMock:
        captured_prompts.append(prompt)
        result = MagicMock()
        result.text = plan_yaml
        result.raw = {}
        result.tokens_sent = 10
        result.tokens_received = 5
        return result

    adapter = MagicMock()
    adapter.name = "mock"
    adapter.generate.side_effect = _generate

    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "making",
                "flow": "tree",
                "effects": [
                    {
                        "type": "reflector",
                        "name": "plan",
                        "effects": [
                            {"type": "prompt", "name": "propose_steps", "template": "Plan."},
                        ],
                    }
                ],
            }
        ]
    }

    root = compile_orchestration(orch=orch)
    store = Store(state={"prime": {"goal": {"value": "Ship the thing by Friday."}}})

    DynamicRuntime(root, adapter=adapter, model="test-model").execute(store=store)

    assert captured_prompts, "expected the planning prompt to be rendered"
    assert "Ship the thing by Friday." in captured_prompts[0]


def test_reflector_renders_redacted_effective_settings_as_context() -> None:
    """{context} is a concise, redacted summary of `runtime.effective_settings`,
    read from the run's root state (#240)."""
    plan_yaml = yaml.dump({
        "done": True,
        "effects": [{"type": "prompt", "name": "generated_step", "template": "Do the thing"}],
    })

    captured_prompts: list[str] = []

    def _generate(*, model: str, prompt: str, timeout_seconds: int) -> MagicMock:
        captured_prompts.append(prompt)
        result = MagicMock()
        result.text = plan_yaml
        result.raw = {}
        result.tokens_sent = 10
        result.tokens_received = 5
        return result

    adapter = MagicMock()
    adapter.name = "mock"
    adapter.generate.side_effect = _generate

    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
                "effects": [
                    {"type": "prompt", "name": "propose_steps", "template": "Plan."},
                ],
            }
        ]
    }

    root = compile_orchestration(orch=orch)
    # The secret-shaped value sits in `plugins`, a field the {context}
    # summary actually keeps (see _best_effort_context) — unlike a planted
    # `effective_settings.runtime.api_key`, which the summary already drops
    # before redaction ever runs, so a test using that placement would pass
    # even with the `redact()` call deleted.
    store = Store(
        state={
            "runtime": {
                "effective_settings": {
                    "model": "gpt-5",
                    "adapter": "openai",
                    "plugins": ["web_search", "sk-" + "x" * 30],
                }
            }
        }
    )

    DynamicRuntime(root, adapter=adapter, model="test-model").execute(store=store)

    assert captured_prompts, "expected the planning prompt to be rendered"
    prompt_text = captured_prompts[0]
    assert "gpt-5" in prompt_text
    assert "openai" in prompt_text
    assert "web_search" in prompt_text
    assert "sk-" + "x" * 30 not in prompt_text
    assert "***REDACTED***" in prompt_text


def test_reflector_nested_under_sequential_loop_still_renders_root_goal() -> None:
    """A reflector inside an ordinary (chain-flow) loop body still reads the
    run's root `goal` effect, same as one nested under a dynamic (#240)."""
    plan_yaml = yaml.dump({
        "done": True,
        "effects": [{"type": "prompt", "name": "generated_step", "template": "Do the thing"}],
    })

    captured_prompts: list[str] = []

    def _generate(*, model: str, prompt: str, timeout_seconds: int) -> MagicMock:
        captured_prompts.append(prompt)
        result = MagicMock()
        result.text = plan_yaml
        result.raw = {}
        result.tokens_sent = 10
        result.tokens_received = 5
        return result

    adapter = MagicMock()
    adapter.name = "mock"
    adapter.generate.side_effect = _generate

    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "rounds",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {
                        "type": "reflector",
                        "name": "plan",
                        "effects": [
                            {"type": "prompt", "name": "propose_steps", "template": "Plan."},
                        ],
                    }
                ],
            }
        ]
    }

    root = compile_orchestration(orch=orch)
    store = Store(
        state={
            "input": {"items": ["one"]},
            "prime": {"goal": {"value": "Ship the thing by Friday."}},
        }
    )

    DynamicRuntime(root, adapter=adapter, model="test-model").execute(store=store)

    assert captured_prompts, "expected the planning prompt to be rendered"
    assert "Ship the thing by Friday." in captured_prompts[0]


def test_reflector_context_truncated_at_max_chars() -> None:
    """{context} is capped at 2000 characters, not dumped in full (#240)."""
    plan_yaml = yaml.dump({
        "done": True,
        "effects": [{"type": "prompt", "name": "generated_step", "template": "Do the thing"}],
    })

    captured_prompts: list[str] = []

    def _generate(*, model: str, prompt: str, timeout_seconds: int) -> MagicMock:
        captured_prompts.append(prompt)
        result = MagicMock()
        result.text = plan_yaml
        result.raw = {}
        result.tokens_sent = 10
        result.tokens_received = 5
        return result

    adapter = MagicMock()
    adapter.name = "mock"
    adapter.generate.side_effect = _generate

    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
                "effects": [
                    {"type": "prompt", "name": "propose_steps", "template": "Plan."},
                ],
            }
        ]
    }

    root = compile_orchestration(orch=orch)
    huge_plugins = [f"plugin_{i}" for i in range(500)]
    store = Store(
        state={
            "runtime": {
                "effective_settings": {
                    "model": "gpt-5",
                    "adapter": "openai",
                    "plugins": huge_plugins,
                }
            }
        }
    )

    DynamicRuntime(root, adapter=adapter, model="test-model").execute(store=store)

    assert captured_prompts, "expected the planning prompt to be rendered"
    prompt_text = captured_prompts[0]
    # The prime still contains the critical YAML warning and the plan
    # instructions beyond {context}, so assert on the {context} block's own
    # size rather than the whole prompt.
    context_start = prompt_text.index('"model"')
    context_block = prompt_text[context_start:]
    truncated_marker = "... (truncated)"
    assert truncated_marker in context_block
    assert len(context_block.split(truncated_marker)[0]) <= 2000


def test_reflector_invalid_plan_records_error() -> None:
    """Invalid YAML from LLM is caught and recorded as error."""
    adapter = _mock_adapter("This is not YAML at all: [[[invalid")

    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
                "effects": [
                    {"type": "prompt", "name": "propose_steps", "template": "Plan."},
                ],
            }
        ]
    }

    root = compile_orchestration(orch=orch)
    store = Store(state={})

    with pytest.raises(RuntimeError):
        DynamicRuntime(root, adapter=adapter, model="test-model").execute(store=store)

    planner = store.state["prime"]["planner"]
    assert planner["value"] is False
