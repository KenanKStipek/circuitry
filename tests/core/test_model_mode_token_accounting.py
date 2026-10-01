"""Issue #263 part 2: model-mode `if`/`while` calls bypass PromptRuntime
entirely (they call ``adapter.generate`` directly) and used to record no
tokens at all anywhere in state.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class FixedReplyAdapter:
    name: str = "fixed"
    text: str = "yes"
    tokens_sent: int = 11
    tokens_received: int = 2

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(
            text=self.text,
            raw={},
            tokens_sent=self.tokens_sent,
            tokens_received=self.tokens_received,
        )


def test_model_mode_if_records_tokens_on_its_meta() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "name": "gate",
                "if": {"mode": "model", "template": "is it ready?"},
                "then": [{"type": "tool", "name": "noop", "provider": "json", "params": {"input": "1"}}],
                "else": [],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})

    DynamicRuntime(
        root, adapter=FixedReplyAdapter(text="yes", tokens_sent=11, tokens_received=2), model="m"
    ).execute(store=store)

    assert store.get("prime.gate.meta.tokens_sent") == 11
    assert store.get("prime.gate.meta.tokens_received") == 2


def test_model_mode_if_records_tokens_even_when_the_answer_is_unparseable() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "name": "gate",
                "if": {"mode": "model", "template": "is it ready?"},
                "then": [],
                "else": [],
                "on_error": "continue",
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})

    DynamicRuntime(
        root,
        adapter=FixedReplyAdapter(text="maybe", tokens_sent=4, tokens_received=1),
        model="m",
    ).execute(store=store)

    assert store.get("prime.gate.meta.tokens_sent") == 4
    assert store.get("prime.gate.meta.tokens_received") == 1
    assert store.get("prime.gate.meta.error") is not None


def test_model_mode_while_records_the_last_checks_tokens() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "while": {"mode": "model", "template": "keep going?"},
                "max_iterations": 1,
                "body": [{"type": "tool", "name": "noop", "provider": "json", "params": {"input": "1"}}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})

    DynamicRuntime(
        root, adapter=FixedReplyAdapter(text="no", tokens_sent=6, tokens_received=3), model="m"
    ).execute(store=store)

    assert store.get("prime.spin.meta.tokens_sent") == 6
    assert store.get("prime.spin.meta.tokens_received") == 3


def test_model_mode_while_accumulates_tokens_across_every_check() -> None:
    """A ``while`` that checks its condition more than once must not report
    only the final check's cost — ``tokens_sent``/``tokens_received`` stay
    the last check's own count, but ``tokens_sent_total``/
    ``tokens_received_total`` sum every check this loop made."""

    @dataclass
    class ScriptedWhileAdapter:
        name: str = "scripted"
        replies: list[tuple[str, int, int]] = field(default_factory=list)
        calls: int = 0

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            text, sent, received = self.replies[self.calls]
            self.calls += 1
            return GenerateResult(
                text=text, raw={}, tokens_sent=sent, tokens_received=received
            )

    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "while": {"mode": "model", "template": "keep going?"},
                "max_iterations": 5,
                "body": [
                    {"type": "tool", "name": "noop", "provider": "json", "params": {"input": "1"}}
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})

    adapter = ScriptedWhileAdapter(
        replies=[("yes", 3, 1), ("yes", 4, 2), ("no", 5, 1)]
    )
    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    assert adapter.calls == 3
    assert store.get("prime.spin.meta.tokens_sent") == 5
    assert store.get("prime.spin.meta.tokens_received") == 1
    assert store.get("prime.spin.meta.tokens_sent_total") == 3 + 4 + 5
    assert store.get("prime.spin.meta.tokens_received_total") == 1 + 2 + 1
