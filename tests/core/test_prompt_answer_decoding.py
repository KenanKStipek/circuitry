"""prompt_type: boolean / number decoding of realistic model replies (issue #247).

A model rarely answers a bare `yes`/`42` — it wraps the answer in a trailing
period, markdown emphasis, or a short explanation. These tests pin that the
runtime reads those replies leniently, that an answer which still doesn't
parse raises (so retries and on_error apply, never a silent null), and that
the raw reply is kept on `meta.answer`.
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
    """Always answers with the same text."""

    text: str
    name: str = "fixed"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=self.text, raw={})


@dataclass
class QueuedReplyAdapter:
    """Returns queued replies in order, raising if exhausted."""

    replies: list[str]
    name: str = "queued"
    calls: int = field(default=0, init=False)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls += 1
        text = self.replies.pop(0)
        return GenerateResult(text=text, raw={})


def _prompt_orch(*, prompt_type: str, on_error: str = "fail", retries: dict | None = None) -> dict:
    effect: dict = {
        "type": "prompt",
        "name": "task",
        "prompt_type": prompt_type,
        "template": "Answer.",
        "on_error": on_error,
    }
    if retries is not None:
        effect["retries"] = retries
    return {"effects": [effect]}


@pytest.mark.parametrize(
    "reply,expected",
    [
        ("Yes.", True),
        ("yes, because it's the right call", True),
        ("**TRUE**", True),
        ("Y", True),
        ("No.", False),
        ("false!", False),
    ],
)
def test_boolean_prompt_decodes_realistic_replies(reply: str, expected: bool) -> None:
    root = compile_orchestration(orch=_prompt_orch(prompt_type="boolean"), root_name="prime")
    store = Store({})

    DynamicRuntime(root, adapter=FixedReplyAdapter(text=reply), model="m").execute(store=store)

    assert store.get("prime.task.value") is expected
    assert store.get("prime.task.meta.answer") == reply
    assert store.get("prime.task.meta.error") is None


@pytest.mark.parametrize(
    "reply,expected",
    [
        ("42", 42),
        ("42.", 42),
        ("3.5", 3.5),
        ("-1", -1),
        ("1e3", 1000.0),
    ],
)
def test_number_prompt_decodes_realistic_replies(reply: str, expected: float) -> None:
    root = compile_orchestration(orch=_prompt_orch(prompt_type="number"), root_name="prime")
    store = Store({})

    DynamicRuntime(root, adapter=FixedReplyAdapter(text=reply), model="m").execute(store=store)

    assert store.get("prime.task.value") == expected
    assert store.get("prime.task.meta.answer") == reply


def test_boolean_prompt_raises_on_unreadable_reply_instead_of_null() -> None:
    """Regression: 'Yes.' used to decode to None with no error recorded."""
    root = compile_orchestration(orch=_prompt_orch(prompt_type="boolean"), root_name="prime")
    store = Store({})

    with pytest.raises(RuntimeError) as excinfo:
        DynamicRuntime(root, adapter=FixedReplyAdapter(text="maybe"), model="m").execute(
            store=store
        )
    assert isinstance(excinfo.value.__cause__, AnswerParseError)

    error = store.get("prime.task.meta.error")
    assert error is not None
    assert "maybe" in error
    assert store.get("prime.task.value") is None


def test_number_prompt_raises_rather_than_guessing_from_trailing_words() -> None:
    root = compile_orchestration(orch=_prompt_orch(prompt_type="number"), root_name="prime")
    store = Store({})

    with pytest.raises(RuntimeError) as excinfo:
        DynamicRuntime(
            root, adapter=FixedReplyAdapter(text="42 degrees"), model="m"
        ).execute(store=store)
    assert isinstance(excinfo.value.__cause__, AnswerParseError)

    assert "42 degrees" in store.get("prime.task.meta.error")


def test_boolean_prompt_on_error_skip_leaves_value_none_with_error_recorded() -> None:
    orch = _prompt_orch(prompt_type="boolean", on_error="skip")
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})

    DynamicRuntime(root, adapter=FixedReplyAdapter(text="maybe"), model="m").execute(store=store)

    assert store.get("prime.task.value") is None
    assert "maybe" in store.get("prime.task.meta.error")


def test_boolean_prompt_retries_after_an_unparseable_reply() -> None:
    """An unparseable answer is a dispatch failure retries can recover from."""
    orch = _prompt_orch(
        prompt_type="boolean", retries={"max_attempts": 2, "backoff_ms": 0}
    )
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    adapter = QueuedReplyAdapter(replies=["uh, unclear", "Yes."])

    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    assert adapter.calls == 2
    assert store.get("prime.task.value") is True
    assert store.get("prime.task.meta.answer") == "Yes."
    assert store.get("prime.task.meta.retries_used") == 1
