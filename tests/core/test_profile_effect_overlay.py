from __future__ import annotations

from dataclasses import dataclass, field

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import apply_effect_overrides, compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


def _nested_orch() -> dict:
    return {
        "effects": [
            {"type": "prompt", "name": "summarize", "template": "s"},
            {
                "type": "dynamic",
                "name": "sub",
                "effects": [
                    {"type": "prompt", "name": "deep_analysis", "template": "d"},
                ],
            },
            {"type": "tool", "name": "fetch", "provider": "http", "params": {}},
        ]
    }


def test_apply_effect_overrides_targets_top_level_and_nested_paths() -> None:
    root = compile_orchestration(orch=_nested_orch(), root_name="prime")

    new_root, matched = apply_effect_overrides(
        root,
        {
            "summarize": {"model": "tier-1", "provider": "cyberdiner"},
            "sub.deep_analysis": {"model": "tier-4"},
        },
    )

    assert matched == {"summarize", "sub.deep_analysis"}

    summarize = new_root.effects[0]
    assert summarize.name == "summarize"
    assert summarize.model == "tier-1"
    assert summarize.provider == "cyberdiner"

    nested = new_root.effects[1].effects[0]
    assert nested.name == "deep_analysis"
    assert nested.model == "tier-4"

    # Original tree is untouched (frozen semantics preserved).
    assert root.effects[0].model is None
    assert root.effects[1].effects[0].model is None


def test_apply_effect_overrides_no_overrides_returns_same_root() -> None:
    root = compile_orchestration(orch=_nested_orch(), root_name="prime")
    new_root, matched = apply_effect_overrides(root, {})
    assert new_root is root
    assert matched == set()


def test_apply_effect_overrides_ignores_attrs_the_target_does_not_have() -> None:
    """A 'use' effect has no model/provider fields — overlay is a harmless no-op."""
    orch = {
        "effects": [
            {"type": "use", "name": "child", "path": "does-not-matter.yml"},
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    new_root, matched = apply_effect_overrides(root, {"child": {"model": "x"}})
    assert matched == {"child"}
    assert not hasattr(new_root.effects[0], "model")


@dataclass(frozen=True)
class RecordingAdapter:
    name: str
    calls: list = field(default_factory=list)

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        self.calls.append((self.name, model))
        return GenerateResult(text=f"{model}:{prompt}", raw={})


def _prompt_only_orch() -> dict:
    return {
        "effects": [
            {"type": "prompt", "name": "summarize", "template": "s"},
            {
                "type": "dynamic",
                "name": "sub",
                "effects": [
                    {"type": "prompt", "name": "deep_analysis", "template": "d"},
                ],
            },
        ]
    }


def test_per_effect_overlay_reaches_prompt_runtime() -> None:
    root = compile_orchestration(orch=_prompt_only_orch(), root_name="prime")
    overridden, _ = apply_effect_overrides(
        root,
        {
            "summarize": {"model": "tier-1"},
            "sub.deep_analysis": {"model": "tier-4"},
        },
    )

    adapter = RecordingAdapter(name="primary")
    store = Store({})
    DynamicRuntime(overridden, adapter=adapter, model="run-default").execute(store=store)

    assert store.get("prime.summarize.meta.model") == "tier-1"
    assert store.get("prime.sub.deep_analysis.meta.model") == "tier-4"
    assert ("primary", "tier-1") in adapter.calls
    assert ("primary", "tier-4") in adapter.calls
