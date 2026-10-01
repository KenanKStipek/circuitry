"""Issue #263 part 2: tokens from every attempt (failed, retried,
fallen-back-from) are recorded, not just the winning one.
"""

from __future__ import annotations

from dataclasses import dataclass

from circuitry.adapters._retry import AdapterCallError, RetryInfo
from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass(frozen=True)
class TokenSpendingFailAdapter:
    """Always fails, but as a provider that *did* spend tokens before
    erroring (e.g. a schema-invalid reply, modelled at the adapter-call
    level here for simplicity)."""

    name: str
    tokens_sent: int
    tokens_received: int

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        raise AdapterCallError(
            f"{self.name} outage", retry_info=RetryInfo(retryable=False)
        )


@dataclass(frozen=True)
class EchoAdapter:
    name: str
    tokens_sent: int = 3
    tokens_received: int = 5

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(
            text="ok", raw={}, tokens_sent=self.tokens_sent, tokens_received=self.tokens_received
        )


def test_fallback_attempts_each_carry_their_own_token_counts(
    monkeypatch,
) -> None:
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "task",
                "template": "hi",
                "provider_fallbacks": ["secondary:backup"],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    def fake_build_adapter(*, adapter_name: str, runtime: dict[str, object]):
        del runtime
        return EchoAdapter(name="secondary", tokens_sent=9, tokens_received=4)

    monkeypatch.setattr("circuitry.core.prompt.build_adapter", fake_build_adapter)

    store = Store({})
    DynamicRuntime(
        root,
        adapter=TokenSpendingFailAdapter(name="primary", tokens_sent=0, tokens_received=0),
        model="m",
    ).execute(store=store)

    attempts = store.get("prime.task.meta.fallback_attempts")
    assert attempts[0]["tokens_sent"] is None  # the primary never got a reply
    assert attempts[0]["tokens_received"] is None
    assert attempts[1]["tokens_sent"] == 9
    assert attempts[1]["tokens_received"] == 4

    # Winning-attempt fields unchanged in meaning.
    assert store.get("prime.task.meta.tokens_sent") == 9
    assert store.get("prime.task.meta.tokens_received") == 4
    # New totals: the winner's tokens, since the primary spent none.
    assert store.get("prime.task.meta.tokens_sent_total") == 9
    assert store.get("prime.task.meta.tokens_received_total") == 4


def test_node_totals_sum_tokens_across_retried_passes() -> None:
    @dataclass
    class FlakyTokenAdapter:
        name: str = "flaky"
        calls: int = 0

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            self.calls += 1
            if self.calls == 1:
                raise AdapterCallError(
                    "transient", retry_info=RetryInfo(retryable=True, status=429)
                )
            return GenerateResult(text="ok", raw={}, tokens_sent=10, tokens_received=20)

    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "task",
                "template": "hi",
                "retries": {"max_attempts": 2, "backoff_ms": 0},
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})

    DynamicRuntime(root, adapter=FlakyTokenAdapter(), model="m").execute(store=store)

    # Winning attempt: 10/20. Total: only the winner spent tokens (the
    # failed first pass's adapter call never got a reply) but the total
    # field still exists and matches.
    assert store.get("prime.task.meta.tokens_sent") == 10
    assert store.get("prime.task.meta.tokens_received") == 20
    assert store.get("prime.task.meta.tokens_sent_total") == 10
    assert store.get("prime.task.meta.tokens_received_total") == 20


def test_node_totals_include_a_failed_decode_attempts_tokens() -> None:
    """A decode-stage failure still spent tokens getting the reply —
    unlike a network-level failure, which never got one."""

    @dataclass
    class BadThenGoodAdapter:
        name: str = "flaky"
        calls: int = 0

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            self.calls += 1
            if self.calls == 1:
                return GenerateResult(
                    text="maybe", raw={}, tokens_sent=7, tokens_received=2
                )
            return GenerateResult(text="Yes.", raw={}, tokens_sent=3, tokens_received=1)

    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "task",
                "template": "hi",
                "prompt_type": "boolean",
                "retries": {"max_attempts": 2, "backoff_ms": 0},
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})

    DynamicRuntime(root, adapter=BadThenGoodAdapter(), model="m").execute(store=store)

    assert store.get("prime.task.value") is True
    assert store.get("prime.task.meta.tokens_sent") == 3
    assert store.get("prime.task.meta.tokens_received") == 1
    # Total includes the first (decode-failed) pass's 7/2 plus the winner's 3/1.
    assert store.get("prime.task.meta.tokens_sent_total") == 10
    assert store.get("prime.task.meta.tokens_received_total") == 3
