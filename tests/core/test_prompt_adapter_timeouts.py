"""Issue #263 part 1: a per-attempt timeout is resolved for the adapter that
attempt actually dispatches to, not the run default's adapter.

Before this fix, ``PromptRuntime._attempt_timeout_seconds`` only ever read
the timeout computed once for the run-default adapter
(``cli/runtime_shim.py``'s ``timeout_seconds``); a ``provider:``/fallback
that sent an attempt to a *different* adapter still ran with the run
default's timeout, so ``runtime.adapters.ollama.timeout_seconds`` did
nothing unless ``ollama`` also happened to be ``default_adapter``.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class RecordingAdapter:
    name: str = "secondary"
    calls: list[int] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append(timeout_seconds)
        return GenerateResult(text="ok", raw={})


def test_a_dispatched_fallback_adapter_uses_its_own_configured_timeout(
    monkeypatch,
) -> None:
    """``provider: secondary`` with a run default of ``primary`` (120s, the
    default) must run at ``secondary``'s own configured 5s, not 120s.
    """
    orch = {
        "effects": [
            {"type": "prompt", "name": "task", "template": "hi", "provider": "secondary"}
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    recorder = RecordingAdapter()

    def fake_build_adapter(*, adapter_name: str, runtime: dict[str, object]):
        del runtime
        assert adapter_name == "secondary"
        return recorder

    monkeypatch.setattr("circuitry.core.prompt.build_adapter", fake_build_adapter)

    @dataclass
    class NeverCalledAdapter:
        name: str = "primary"

        def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120):
            raise AssertionError("the run-default adapter must not be dispatched")

    store = Store({})
    DynamicRuntime(
        root,
        adapter=NeverCalledAdapter(),
        model="m",
        runtime_config={"adapters": {"secondary": {"timeout_seconds": 5}}},
        timeout_seconds=120,
    ).execute(store=store)

    assert recorder.calls == [5]


def test_a_dispatched_fallback_adapter_without_its_own_config_uses_the_run_default(
    monkeypatch,
) -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "task", "template": "hi", "provider": "secondary"}
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    recorder = RecordingAdapter()

    def fake_build_adapter(*, adapter_name: str, runtime: dict[str, object]):
        del runtime
        return recorder

    monkeypatch.setattr("circuitry.core.prompt.build_adapter", fake_build_adapter)

    @dataclass
    class NeverCalledAdapter:
        name: str = "primary"

        def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120):
            raise AssertionError("not the run default this time either")

    store = Store({})
    DynamicRuntime(
        root,
        adapter=NeverCalledAdapter(),
        model="m",
        runtime_config={},
        timeout_seconds=120,
    ).execute(store=store)

    assert recorder.calls == [120]


def test_timeout_ms_still_caps_the_dispatched_adapters_own_timeout(
    monkeypatch,
) -> None:
    """``timeout_ms`` only ever shortens: a per-effect budget under the
    dispatched adapter's own configured timeout still wins."""
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "task",
                "template": "hi",
                "provider": "secondary",
                "timeout_ms": 2500,
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    recorder = RecordingAdapter()
    monkeypatch.setattr(
        "circuitry.core.prompt.build_adapter",
        lambda *, adapter_name, runtime: recorder,
    )

    store = Store({})
    DynamicRuntime(
        root,
        adapter=recorder,
        model="m",
        runtime_config={"adapters": {"secondary": {"timeout_seconds": 60}}},
        timeout_seconds=120,
    ).execute(store=store)

    assert recorder.calls == [3]  # ceil(2500 / 1000)


def test_litellm_timeout_alias_reaches_the_per_attempt_dispatch(
    monkeypatch,
) -> None:
    """#263 part 1: ``runtime.adapters.litellm.timeout`` (the deprecated
    alias) must reach a per-attempt dispatch resolved through
    ``_attempt_timeout_seconds``, not just the adapter instance's own
    default — the same lookup a run-default litellm adapter goes through
    (see test_adapter_timeout_config.py for that path)."""
    orch = {
        "effects": [
            {"type": "prompt", "name": "task", "template": "hi", "provider": "litellm"}
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    recorder = RecordingAdapter(name="litellm")
    monkeypatch.setattr(
        "circuitry.core.prompt.build_adapter",
        lambda *, adapter_name, runtime: recorder,
    )

    store = Store({})
    DynamicRuntime(
        root,
        adapter=recorder,
        model="m",
        runtime_config={"adapters": {"litellm": {"timeout": 45}}},
        timeout_seconds=120,
    ).execute(store=store)

    assert recorder.calls == [45]


def test_adapter_name_lookup_is_normalised_to_lower_case(monkeypatch) -> None:
    """``build_adapter`` normalises an adapter name to ``.strip().lower()``
    before reading its config; the timeout lookup must match, or
    ``provider: Ollama`` silently misses ``runtime.adapters.ollama``."""
    orch = {
        "effects": [
            {"type": "prompt", "name": "task", "template": "hi", "provider": "Secondary"}
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    recorder = RecordingAdapter()
    monkeypatch.setattr(
        "circuitry.core.prompt.build_adapter",
        lambda *, adapter_name, runtime: recorder,
    )

    store = Store({})
    DynamicRuntime(
        root,
        adapter=recorder,
        model="m",
        runtime_config={"adapters": {"secondary": {"timeout_seconds": 5}}},
        timeout_seconds=120,
    ).execute(store=store)

    assert recorder.calls == [5]


def test_a_zero_or_negative_configured_timeout_falls_back_to_the_run_default(
    monkeypatch,
) -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "task", "template": "hi", "provider": "secondary"}
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")

    recorder = RecordingAdapter()
    monkeypatch.setattr(
        "circuitry.core.prompt.build_adapter",
        lambda *, adapter_name, runtime: recorder,
    )

    store = Store({})
    DynamicRuntime(
        root,
        adapter=recorder,
        model="m",
        runtime_config={"adapters": {"secondary": {"timeout_seconds": 0}}},
        timeout_seconds=99,
    ).execute(store=store)

    assert recorder.calls == [99]
