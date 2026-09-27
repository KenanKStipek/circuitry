"""Model-mode `if`/`while` conditions read realistic yes/no replies (issue #247).

Before this fix, `answer in ("yes", "true", "1", "y")` meant `"Yes."`,
`"yes, because ..."` or `"**Yes**"` all counted as *no* — the branch/loop
took the wrong path with nothing in meta to explain why. These tests pin
the fixed behaviour: realistic replies parse correctly, an answer that
truly can't be read raises instead of silently becoming false, and the
raw reply/adapter/model land on `meta`.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.answers import AnswerParseError
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class FixedReplyAdapter:
    text: str
    name: str = "fixed"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=self.text, raw={})


@dataclass
class QueuedReplyAdapter:
    replies: list[str]
    name: str = "queued"
    prompts: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.prompts.append(prompt)
        text = self.replies.pop(0)
        return GenerateResult(text=text, raw={})


def _if_orch(*, on_error: str = "fail") -> dict:
    return {
        "effects": [
            {
                "type": "if",
                "name": "gate",
                "if": {"mode": "model", "template": "Is 2 > 1? Answer yes or no."},
                "on_error": on_error,
                "then": [{"type": "prompt", "name": "yes_branch", "template": "y"}],
                "else": [{"type": "prompt", "name": "no_branch", "template": "n"}],
            }
        ]
    }


def test_if_model_condition_takes_then_branch_on_a_period_terminated_yes() -> None:
    """Repro from the issue: an adapter answering 'Yes.' used to take else."""
    root = compile_orchestration(orch=_if_orch(), root_name="prime")
    store = Store({})

    DynamicRuntime(root, adapter=FixedReplyAdapter(text="Yes."), model="m").execute(
        store=store
    )

    assert store.get("prime.gate.value.branch") == "then"
    assert store.get("prime.gate.value.effects")[0]["name"] == "yes_branch"
    assert store.get("prime.gate.meta.condition_result") is True
    assert store.get("prime.gate.meta.answer") == "Yes."
    assert store.get("prime.gate.meta.adapter") == "fixed"
    assert store.get("prime.gate.meta.model") == "m"


@pytest.mark.parametrize("reply", ["**Yes**", "yes, because it is"])
def test_if_model_condition_reads_markdown_and_trailing_explanation(reply: str) -> None:
    root = compile_orchestration(orch=_if_orch(), root_name="prime")
    store = Store({})

    DynamicRuntime(root, adapter=FixedReplyAdapter(text=reply), model="m").execute(
        store=store
    )

    assert store.get("prime.gate.value.branch") == "then"


def test_if_model_condition_raises_on_an_answer_it_cannot_read() -> None:
    root = compile_orchestration(orch=_if_orch(), root_name="prime")
    store = Store({})

    with pytest.raises(RuntimeError) as excinfo:
        DynamicRuntime(root, adapter=FixedReplyAdapter(text="maybe"), model="m").execute(
            store=store
        )
    assert isinstance(excinfo.value.__cause__, AnswerParseError)

    assert "maybe" in store.get("prime.gate.meta.error")
    assert store.get("prime.gate.meta.answer") == "maybe"
    assert "condition_result" not in store.get("prime.gate.meta")


def test_if_model_condition_on_error_continue_falls_to_else_and_keeps_the_answer() -> None:
    root = compile_orchestration(orch=_if_orch(on_error="continue"), root_name="prime")
    store = Store({})

    DynamicRuntime(root, adapter=FixedReplyAdapter(text="maybe"), model="m").execute(
        store=store
    )

    assert store.get("prime.gate.value.branch") == "else"
    assert store.get("prime.gate.meta.answer") == "maybe"


def _while_orch() -> dict:
    return {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "max_iterations": 5,
                "while": {"mode": "model", "template": "Continue?"},
                "body": [{"type": "prompt", "name": "step", "template": "STEP"}],
            }
        ]
    }


def test_while_model_condition_reads_a_period_terminated_reply_as_continue() -> None:
    root = compile_orchestration(orch=_while_orch(), root_name="prime")
    store = Store({})
    adapter = QueuedReplyAdapter(replies=["Yes.", "STEP", "No.", "STEP"])

    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    value = store.get("prime.spin.value")
    assert value["iterations"] == 1
    assert value["termination"]["reason"] == "condition_false"
    assert store.get("prime.spin.meta.answer") == "No."
    assert store.get("prime.spin.meta.adapter") == "queued"
    assert store.get("prime.spin.meta.model") == "m"


def test_while_model_condition_error_stops_the_loop_and_records_the_answer() -> None:
    orch = _while_orch()
    orch["effects"][0]["on_error"] = "break"
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    adapter = QueuedReplyAdapter(replies=["unclear"])

    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    value = store.get("prime.spin.value")
    assert value["termination"]["reason"] == "condition_error"
    assert store.get("prime.spin.meta.answer") == "unclear"
    assert "unclear" in store.get("prime.spin.meta.error")


@dataclass
class RaisesOnSecondCallAdapter:
    """Answers 'Yes.' on the first while-condition check, lets the body's
    own prompt succeed, then raises (e.g. a timeout) on the second
    condition check, before ever generating a reply for it.
    """

    name: str = "flaky"
    calls: int = field(default=0, init=False)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls += 1
        if self.calls == 1:
            return GenerateResult(text="Yes.", raw={})
        if self.calls == 2:
            return GenerateResult(text="STEP", raw={})
        raise TimeoutError("simulated timeout")


def test_while_model_condition_does_not_carry_over_a_previous_answer_on_error() -> None:
    """Check 2's own failure must not be reported next to check 1's answer."""
    orch = _while_orch()
    orch["effects"][0]["on_error"] = "break"
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    adapter = RaisesOnSecondCallAdapter()

    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    value = store.get("prime.spin.value")
    assert value["termination"]["reason"] == "condition_error"
    assert store.get("prime.spin.meta.answer") is None
    assert "simulated timeout" in store.get("prime.spin.meta.error")


# --------------------------------------------------------------------------
# Guidebook regression: chapter 1's `is_vegetarian` classifier pattern.
# --------------------------------------------------------------------------


@dataclass
class ClassifierAdapter:
    """Scripted model that always answers a yes/no question with a period,
    the way a real 3B model does (see issue #247) rather than a bare token.
    """

    name: str = "classifier"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text="Yes.", raw={})


def test_guidebook_is_vegetarian_boolean_prompt_decodes_a_natural_reply() -> None:
    """docs/guidebook/01-prompt.md's `is_vegetarian` example, driven by a
    scripted adapter that answers the way a real model does.
    """
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "suggest_dish",
                "template": "Suggest one main course for a dinner party, in one sentence.",
            },
            {
                "type": "prompt",
                "name": "is_vegetarian",
                "prompt_type": "boolean",
                "messages": [
                    {
                        "role": "system",
                        "content": "You are a strict classifier. Reply with only true or false.",
                    },
                    {
                        "role": "user",
                        "content": "Is this dish vegetarian? {{prime.suggest_dish.value}}",
                    },
                ],
            },
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})

    DynamicRuntime(root, adapter=ClassifierAdapter(), model="m").execute(store=store)

    assert store.get("prime.is_vegetarian.value") is True
    assert store.get("prime.is_vegetarian.meta.answer") == "Yes."
    assert store.get("prime.is_vegetarian.meta.error") is None
