"""Issue #660: `meta.error_code`/`meta.http_status` on a failed prompt.

Both fields come from the attempt whose error ended the effect — the last
entry of the final pass's `meta.fallback_attempts` — and are absent,
never `None`, whenever that attempt didn't carry one.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from circuitry.adapters._retry import AdapterCallError, RetryInfo
from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


def _single_prompt_orch(*, on_error: str = "fail", max_attempts: int = 1) -> dict:
    return {
        "effects": [
            {
                "type": "prompt",
                "name": "task",
                "template": "hi",
                "retries": {"max_attempts": max_attempts, "backoff_ms": 0},
                "on_error": on_error,
            }
        ]
    }


@dataclass
class ScriptedFailAdapter:
    name: str = "scripted"
    failures: list[Exception] = field(default_factory=list)
    reply: GenerateResult | None = None
    calls: int = 0

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls += 1
        if self.calls <= len(self.failures):
            raise self.failures[self.calls - 1]
        return self.reply or GenerateResult(text="ok", raw={})


def test_fields_present_on_an_outright_failure() -> None:
    root = compile_orchestration(orch=_single_prompt_orch(on_error="continue"), root_name="prime")
    adapter = ScriptedFailAdapter(
        failures=[
            AdapterCallError(
                "guardrail tripped",
                retry_info=RetryInfo(retryable=False, status=400),
                error_code="guardrail_violation",
            )
        ]
    )
    store = Store({})
    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    assert store.get("prime.task.meta.error_code") == "guardrail_violation"
    assert store.get("prime.task.meta.http_status") == 400
    attempts = store.get("prime.task.meta.fallback_attempts")
    assert attempts[-1]["error_code"] == "guardrail_violation"
    assert attempts[-1]["http_status"] == 400
    error_text = store.get("prime.task.meta.error")
    assert "'error_code': 'guardrail_violation'" in error_text
    assert "'http_status': 400" in error_text


def test_fields_present_under_on_error_fail() -> None:
    import pytest

    root = compile_orchestration(orch=_single_prompt_orch(on_error="fail"), root_name="prime")
    adapter = ScriptedFailAdapter(
        failures=[
            AdapterCallError(
                "boom",
                retry_info=RetryInfo(retryable=False, status=503),
                error_code="server_error",
            )
        ]
    )
    store = Store({})
    with pytest.raises(RuntimeError):
        DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    assert store.get("prime.task.meta.error_code") == "server_error"
    assert store.get("prime.task.meta.http_status") == 503


def test_fields_absent_on_success() -> None:
    root = compile_orchestration(orch=_single_prompt_orch(), root_name="prime")
    adapter = ScriptedFailAdapter()
    store = Store({})
    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    meta = store.get("prime.task.meta")
    assert "error_code" not in meta
    assert "http_status" not in meta


def test_fields_absent_on_a_fallback_that_recovered() -> None:
    root = compile_orchestration(
        orch={
            "effects": [
                {
                    "type": "prompt",
                    "name": "task",
                    "template": "hi",
                    "provider_fallbacks": ["secondary:m2"],
                }
            ]
        },
        root_name="prime",
    )

    @dataclass(frozen=True)
    class EchoAdapter:
        name: str

        def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120):
            return GenerateResult(text="ok", raw={})

    import pytest

    primary = ScriptedFailAdapter(
        failures=[
            AdapterCallError(
                "bad request",
                retry_info=RetryInfo(retryable=False, status=400),
                error_code="context_length_exceeded",
            )
        ]
    )

    def fake_build_adapter(*, adapter_name: str, runtime: dict[str, object]):
        del runtime
        assert adapter_name == "secondary"
        return EchoAdapter(name="secondary")

    import circuitry.core.prompt as prompt_mod
    monkeypatch = pytest.MonkeyPatch()
    monkeypatch.setattr(prompt_mod, "build_adapter", fake_build_adapter)
    try:
        store = Store({})
        DynamicRuntime(root, adapter=primary, model="m").execute(store=store)
    finally:
        monkeypatch.undo()

    meta = store.get("prime.task.meta")
    assert "error_code" not in meta
    assert "http_status" not in meta
    attempts = store.get("prime.task.meta.fallback_attempts")
    assert attempts[0]["error_code"] == "context_length_exceeded"
    assert attempts[0]["http_status"] == 400
    assert "error_code" not in attempts[1]
    assert "http_status" not in attempts[1]


def test_fields_come_from_the_last_fallback_attempt_not_the_first() -> None:
    import pytest

    root = compile_orchestration(
        orch={
            "effects": [
                {
                    "type": "prompt",
                    "name": "task",
                    "template": "hi",
                    "provider_fallbacks": ["secondary:m2"],
                    "on_error": "continue",
                }
            ]
        },
        root_name="prime",
    )

    primary = ScriptedFailAdapter(
        failures=[
            AdapterCallError(
                "bad request",
                retry_info=RetryInfo(retryable=False, status=400),
                error_code="guardrail_violation",
            )
        ]
    )
    secondary = ScriptedFailAdapter(
        name="secondary",
        failures=[
            AdapterCallError(
                "unavailable",
                retry_info=RetryInfo(retryable=False, status=503),
                error_code="server_error",
            )
        ],
    )

    def fake_build_adapter(*, adapter_name: str, runtime: dict[str, object]):
        del runtime
        assert adapter_name == "secondary"
        return secondary

    import circuitry.core.prompt as prompt_mod
    monkeypatch = pytest.MonkeyPatch()
    monkeypatch.setattr(prompt_mod, "build_adapter", fake_build_adapter)
    try:
        store = Store({})
        DynamicRuntime(root, adapter=primary, model="m").execute(store=store)
    finally:
        monkeypatch.undo()

    assert store.get("prime.task.meta.error_code") == "server_error"
    assert store.get("prime.task.meta.http_status") == 503
    attempts = store.get("prime.task.meta.fallback_attempts")
    assert attempts[0]["error_code"] == "guardrail_violation"
    assert attempts[0]["http_status"] == 400
    assert attempts[1]["error_code"] == "server_error"
    assert attempts[1]["http_status"] == 503


def test_fields_absent_on_a_connection_failure() -> None:
    root = compile_orchestration(orch=_single_prompt_orch(on_error="continue"), root_name="prime")
    adapter = ScriptedFailAdapter(
        failures=[AdapterCallError("connection refused", retry_info=RetryInfo(retryable=True))]
    )
    store = Store({})
    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    meta = store.get("prime.task.meta")
    assert "error_code" not in meta
    assert "http_status" not in meta


def test_fields_absent_on_a_missing_key_failure() -> None:
    root = compile_orchestration(orch=_single_prompt_orch(on_error="continue"), root_name="prime")
    adapter = ScriptedFailAdapter(failures=[RuntimeError("API key not found")])
    store = Store({})
    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    meta = store.get("prime.task.meta")
    assert "error_code" not in meta
    assert "http_status" not in meta


def test_fields_absent_on_a_decode_failure() -> None:
    root = compile_orchestration(
        orch={
            "effects": [
                {
                    "type": "prompt",
                    "name": "task",
                    "template": "hi",
                    "prompt_type": "number",
                    "on_error": "continue",
                }
            ]
        },
        root_name="prime",
    )
    adapter = ScriptedFailAdapter(reply=GenerateResult(text="not a number", raw={}))
    store = Store({})
    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    meta = store.get("prime.task.meta")
    assert "error_code" not in meta
    assert "http_status" not in meta


def test_fields_reset_on_a_reused_node() -> None:
    """An unnamed loop's second pass must not keep the first pass's fields
    when the second pass's own failure (or success) carries none (#260's
    rule, extended to these two keys)."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {
                        "type": "prompt",
                        "name": "step",
                        "template": "{{item}}",
                        "on_error": "continue",
                    }
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    @dataclass
    class FlakyAdapter:
        name: str = "flaky"
        calls: int = 0

        def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120):
            self.calls += 1
            if self.calls == 1:
                raise AdapterCallError(
                    "pass 0 failure",
                    retry_info=RetryInfo(retryable=False, status=400),
                    error_code="bad_request",
                )
            return GenerateResult(text="ok", raw={})

    state = {"input": {"items": ["a", "b"]}}
    DynamicRuntime(root, adapter=FlakyAdapter(), model="m").execute(store=Store(state))

    node = state["prime"]["step"]
    assert "error_code" not in node["meta"]
    assert "http_status" not in node["meta"]
    assert node["value"] == "ok"
