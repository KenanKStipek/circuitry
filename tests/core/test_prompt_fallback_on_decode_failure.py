"""Issue #281: an unreadable boolean/number or a schema-invalid JSON reply
moves to the next provider in ``provider_fallbacks`` before a retry is
counted, instead of restarting the whole chain from the primary.
"""

from __future__ import annotations

from dataclasses import dataclass

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass(frozen=True)
class FixedReplyAdapter:
    name: str
    text: str

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=self.text, raw={})


def test_an_unreadable_boolean_answer_falls_back_to_the_next_provider(
    monkeypatch,
) -> None:
    """Acceptance: a scripted primary answering 'maybe' and a fallback
    answering 'Yes.' gives ``true`` with ``meta.fallback_recovered: true``."""
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "task",
                "template": "hi",
                "prompt_type": "boolean",
                "provider_fallbacks": ["secondary:backup"],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    def fake_build_adapter(*, adapter_name: str, runtime: dict[str, object]):
        del runtime
        assert adapter_name == "secondary"
        return FixedReplyAdapter(name="secondary", text="Yes.")

    monkeypatch.setattr("circuitry.core.prompt.build_adapter", fake_build_adapter)

    store = Store({})
    DynamicRuntime(
        root, adapter=FixedReplyAdapter(name="primary", text="maybe"), model="m"
    ).execute(store=store)

    assert store.get("prime.task.value") is True
    assert store.get("prime.task.meta.fallback_recovered") is True
    attempts = store.get("prime.task.meta.fallback_attempts")
    assert [a["adapter"] for a in attempts] == ["primary", "secondary"]
    assert attempts[0]["status"] == "decode_failed"
    assert attempts[0]["raw_reply"] == "maybe"
    assert attempts[1]["status"] == "succeeded"
    # No retry was spent getting here — the fallback chain absorbed it.
    assert "retries_used" not in (store.get("prime.task.meta") or {})


def test_a_schema_invalid_reply_falls_back_to_the_next_provider(monkeypatch) -> None:
    """Acceptance: the same recovery for schema-invalid JSON."""
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "task",
                "template": "hi",
                "prompt_type": "json",
                "schema": {"type": "object", "required": ["ok"]},
                "provider_fallbacks": ["secondary:backup"],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    def fake_build_adapter(*, adapter_name: str, runtime: dict[str, object]):
        del runtime
        return FixedReplyAdapter(name="secondary", text='{"ok": true}')

    monkeypatch.setattr("circuitry.core.prompt.build_adapter", fake_build_adapter)

    store = Store({})
    DynamicRuntime(
        root,
        adapter=FixedReplyAdapter(name="primary", text='{"nope": 1}'),
        model="m",
    ).execute(store=store)

    assert store.get("prime.task.value") == {"ok": True}
    assert store.get("prime.task.meta.fallback_recovered") is True
    attempts = store.get("prime.task.meta.fallback_attempts")
    assert attempts[0]["status"] == "schema_invalid"
    assert attempts[1]["status"] == "succeeded"


def test_with_no_fallbacks_configured_a_decode_failure_behaves_as_before(
) -> None:
    """#281's explicit non-regression: no ``provider_fallbacks`` means the
    unreadable-reply failure path is unchanged — it still fails the effect
    (default ``retries: 0``), with the raw reply on ``meta.answer`` exactly
    as before this fix."""
    orch = {
        "effects": [
            {"type": "prompt", "name": "task", "template": "hi", "prompt_type": "boolean"}
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})

    try:
        DynamicRuntime(
            root, adapter=FixedReplyAdapter(name="primary", text="maybe"), model="m"
        ).execute(store=store)
    except RuntimeError:
        pass

    assert store.get("prime.task.value") is None
    assert store.get("prime.task.meta.answer") == "maybe"
    assert "maybe" in store.get("prime.task.meta.error")
