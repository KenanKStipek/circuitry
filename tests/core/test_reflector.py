"""Tests for the reflector runtime (now delegating to use(inline))."""

from __future__ import annotations

import io
import re
from pathlib import Path
from unittest.mock import MagicMock

import pytest
import yaml

from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.primes import REFLECTOR_PRIME_V1
from circuitry.core.reflector import (
    _extract_done_flag,
    _render_reflector_prime,
    prime_template_str_format_error,
)
from circuitry.core.store import Store
from circuitry.core.templates import TemplateError


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
    done, parsed = _extract_done_flag(text)
    assert done is False
    assert "effects" in parsed
    assert "done" not in parsed


def test_extract_done_true() -> None:
    text = yaml.dump({"done": True, "effects": [{"type": "prompt", "name": "a", "template": "hi"}]})
    done, _cleaned = _extract_done_flag(text)
    assert done is True


def test_extract_done_empty() -> None:
    done, parsed = _extract_done_flag("")
    assert done is True
    assert parsed is None


def test_extract_done_bare_list() -> None:
    text = yaml.dump([{"type": "prompt", "name": "a", "template": "hi"}])
    done, parsed = _extract_done_flag(text)
    assert done is False
    assert "effects" in parsed


def test_extract_done_code_fences() -> None:
    text = "```yaml\ndone: false\neffects:\n  - type: prompt\n    name: a\n    template: hi\n```"
    done, parsed = _extract_done_flag(text)
    assert done is False
    assert parsed == {"effects": [{"type": "prompt", "name": "a", "template": "hi"}]}


def test_extract_done_legacy_steps() -> None:
    text = yaml.dump({"done": False, "steps": [{"type": "prompt", "name": "a", "template": "hi"}]})
    done, parsed = _extract_done_flag(text)
    assert done is False
    assert "effects" in parsed


# ── _render_reflector_prime (Mustache, no fallback) ──────────────────────────


