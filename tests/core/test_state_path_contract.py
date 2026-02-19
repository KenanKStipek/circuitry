from __future__ import annotations

from dataclasses import dataclass

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass(frozen=True)
class EchoAdapter:
    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=prompt, raw={"model": model, "prompt": prompt})


def test_named_conditional_writes_under_wrapper_path() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "name": "gate",
                "if": {"mode": "cel", "expr": "state.input.ok == True"},
                "then": [{"type": "prompt", "name": "show_admin", "template": "ok"}],
                "else": [{"type": "prompt", "name": "show_user", "template": "no"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"ok": True}})
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    assert isinstance(store.get("prime.gate.value"), dict)
    assert store.get("prime.gate.show_admin.value") == "ok"


def test_transparent_conditional_does_not_create_wrapper_segment() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "if": {"mode": "cel", "expr": "state.input.ok == True"},
                "then": [{"type": "prompt", "name": "show_admin", "template": "ok"}],
                "else": [{"type": "prompt", "name": "show_user", "template": "no"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"ok": True}})
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    assert store.get("prime.show_admin.value") == "ok"
    # No named wrapper segment should exist.
    assert store.get("prime.gate", default=None) is None


def test_named_loop_writes_iter_segments() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "repeat",
                "each": {"in": "input.items", "as": "item"},
                "body": [{"type": "prompt", "name": "render", "template": "{{item}}"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": ["a", "b"]}})
    DynamicRuntime(
        root, adapter=EchoAdapter(), model="unit-test", dry_run=True
    ).execute(store=store)

    assert store.get("prime.repeat.iter_0.render.meta") is not None
    assert store.get("prime.repeat.iter_1.render.meta") is not None


def test_transparent_loop_does_not_create_wrapper_segment() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "each": {"in": "input.items", "as": "item"},
                "body": [{"type": "prompt", "name": "render", "template": "{{item}}"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": ["a", "b"]}})
    DynamicRuntime(
        root, adapter=EchoAdapter(), model="unit-test", dry_run=True
    ).execute(store=store)

    assert store.get("prime.render.meta") is not None
    # Transparent loop has no named wrapper.
    assert store.get("prime.repeat", default=None) is None
