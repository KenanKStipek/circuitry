"""``meta.attempts`` on a retried `prompt` (#668), mirroring `use`'s own
(#659): one entry per attempt, oldest first, present only when more than
one attempt ran.
"""

from __future__ import annotations

from copy import deepcopy
from dataclasses import dataclass

import pytest

from circuitry.adapters._retry import AdapterCallError, RetryInfo
from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.prompt import PromptDefinition, PromptRuntime, RetryPolicyDef
from circuitry.core.store import Store


def _patch_sleep(monkeypatch: pytest.MonkeyPatch, fn) -> None:
    from circuitry.core.cancellation import get_token

    monkeypatch.setattr(get_token(), "sleep_or_raise", fn)


@dataclass
class _MockAdapter:
    name: str = "mock"

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120):
        return GenerateResult(text="ok", raw={})


def _mock_adapter() -> _MockAdapter:
    return _MockAdapter()


@dataclass
class _ScriptedAdapter:
    """Fails with a retryable 503, then a schema-invalid reply, then
    succeeds -- the two distinct retryable failure modes a prompt's own
    fallback chain can hand back to the retry loop (#263, #281)."""

    name: str = "primary"
    calls: int = 0

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120):
        self.calls += 1
        if self.calls == 1:
            raise AdapterCallError(
                "primary unavailable", retry_info=RetryInfo(retryable=True, status=503)
            )
        if self.calls == 2:
            return GenerateResult(text='{"nope": 1}', raw={})
        return GenerateResult(text='{"ok": true}', raw={})


def _json_retry_orch(max_attempts: int = 3) -> dict:
    return {
        "effects": [
            {
                "type": "prompt",
                "name": "task",
                "template": "hi",
                "prompt_type": "json",
                "schema": {"type": "object", "required": ["ok"]},
                "retries": {"max_attempts": max_attempts, "backoff_ms": 0},
            }
        ]
    }


def test_attempts_absent_when_only_one_attempt_ran(monkeypatch: pytest.MonkeyPatch) -> None:
    _patch_sleep(monkeypatch, lambda s: None)
    root = compile_orchestration(orch=_json_retry_orch(max_attempts=3), root_name="prime")
    store = Store({})

    @dataclass
    class _AlwaysOk:
        name: str = "primary"

        def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120):
            return GenerateResult(text='{"ok": true}', raw={})

    DynamicRuntime(root, adapter=_AlwaysOk(), model="m").execute(store=store)

    assert "attempts" not in store.state["prime"]["task"]["meta"]
    assert "retries_used" not in store.state["prime"]["task"]["meta"]


def test_attempts_retried_on_a_503_then_a_schema_mismatch_then_succeeds(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _patch_sleep(monkeypatch, lambda s: None)
    root = compile_orchestration(orch=_json_retry_orch(max_attempts=3), root_name="prime")
    store = Store({})
    adapter = _ScriptedAdapter()

    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    node = store.state["prime"]["task"]
    meta = node["meta"]
    assert node["value"] == {"ok": True}
    assert meta["retries_used"] == 2
    assert list(meta)[-1] == "attempts"

    attempts = meta["attempts"]
    assert len(attempts) == 3

    assert attempts[0]["value"] is None
    assert "primary unavailable" in attempts[0]["error"]
    assert [a["status"] for a in attempts[0]["fallback_attempts"]] == ["failed"]
    assert attempts[0]["tokens_sent"] is None
    assert attempts[0]["tokens_received"] is None

    assert attempts[1]["value"] is None
    assert attempts[1]["error"] is not None
    assert [a["status"] for a in attempts[1]["fallback_attempts"]] == ["schema_invalid"]

    assert attempts[2]["value"] == {"ok": True}
    assert attempts[2]["error"] is None
    assert [a["status"] for a in attempts[2]["fallback_attempts"]] == ["succeeded"]

    # The deciding (last) attempt repeats the node's own fields.
    assert attempts[-1]["value"] == node["value"]
    assert attempts[-1]["error"] == meta["error"]
    assert attempts[-1]["completed_at"] == meta["completed_at"]
    # meta.created_at is set ahead of the loop (so it's on the node before
    # effect_start fires), the first attempt's own entry a moment later --
    # close, not identical.
    assert attempts[0]["created_at"] >= meta["created_at"]

    # Prompts have no `expect`, unlike a tool or use effect's own entry.
    assert "expect" not in attempts[0]


def test_attempts_entry_key_order(monkeypatch: pytest.MonkeyPatch) -> None:
    _patch_sleep(monkeypatch, lambda s: None)
    root = compile_orchestration(orch=_json_retry_orch(max_attempts=3), root_name="prime")
    store = Store({})

    DynamicRuntime(root, adapter=_ScriptedAdapter(), model="m").execute(store=store)

    attempts = store.state["prime"]["task"]["meta"]["attempts"]
    for entry in attempts:
        assert list(entry) == [
            "created_at",
            "completed_at",
            "value",
            "error",
            "fallback_attempts",
            "tokens_sent",
            "tokens_received",
        ]


def test_attempts_kept_on_continue_after_retries_exhausted(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _patch_sleep(monkeypatch, lambda s: None)
    orch = _json_retry_orch(max_attempts=2)
    orch["effects"][0]["on_error"] = "continue"
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})

    @dataclass
    class _AlwaysBadSchema:
        name: str = "primary"

        def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120):
            return GenerateResult(text='{"nope": 1}', raw={})

    DynamicRuntime(root, adapter=_AlwaysBadSchema(), model="m").execute(store=store)

    node = store.state["prime"]["task"]
    assert node["value"] is None
    attempts = node["meta"]["attempts"]
    assert len(attempts) == 2
    assert all(a["error"] is not None for a in attempts)
    assert node["meta"]["retries_used"] == 1


def test_attempts_is_reset_on_a_reused_node() -> None:
    defn = PromptDefinition(name="task", template="hi")
    store = Store({})
    node = store.ensure_dict("task")
    node["meta"] = {
        "retries_used": 2,
        "attempts": [
            {
                "created_at": "T",
                "completed_at": "T",
                "value": None,
                "error": "boom",
                "fallback_attempts": [],
                "tokens_sent": None,
                "tokens_received": None,
            }
        ],
    }

    PromptRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    assert "attempts" not in store.state["task"]["meta"]
    assert "retries_used" not in store.state["task"]["meta"]


def test_attempts_live_published_before_the_backoff_wait(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Each entry is written and published as its attempt ends and
    another follows, before the backoff wait."""
    seen: list[int] = []

    def _sleep(seconds: float) -> None:
        seen.append(seconds)

    _patch_sleep(monkeypatch, _sleep)

    published: list[dict] = []
    root = compile_orchestration(orch=_json_retry_orch(max_attempts=3), root_name="prime")
    store = Store({}, on_write=lambda state: published.append(deepcopy(state)))
    adapter = _ScriptedAdapter()

    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    # By the time the run ends, at least one published snapshot already
    # shows the first attempt recorded while fewer than all three had run.
    assert any(
        "attempts" in (snap.get("prime", {}).get("task", {}).get("meta") or {})
        and len(snap["prime"]["task"]["meta"]["attempts"]) < 3
        for snap in published
    )
