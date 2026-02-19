from __future__ import annotations

from dataclasses import dataclass

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass(frozen=True)
class AlwaysFailAdapter:
    name: str

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        raise RuntimeError(f"{self.name} outage")


@dataclass(frozen=True)
class EchoAdapter:
    name: str

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=f"{self.name}:{model}:{prompt}", raw={})


def test_prompt_fallback_recovers_and_records_attempt_chain(monkeypatch: pytest.MonkeyPatch) -> None:
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "task",
                "template": "hello",
                "provider_fallbacks": ["secondary:backup-model"],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    def fake_build_adapter(*, adapter_name: str, runtime: dict[str, object]):
        del runtime
        if adapter_name == "secondary":
            return EchoAdapter(name="secondary")
        raise RuntimeError(f"unknown adapter requested: {adapter_name}")

    monkeypatch.setattr("circuitry.core.prompt.build_adapter", fake_build_adapter)

    store = Store({})
    DynamicRuntime(
        root,
        adapter=AlwaysFailAdapter(name="primary"),
        model="primary-model",
    ).execute(store=store)

    assert store.get("prime.task.value") == "secondary:backup-model:hello"
    attempts = store.get("prime.task.meta.fallback_attempts")
    assert isinstance(attempts, list)
    assert [a["adapter"] for a in attempts] == ["primary", "secondary"]
    assert [a["status"] for a in attempts] == ["failed", "succeeded"]
    assert store.get("prime.task.meta.fallback_recovered") is True


def test_prompt_fallback_exhaustion_surfaces_structured_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "task",
                "template": "hello",
                "provider_fallbacks": ["secondary:backup-model"],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    def fake_build_adapter(*, adapter_name: str, runtime: dict[str, object]):
        del runtime
        return AlwaysFailAdapter(name=adapter_name)

    monkeypatch.setattr("circuitry.core.prompt.build_adapter", fake_build_adapter)

    store = Store({})
    with pytest.raises(RuntimeError):
        DynamicRuntime(
            root,
            adapter=AlwaysFailAdapter(name="primary"),
            model="primary-model",
        ).execute(store=store)

    attempts = store.get("prime.task.meta.fallback_attempts")
    assert isinstance(attempts, list)
    assert [a["adapter"] for a in attempts] == ["primary", "secondary"]
    assert [a["status"] for a in attempts] == ["failed", "failed"]
    error = store.get("prime.task.meta.error")
    assert isinstance(error, str)
    assert "All adapter attempts failed" in error
