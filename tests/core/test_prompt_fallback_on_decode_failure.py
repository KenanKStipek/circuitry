"""Issue #281: an unreadable boolean/number or a schema-invalid JSON reply
moves to the next provider in ``provider_fallbacks`` before a retry is
counted, instead of restarting the whole chain from the primary.
"""

from __future__ import annotations

from dataclasses import dataclass, field

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


def test_ordering_with_retries_the_primary_is_tried_once_per_pass_before_the_fallback(
    monkeypatch,
) -> None:
    """#281 with ``max_attempts > 1``: both providers fail decode on the
    first pass (so the whole chain retries), and the second pass's primary
    fails decode again before the fallback finally answers — proving the
    primary is retried once per *pass*, not spun through repeatedly before
    the fallback ever gets a turn."""

    @dataclass
    class ScriptedAdapter:
        name: str
        replies: list[str]
        calls: int = 0

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            text = self.replies[min(self.calls, len(self.replies) - 1)]
            self.calls += 1
            return GenerateResult(text=text, raw={})

    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "task",
                "template": "hi",
                "prompt_type": "boolean",
                "provider_fallbacks": ["secondary:backup"],
                "retries": {"max_attempts": 2, "backoff_ms": 0},
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    primary = ScriptedAdapter(name="primary", replies=["maybe", "maybe"])
    secondary = ScriptedAdapter(name="secondary", replies=["maybe", "Yes."])

    def fake_build_adapter(*, adapter_name: str, runtime: dict[str, object]):
        del runtime
        assert adapter_name == "secondary"
        return secondary

    monkeypatch.setattr("circuitry.core.prompt.build_adapter", fake_build_adapter)

    store = Store({})
    DynamicRuntime(root, adapter=primary, model="m").execute(store=store)

    assert store.get("prime.task.value") is True
    # One primary + one secondary call per pass, two passes: the primary
    # never ran more than once before the fallback got its turn on a pass.
    assert primary.calls == 2
    assert secondary.calls == 2
    assert store.get("prime.task.meta.retries_used") == 1
    assert store.get("prime.task.meta.fallback_recovered") is True


def test_a_non_retryable_dispatch_failure_still_moves_to_the_next_fallback(
    monkeypatch,
) -> None:
    """A plain adapter-call failure (not a decode/schema one) that classifies
    as non-retryable still isn't the end of the chain — provider_fallbacks
    tries the next provider regardless of why the previous one failed; only
    the *last* attempt's classification governs whether the whole pass
    retries."""
    from circuitry.adapters._retry import AdapterCallError, RetryInfo

    @dataclass
    class FailingAdapter:
        name: str

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            raise AdapterCallError(
                "unauthorized", retry_info=RetryInfo(retryable=False, status=401)
            )

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
        assert adapter_name == "secondary"
        return FixedReplyAdapter(name="secondary", text="from fallback")

    monkeypatch.setattr("circuitry.core.prompt.build_adapter", fake_build_adapter)

    store = Store({})
    DynamicRuntime(
        root, adapter=FailingAdapter(name="primary"), model="m"
    ).execute(store=store)

    assert store.get("prime.task.value") == "from fallback"
    attempts = store.get("prime.task.meta.fallback_attempts")
    assert [a["adapter"] for a in attempts] == ["primary", "secondary"]
    assert attempts[0]["status"] == "failed"


def test_a_timeout_reached_through_provider_fallbacks_uses_its_own_configured_timeout(
    monkeypatch,
) -> None:
    """#263 part 1's per-attempt timeout lookup applies the same way to an
    adapter reached through ``provider_fallbacks`` as it does to one named
    directly by ``provider:`` (see test_prompt_adapter_timeouts.py for that
    case)."""

    @dataclass
    class RecordingFallbackAdapter:
        name: str = "secondary"
        calls: list[int] = field(default_factory=list)

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            self.calls.append(timeout_seconds)
            return GenerateResult(text="ok", raw={})

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

    recorder = RecordingFallbackAdapter()

    def fake_build_adapter(*, adapter_name: str, runtime: dict[str, object]):
        del runtime
        assert adapter_name == "secondary"
        return recorder

    monkeypatch.setattr("circuitry.core.prompt.build_adapter", fake_build_adapter)

    @dataclass
    class AlwaysFailsPrimary:
        name: str = "primary"

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            raise RuntimeError("primary unavailable")

    store = Store({})
    DynamicRuntime(
        root,
        adapter=AlwaysFailsPrimary(),
        model="m",
        runtime_config={"adapters": {"secondary": {"timeout_seconds": 7}}},
        timeout_seconds=120,
    ).execute(store=store)

    assert recorder.calls == [7]


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
