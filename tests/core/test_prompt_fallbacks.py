from __future__ import annotations

from collections.abc import Callable
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


def _capture_console_prints() -> tuple[list[str], Callable[[], None]]:
    """Patch ``output.console.print`` to record calls; returns (buffer, restore)."""
    import circuitry.output as output_mod

    captured: list[str] = []
    original_print = output_mod.console.print

    def capture(*args: object, **kwargs: object) -> None:
        captured.append(str(args[0]) if args else "")

    output_mod.console.print = capture  # type: ignore[method-assign]

    def restore() -> None:
        output_mod.console.print = original_print  # type: ignore[method-assign]

    return captured, restore


def test_verbose_label_reflects_prompt_provider_not_run_default(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A per-effect ``provider:`` must label the adapter dispatch actually
    uses, not the run default injected into the runtime — ``secondary`` is
    the effect's real first (and only) attempt here, so ``primary`` (the run
    default) should never appear in its verbose line.
    """
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "plan",
                "template": "hello",
                "provider": "secondary",
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    def fake_build_adapter(*, adapter_name: str, runtime: dict[str, object]):
        del runtime
        assert adapter_name == "secondary"
        return EchoAdapter(name="secondary")

    monkeypatch.setattr("circuitry.core.prompt.build_adapter", fake_build_adapter)

    captured, restore = _capture_console_prints()
    try:
        store = Store({})
        DynamicRuntime(
            root,
            adapter=AlwaysFailAdapter(name="primary"),
            model="run-default-model",
            verbose=True,
        ).execute(store=store)
    finally:
        restore()

    done = next((m for m in captured if "✓" in m and "plan" in m), None)
    assert done is not None
    assert "secondary" in done
    assert "primary" not in done
    assert store.get("prime.plan.meta.adapter") == "secondary"


def test_verbose_label_reflects_profile_provider_override(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A profile's ``effects.<path>.provider`` overlay lands on the same
    ``PromptDefinition.provider`` field a YAML ``provider:`` would, so it
    must produce the identical verbose label.
    """
    from circuitry.core.compiler import apply_effect_overrides

    orch = {
        "effects": [
            {"type": "prompt", "name": "plan", "template": "hello"},
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    root, matched = apply_effect_overrides(root, {"plan": {"provider": "secondary"}})
    assert matched == {"plan"}

    def fake_build_adapter(*, adapter_name: str, runtime: dict[str, object]):
        del runtime
        assert adapter_name == "secondary"
        return EchoAdapter(name="secondary")

    monkeypatch.setattr("circuitry.core.prompt.build_adapter", fake_build_adapter)

    captured, restore = _capture_console_prints()
    try:
        store = Store({})
        DynamicRuntime(
            root,
            adapter=AlwaysFailAdapter(name="primary"),
            model="run-default-model",
            verbose=True,
        ).execute(store=store)
    finally:
        restore()

    done = next((m for m in captured if "✓" in m and "plan" in m), None)
    assert done is not None
    assert "secondary" in done
    assert "primary" not in done


def test_verbose_completion_line_matches_meta_adapter_on_fallback(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """When the primary provider errors and a fallback answers, the
    completion line must name the fallback that actually answered — and
    agree, byte for byte, with ``meta.adapter`` — while still showing the
    failed primary in the chain.
    """
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "plan",
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

    captured, restore = _capture_console_prints()
    try:
        store = Store({})
        DynamicRuntime(
            root,
            adapter=AlwaysFailAdapter(name="primary"),
            model="primary-model",
            verbose=True,
        ).execute(store=store)
    finally:
        restore()

    done = next((m for m in captured if "✓" in m and "plan" in m), None)
    assert done is not None
    assert "primary ✗" in done
    assert "→" in done

    meta_adapter = store.get("prime.plan.meta.adapter")
    meta_model = store.get("prime.plan.meta.model")
    assert meta_adapter == "secondary"
    assert f"{meta_adapter} · {meta_model}" in done