#: `_render_reflector_prime`'s exact output for the default prime and the
#: inputs below, pinned once (#778) against what the old `str.format`
#: rendering of the pre-#778 template produced for the same inputs --
#: confirmed byte for byte equal to that output, not just containing its
#: substrings.
_DEFAULT_PRIME_RENDERED = (
    "CRITICAL: The next system will parse your response as YAML. If you include ANY markdown "
    "(like **bold**, *italics*, backticks, or headings), parsing fails.\n"
    "Do not output any '*' characters except as plain text INSIDE a YAML quoted string.\n"
    "Never use *, **, backticks, headings, or bullet prose outside YAML.\n"
    "Every line must be valid YAML.\n"
    "CRITICAL: Your output will be parsed by a YAML parser and then validated as a Circuitry orchestration.\n"
    "\n"
    "You MUST output ONE valid YAML document, and NOTHING ELSE.\n"
    "Do NOT wrap in ``` fences.\n"
    "Do NOT output '---' or multiple YAML documents.\n"
    "Do NOT include any markdown formatting (no **, *, backticks, headings, bullet prose).\n"
    "\n"
    "OUTPUT SHAPE (required):\n"
    "done: <true|false>\n"
    "effects:\n"
    "  - type: prompt\n"
    "    name: <snake_case>\n"
    "    template: <string>\n"
    "  - type: dynamic\n"
    "    name: <snake_case>\n"
    "    flow: chain\n"
    "    effects: [ ... ]\n"
    "  - type: use\n"
    "    name: <snake_case>\n"
    "    ref: <library_name>\n"
    "    inputs: { ... }\n"
    "\n"
    "HARD RULES:\n"
    "- The top-level YAML MUST be a dict with these keys: done, effects.\n"
    "- effects MUST be a YAML list.\n"
    "- Each effect MUST be a YAML dict containing at minimum:\n"
    "  - type (one of: prompt, dynamic, loop, if, use, tool)\n"
    "  - name (snake_case, letters/numbers/underscore only)\n"
    "  - AND the required fields for that type:\n"
    "    - prompt: template (non-empty string)\n"
    "    - dynamic: effects (a list), optional flow (chain|tree)\n"
    "    - loop: body (a list of effects), plus each or while\n"
    "    - if: if (condition), then (effects list)\n"
    "    - use: ref (library name) or path (filesystem) or inline (YAML template)\n"
    "    - tool: provider (plugin name)\n"
    "- NEVER emit alternative schemas such as:\n"
    "  - plan:\n"
    "  - plan: { effects: ... }\n"
    "  - step_1:\n"
    "  - id/action/description-only effects\n"
    "- NEVER include '*' characters outside of a YAML quoted string (single or double quotes).\n"
    "- Keep descriptions inside YAML strings only.\n"
    "\n"
    "=== LANGUAGE: SIMPLIFIED TECHNICAL ENGLISH ===\n"
    "Write the plan's step descriptions and every effect's `template` in\n"
    "Simplified Technical English (ASD-STE100 style). Small models and humans\n"
    "read this text next — short, imperative, unambiguous sentences carry\n"
    "further than free-register prose.\n"
    '- One instruction per sentence. Do not join two instructions with "and" or\n'
    '  "then" — write two sentences instead.\n'
    '- Active voice. Imperative mood for instructions: "Write the summary.",\n'
    '  not "The summary should be written."\n'
    "- Procedural sentences (instructions, steps): 20 words or fewer.\n"
    "- Descriptive sentences (facts, context): 25 words or fewer.\n"
    "- One meaning per word. Use the same word for the same thing every time —\n"
    '  never swap "step" for "phase" for "stage".\n'
    '- No noun cluster longer than 3 words. Write "the user login form", not\n'
    '  "the user account login authentication form".\n'
    '- No vague verbs. "handle", "manage", "process", and "deal with" name\n'
    '  nothing — name the actual action: "validate", "write", "delete", "send".\n'
    '- Do not drop articles: write "the file", "a question" — never "file" or\n'
    '  "question" alone as a noun phrase.\n'
    "These rules govern every `template` you generate, not just this prompt's\n"
    "own prose.\n"
    "\n"
    "CONTEXT:\n"
    "Goal:\n"
    "Summarize the project.\n"
    "\n"
    "Additional Context (may be empty):\n"
    "{\n"
    '  "adapter": "scripted",\n'
    '  "model": "test",\n'
    '  "plugins": []\n'
    "}\n"
    "\n"
    "TASK:\n"
    "Generate Circuitry effects to advance the Goal.\n"
    "- Keep the total number of top-level effects <= 8.\n"
    "- Prefer a small number of high-leverage effects.\n"
    "- Use prompts to ask for missing info or to produce artifacts.\n"
    "- Use dynamics to group related prompts.\n"
    "- Use `use` to invoke existing orchestrations by name.\n"
    "\n"
    "EXAMPLE (this is the exact style you must follow, in Simplified Technical\n"
    "English; do not copy the content literally):\n"
    "done: false\n"
    "effects:\n"
    "  - type: prompt\n"
    "    name: clarify_requirements\n"
    '    template: "Ask 3 short questions about the goal. Ask about the missing constraints."\n'
    "\n"
    "  - type: dynamic\n"
    "    name: draft_plan\n"
    "    flow: chain\n"
    "    effects:\n"
    "      - type: prompt\n"
    "        name: propose_architecture\n"
    '        template: "Propose a minimal architecture. List the key components."\n'
    "\n"
    "      - type: prompt\n"
    "        name: define_milestones\n"
    '        template: "List 3 milestones. Give one acceptance criterion for each milestone."\n'
    "\n"
    "  - type: prompt\n"
    "    name: summarize_next_actions\n"
    '    template: "Summarize the next actions. Write 5 bullets in one YAML string."\n'
    "\n"
    "END. Output YAML only.\n"
    "\n"
)


def test_render_reflector_prime_default_renders_the_documented_text() -> None:
    """The default prime renders byte for byte the text it rendered under
    the old `str.format` rendering (#778), not just a text containing the
    expected substrings."""
    import json

    rendered = _render_reflector_prime(
        prime_template=REFLECTOR_PRIME_V1,
        max_effects=8,
        goal="Summarize the project.",
        context=json.dumps(
            {"adapter": "scripted", "model": "test", "plugins": []}, indent=2, sort_keys=True
        ),
    )
    assert rendered == _DEFAULT_PRIME_RENDERED


def test_render_reflector_prime_renders_mustache_without_html_escaping() -> None:
    rendered = _render_reflector_prime(
        prime_template="Goal: {{goal}}", max_effects=1, goal="a & b < c", context=""
    )
    assert "Goal: a & b < c" in rendered


def test_render_reflector_prime_has_no_fallback_for_a_malformed_template() -> None:
    with pytest.raises(TemplateError):
        _render_reflector_prime(
            prime_template="{{#unclosed", max_effects=1, goal="g", context="c"
        )


# ── prime_template_str_format_error ──────────────────────────────────────────


@pytest.mark.parametrize("name", ["goal", "context", "max_effects"])
def test_prime_template_str_format_error_catches_each_name(name: str) -> None:
    error = prime_template_str_format_error(f"Plan {{{name}}}")
    assert error is not None
    assert f"'{{{name}}}'" in error
    assert f"'{{{{{name}}}}}'" in error


def test_prime_template_str_format_error_ignores_the_mustache_form() -> None:
    assert prime_template_str_format_error("Plan {{goal}} {{context}} {{max_effects}}") is None


def test_prime_template_str_format_error_ignores_an_unrelated_single_brace() -> None:
    assert prime_template_str_format_error("literal: { not a field }") is None


