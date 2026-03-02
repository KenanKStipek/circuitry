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


def test_conditional_records_executed_effects_metadata() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "name": "gate",
                "if": {"mode": "cel", "expr": "state.input.ok == True"},
                "then": [{"type": "prompt", "name": "then_step", "template": "yes"}],
                "else": [{"type": "prompt", "name": "else_step", "template": "no"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"ok": True}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    value = store.get("prime.gate.value")
    assert isinstance(value, dict)
    assert value["branch"] == "then"
    assert isinstance(value["effects"], list)
    assert len(value["effects"]) == 1
    assert value["effects"][0]["name"] == "then_step"
    assert value["effects"][0]["type"] == "PromptDefinition"


def test_loop_records_effects_by_iteration_with_body_summary() -> None:
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
    store = Store({"input": {"items": ["a", "b", "c"]}})

    DynamicRuntime(
        root,
        adapter=EchoAdapter(),
        model="unit-test",
        dry_run=True,
    ).execute(store=store)

    loop_value = store.get("prime.repeat.value")
    assert isinstance(loop_value, dict)
    assert loop_value["iterations"] == 3
    assert loop_value["termination"]["reason"] == "collection_exhausted"

    effects_by_iteration = loop_value["effects_by_iteration"]
    assert isinstance(effects_by_iteration, list)
    assert len(effects_by_iteration) == 3
    for item in effects_by_iteration:
        assert item["count"] == 1
        assert item["executed_effects"][0]["name"] == "render"
        assert item["executed_effects"][0]["type"] == "PromptDefinition"


def test_loop_collect_aggregates_body_effect_values() -> None:
    """collect: <body_effect> aggregates each iteration's .value into collected.value."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "process",
                "collect": "render",
                "each": {"in": "input.items", "as": "item"},
                "body": [{"type": "prompt", "name": "render", "template": "{{item}}"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": ["x", "y", "z"]}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    collected = store.get("prime.process.collected.value")
    assert isinstance(collected, list)
    assert len(collected) == 3
    # EchoAdapter echoes the rendered prompt back as the value
    assert collected[0] == "x"
    assert collected[1] == "y"
    assert collected[2] == "z"


def test_loop_collect_absent_when_not_specified() -> None:
    """Without collect:, prime.<loop>.collected is not written."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "process",
                "each": {"in": "input.items", "as": "item"},
                "body": [{"type": "prompt", "name": "render", "template": "{{item}}"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": ["a", "b"]}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    assert store.get("prime.process.collected") is None


def test_parallel_loop_all_iterations_run() -> None:
    """flow: tree runs all iterations and produces results in original order."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "process",
                "flow": "tree",
                "collect": "render",
                "each": {"in": "input.items", "as": "item"},
                "body": [{"type": "prompt", "name": "render", "template": "{{item}}"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": ["alpha", "beta", "gamma"]}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    # All three iterations should have run
    assert store.get("prime.process.iter_0.render.value") is not None
    assert store.get("prime.process.iter_1.render.value") is not None
    assert store.get("prime.process.iter_2.render.value") is not None

    # collect preserves original order
    collected = store.get("prime.process.collected.value")
    assert isinstance(collected, list)
    assert len(collected) == 3
    # EchoAdapter echoes the prompt, which is the rendered template = item value
    assert collected[0] is not None
    assert collected[1] is not None
    assert collected[2] is not None


def test_parallel_loop_order_preserved() -> None:
    """flow: tree collected results are in the original collection order (not completion order)."""
    import threading
    import time

    call_order: list[str] = []
    lock = threading.Lock()

    @dataclass(frozen=True)
    class SlowEchoAdapter:
        """Returns items in reverse order by sleeping longer for earlier items."""
        name: str = "slow_echo"

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            # Sleep proportional to position so later items finish first
            item = prompt.strip()
            delay = 0.05 if item == "first" else 0.01
            time.sleep(delay)
            with lock:
                call_order.append(item)
            return GenerateResult(text=item, raw={})

    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "process",
                "flow": "tree",
                "collect": "render",
                "each": {"in": "input.items", "as": "item"},
                "body": [{"type": "prompt", "name": "render", "template": "{{item}}"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": ["first", "second", "third"]}})

    DynamicRuntime(root, adapter=SlowEchoAdapter(), model="unit-test").execute(store=store)

    # Results must be in original order regardless of completion order
    collected = store.get("prime.process.collected.value")
    assert isinstance(collected, list)
    assert len(collected) == 3
    assert collected[0] == "first"
    assert collected[1] == "second"
    assert collected[2] == "third"


def test_loop_body_context_sharing_between_effects() -> None:
    """Body effect 2 can reference body effect 1's output via {{effect_name.value}}."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "process",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "step_one", "template": "result_{{item}}"},
                    {
                        "type": "prompt",
                        "name": "step_two",
                        "template": "got:{{step_one.value}}",
                    },
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": ["a", "b"]}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    # step_two should have seen step_one's value via body context sharing
    assert store.get("prime.process.iter_0.step_two.value") == "got:result_a"
    assert store.get("prime.process.iter_1.step_two.value") == "got:result_b"


def test_parallel_loop_max_concurrency_respected() -> None:
    """max_concurrency limits the number of simultaneously running workers."""
    import threading

    active_count: list[int] = []
    peak_active = [0]
    lock = threading.Lock()
    sem_lock = threading.Lock()
    active = [0]

    @dataclass(frozen=True)
    class CountingAdapter:
        name: str = "counting"

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            import time
            with sem_lock:
                active[0] += 1
                if active[0] > peak_active[0]:
                    peak_active[0] = active[0]
            time.sleep(0.02)
            with sem_lock:
                active[0] -= 1
            return GenerateResult(text=prompt.strip(), raw={})

    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "process",
                "flow": "tree",
                "max_concurrency": 2,
                "collect": "render",
                "each": {"in": "input.items", "as": "item"},
                "body": [{"type": "prompt", "name": "render", "template": "{{item}}"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": ["a", "b", "c", "d", "e", "f"]}})

    DynamicRuntime(root, adapter=CountingAdapter(), model="unit-test").execute(store=store)

    # With max_concurrency=2, at most 2 workers should run simultaneously
    assert peak_active[0] <= 2

    # All 6 items should still be processed
    collected = store.get("prime.process.collected.value")
    assert isinstance(collected, list)
    assert len(collected) == 6
