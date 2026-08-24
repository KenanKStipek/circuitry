"""STE (ASD-STE100) constraints on REFLECTOR_PRIME_V1 — issue #167.

Two things are pinned here:
  1. The prime teaches the STE rule set — a drift guard on the header and the
     key rules, verbatim (style of test_wizard_agent.py).
  2. The worked example the prime teaches by imitation is itself STE-
     compliant, checked both directly and through an actual mock-adapter
     reflector run (mechanical proxies: word-count ceiling, imperative
     openings — not a full STE linter, which is a non-goal of the issue).
"""

from __future__ import annotations

import re
from typing import Any
from unittest.mock import MagicMock

import yaml

from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.primes import REFLECTOR_PRIME_V1
from circuitry.core.store import Store

# Imperative verbs the worked example actually opens sentences with. Not a
# general STE verb list — just what this exemplar teaches the model to imitate.
IMPERATIVE_OPENERS = {"ask", "propose", "list", "give", "summarize", "write"}

PROCEDURAL_WORD_CEILING = 20


def _mock_adapter(response: str) -> MagicMock:
    adapter = MagicMock()
    adapter.name = "mock"
    result = MagicMock()
    result.text = response
    result.raw = {}
    result.tokens_sent = 10
    result.tokens_received = 5
    adapter.generate.return_value = result
    return adapter


def _sentences(template: str) -> list[str]:
    return [s.strip() for s in template.split(".") if s.strip()]


def _walk_templates(effects: list[dict[str, Any]]) -> list[str]:
    out: list[str] = []
    for effect in effects:
        template = effect.get("template")
        if isinstance(template, str):
            out.append(template)
        inner = effect.get("effects")
        if isinstance(inner, list):
            out.extend(_walk_templates(inner))
    return out


def _assert_ste_sentences(templates: list[str]) -> None:
    assert templates, "no templates to check"
    for template in templates:
        for sentence in _sentences(template):
            words = sentence.split()
            assert len(words) <= PROCEDURAL_WORD_CEILING, (
                f"sentence exceeds the {PROCEDURAL_WORD_CEILING}-word procedural "
                f"ceiling the prime teaches: {sentence!r}"
            )
            opener = words[0].lower()
            assert opener in IMPERATIVE_OPENERS, (
                f"sentence does not open with a taught imperative verb: {sentence!r}"
            )


def _extract_example_plan() -> dict[str, Any]:
    """Pull the worked EXAMPLE block out of REFLECTOR_PRIME_V1 verbatim."""
    match = re.search(
        r"EXAMPLE \(this is the exact style.*?\):\n(.*?)\nEND\. Output YAML only\.",
        REFLECTOR_PRIME_V1,
        re.DOTALL,
    )
    assert match, "REFLECTOR_PRIME_V1 worked example not found"
    parsed = yaml.safe_load(match.group(1))
    assert isinstance(parsed, dict)
    return parsed


# ── Drift guard: the STE section itself ─────────────────────────────────────


def test_prime_teaches_simplified_technical_english() -> None:
    """Pin the STE section header and its key rules verbatim."""
    assert "=== LANGUAGE: SIMPLIFIED TECHNICAL ENGLISH ===" in REFLECTOR_PRIME_V1
    for rule in (
        "One instruction per sentence",
        "Active voice",
        "Imperative mood",
        "20 words or fewer",
        "25 words or fewer",
        "One meaning per word",
        "No noun cluster longer than 3 words",
        "No vague verbs",
        '"handle"',
        '"manage"',
        '"deal with"',
        "Do not drop articles",
    ):
        assert rule in REFLECTOR_PRIME_V1, f"STE rule missing from prime: {rule!r}"


# ── The worked example imitates compliant output ────────────────────────────


def test_worked_example_templates_are_ste() -> None:
    """The exemplar the prime teaches must itself be STE-compliant, or the
    model has nothing correct to imitate."""
    plan = _extract_example_plan()
    templates = _walk_templates(plan["effects"])
    _assert_ste_sentences(templates)


def test_reflector_mock_run_yields_ste_templates() -> None:
    """End-to-end proxy: feed the prime's own worked example back through a
    mock-adapter reflector run and check the generated plan's templates land
    in imperative STE register."""
    plan = dict(_extract_example_plan())
    plan["done"] = True
    plan_yaml = yaml.dump(plan, default_flow_style=False)
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
    parsed_plan = planner["meta"]["iterations"][0]["parsed"]
    templates = _walk_templates(parsed_plan["effects"])
    _assert_ste_sentences(templates)