# ── compile-time checks on prime_template ────────────────────────────────────


def _reflector_orch(prime_template: str) -> dict:
    return {
        "effects": [
            {
                "type": "reflector",
                "name": "plan_it",
                "prime_template": prime_template,
                "effects": [
                    {"type": "prompt", "name": "propose_steps", "template": "Plan."},
                ],
            }
        ]
    }


def test_compile_refuses_a_str_format_placeholder_in_prime_template() -> None:
    with pytest.raises(ValueError, match=r"write '\{\{goal\}\}' instead"):
        compile_orchestration(orch=_reflector_orch("Plan it. Goal: {goal}"))


def test_compile_refuses_a_malformed_mustache_prime_template() -> None:
    with pytest.raises(ValueError, match="malformed Mustache template"):
        compile_orchestration(orch=_reflector_orch("Plan it. {{#unclosed"))


def test_compile_accepts_a_mustache_prime_template() -> None:
    root = compile_orchestration(orch=_reflector_orch("Plan it. Goal: {{goal}}"))
    assert root.effects[0].prime_template == "Plan it. Goal: {{goal}}"


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
    """A reflector that is a direct child of a ``flow: tree`` dynamic still
    reads the run's root ``goal`` effect, not an empty isolated branch state
    (issue #240).

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
    """Unparseable YAML from the LLM is caught and recorded as ``parse_failed``
    -- there is no raw text left to hand use(inline) to diagnose instead
    (#778: the plan is never dumped back to YAML and reparsed, so an
    unparseable plan never reaches that path at all).
    """
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
    assert planner["meta"]["error"].startswith("parse_failed: ")
    # No `generated` child ever ran: nothing was left to hand use(inline).
    assert "iter_0" not in planner.get("generated", {})


def test_reflector_generated_plan_with_a_partial_fails_and_reads_no_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A generated plan is model output, glued into a `use: inline` child —
    `inline` is itself a composable field (#396), so a plan containing the
    literal text `{{> evil}}` is rejected as an unknown name rather than
    read as a file reference; the file it would have named is never
    touched (#354)."""
    monkeypatch.chdir(tmp_path)
    (tmp_path / "evil.mustache").write_text("LEAKED", encoding="utf-8")
    real_open = io.open

    def _guard_open(path: object, *args: object, **kwargs: object) -> object:
        if "evil.mustache" in str(path):
            raise AssertionError("rendering must not read evil.mustache")
        return real_open(path, *args, **kwargs)  # type: ignore[arg-type]

    monkeypatch.setattr(io, "open", _guard_open)

    plan_yaml = yaml.dump({
        "done": False,
        "effects": [{"type": "prompt", "name": "generated_step", "template": "{{> evil}}"}],
    })
    adapter = _mock_adapter(plan_yaml)

    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
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

    with pytest.raises(RuntimeError, match=re.escape("'{{> evil}}' does not name")):
        DynamicRuntime(root, adapter=adapter, model="test-model").execute(store=store)

    planner = store.state["prime"]["planner"]
    assert planner["value"] is False
    assert "LEAKED" not in str(store.state)


# ── No YAML round trip: plan key order, canonical-JSON hash (#778) ──────────


def test_reflector_plan_runs_with_keys_in_the_order_the_model_wrote_them() -> None:
    """The generated plan's own effect dict keeps the model's own key
    order -- never dumped to YAML and reparsed, which would alphabetize it."""
    plan_text = (
        "effects:\n"
        "  - type: tool\n"
        "    name: check\n"
        "    provider: json\n"
        "    params: {mode: parse, input: '{\"ok\": true}'}\n"
        "done: true\n"
    )
    adapter = _mock_adapter(plan_text)

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
    effect = planner["meta"]["iterations"][0]["parsed"]["effects"][0]
    assert list(effect.keys()) == ["type", "name", "provider", "params"]
    assert list(effect["params"].keys()) == ["mode", "input"]


def test_reflector_generated_child_orchestration_sha256_is_canonical_json() -> None:
    """A reflector-generated document's `meta.orchestration_sha256` is the
    sha256 of its canonical JSON text (sorted keys, compact separators,
    `ensure_ascii=False`), never of any YAML text (#778)."""
    import hashlib
    import json

    plan_text = yaml.dump({
        "done": False,
        "effects": [{"type": "prompt", "name": "generated_step", "template": "Do the thing"}],
    })
    adapter = _mock_adapter(plan_text)

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
    generated_doc = {
        "effects": [{"type": "prompt", "name": "generated_step", "template": "Do the thing"}]
    }
    expected = hashlib.sha256(
        json.dumps(
            generated_doc, sort_keys=True, separators=(",", ":"), ensure_ascii=False, default=str
        ).encode("utf-8")
    ).hexdigest()
    assert planner["generated"]["iter_0"]["meta"]["orchestration_sha256"] == expected
