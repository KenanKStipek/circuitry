from __future__ import annotations

from dataclasses import dataclass

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.prompt import PromptDefinition, PromptRuntime
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


def test_prompt_error_meta_is_redacted_before_storage(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """An adapter exception that is itself a bare credential-shaped string
    (e.g. an SDK surfacing a raw API key) must not land verbatim in
    ``meta.error`` / ``meta.fallback_attempts`` — both are stored in run
    state and echoed by ``--out``, ``--print``, ``--live-state``, etc.
    """
    from circuitry.cli.redaction import REDACTED

    secret = "sk-" + "a" * 30

    @dataclass(frozen=True)
    class LeakyAdapter:
        name: str

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            raise RuntimeError(secret)

    orch = {
        "effects": [{"type": "prompt", "name": "task", "template": "hello"}]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    store = Store({})
    with pytest.raises(RuntimeError):
        DynamicRuntime(
            root, adapter=LeakyAdapter(name="primary"), model="m"
        ).execute(store=store)

    error = store.get("prime.task.meta.error")
    assert secret not in error
    attempts = store.get("prime.task.meta.fallback_attempts")
    assert secret not in attempts[0]["error"]
    assert attempts[0]["error"] == REDACTED


def _capture_console_prints(monkeypatch: pytest.MonkeyPatch) -> list[str]:
    """Patch ``output.console.print`` to record calls."""
    import circuitry.output as output_mod

    captured: list[str] = []

    def capture(*args: object, **kwargs: object) -> None:
        captured.append(str(args[0]) if args else "")

    monkeypatch.setattr(output_mod.console, "print", capture)
    return captured


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

    captured = _capture_console_prints(monkeypatch)
    store = Store({})
    DynamicRuntime(
        root,
        adapter=AlwaysFailAdapter(name="primary"),
        model="run-default-model",
        verbose=True,
    ).execute(store=store)

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

    captured = _capture_console_prints(monkeypatch)
    store = Store({})
    DynamicRuntime(
        root,
        adapter=AlwaysFailAdapter(name="primary"),
        model="run-default-model",
        verbose=True,
    ).execute(store=store)

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

    captured = _capture_console_prints(monkeypatch)
    store = Store({})
    DynamicRuntime(
        root,
        adapter=AlwaysFailAdapter(name="primary"),
        model="primary-model",
        verbose=True,
    ).execute(store=store)

    done = next((m for m in captured if "✓" in m and "plan" in m), None)
    assert done is not None
    assert "primary ✗" in done
    assert "→" in done

    meta_adapter = store.get("prime.plan.meta.adapter")
    meta_model = store.get("prime.plan.meta.model")
    assert meta_adapter == "secondary"
    assert f"{meta_adapter} · {meta_model}" in done


def test_verbose_running_label_reflects_prompt_provider_not_run_default(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The start/running label (built by ``_pre_dispatch_target`` before any
    attempt runs) must already name the effect's real first attempt, not the
    run default — unlike the completion line, this label does not depend on
    ``attempts_meta`` and would not catch a regression back to ``self.adapter``.
    """

    def fake_build_adapter(*, adapter_name: str, runtime: dict[str, object]):
        del runtime
        assert adapter_name == "secondary"
        return EchoAdapter(name="secondary")

    monkeypatch.setattr("circuitry.core.prompt.build_adapter", fake_build_adapter)

    running_calls: list[tuple[str, int]] = []

    def cb_running(target: str, estimated_out: int) -> None:
        running_calls.append((target, estimated_out))

    store = Store({})
    PromptRuntime(
        PromptDefinition(name="plan", template="hello", provider="secondary"),
        adapter=AlwaysFailAdapter(name="primary"),
        model="run-default-model",
        verbose=True,
        cb_running=cb_running,
    ).execute(store=store, ctx={})

    assert len(running_calls) == 1
    target, _estimated_out = running_calls[0]
    assert "secondary" in target
    assert "primary" not in target


def test_verbose_running_label_reflects_profile_provider_override(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Same as above, via a profile's ``effects.<path>.provider`` overlay
    (``apply_effect_overrides``) rather than a YAML ``provider:``.
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
    plan_defn = next(e for e in root.effects if getattr(e, "name", None) == "plan")
    assert isinstance(plan_defn, PromptDefinition)

    def fake_build_adapter(*, adapter_name: str, runtime: dict[str, object]):
        del runtime
        assert adapter_name == "secondary"
        return EchoAdapter(name="secondary")

    monkeypatch.setattr("circuitry.core.prompt.build_adapter", fake_build_adapter)

    running_calls: list[tuple[str, int]] = []

    def cb_running(target: str, estimated_out: int) -> None:
        running_calls.append((target, estimated_out))

    store = Store({})
    PromptRuntime(
        plan_defn,
        adapter=AlwaysFailAdapter(name="primary"),
        model="run-default-model",
        verbose=True,
        cb_running=cb_running,
    ).execute(store=store, ctx={})

    assert len(running_calls) == 1
    target, _estimated_out = running_calls[0]
    assert "secondary" in target
    assert "primary" not in target


def test_verbose_bad_provider_does_not_crash_and_records_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A ``provider:`` naming an adapter that fails to build (unknown name,
    or one like ``host_claude`` that refuses config-based construction) must
    still go through the normal error path under ``--verbose``: ``on_error``
    respected, ``meta.error`` set, ``execute`` does not raise. Before the P0
    fix, ``_pre_dispatch_target`` built the adapter ahead of the dispatch
    ``try`` block, so this same setup raised straight out of ``execute``.
    """
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "plan",
                "template": "hello",
                "provider": "nonexistent",
                "on_error": "continue",
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    from circuitry.adapters.factory import build_adapter as real_build_adapter

    monkeypatch.setattr("circuitry.core.prompt.build_adapter", real_build_adapter)

    store = Store({})
    DynamicRuntime(
        root,
        adapter=AlwaysFailAdapter(name="primary"),
        model="run-default-model",
        verbose=True,
    ).execute(store=store)

    assert store.get("prime.plan.value") is None
    error = store.get("prime.plan.meta.error")
    assert isinstance(error, str)
    assert "nonexistent" in error
    assert store.get("prime.plan.meta.completed_at") is not None
