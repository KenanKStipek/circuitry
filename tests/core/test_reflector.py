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
    done, cleaned = _extract_done_flag(text)
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
