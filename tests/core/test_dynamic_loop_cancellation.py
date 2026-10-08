"""#385 review P1: a signal landing mid-dispatch, while a tree-flow
`dynamic`/parallel `loop` is still submitting branches to its
`ThreadPoolExecutor`, must still be reported as a cancellation — not as
`UnboundLocalError`.

Both used to build their branch-future dict with a comprehension inside
the dispatch `try:` block; a `KeyboardInterrupt`/`RunCancelledBySignal`
raised mid-comprehension (by a real signal, or here by a monkeypatched
`submit_with_context`) left that name unbound, so the `except` clause's
own `futures.keys()`/`future_to_idx.keys()` call raised `UnboundLocalError`
— an ordinary `Exception`, not a `BaseException` — instead of the
cancellation ever reaching the caller. `is_cancellation = not
isinstance(exc, Exception)` then misreported it as a plain failure with
the wrong message and outcome. Both now build the dict empty before the
`try` and fill it with a plain loop, so a branch already submitted before
the signal lands is still present (and still waited for below) rather
than silently dropped.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core import dynamic as dynamic_module
from circuitry.core import loop as loop_module
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class EchoAdapter:
    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=prompt, raw={"model": model})


def _raise_keyboard_interrupt_on_second_call(
    monkeypatch: pytest.MonkeyPatch, module: Any
) -> list[int]:
    """Lets the first `submit_with_context` call through for real, then
    raises `KeyboardInterrupt` on the second -- simulating a signal
    landing after one branch has already been submitted but before the
    dict comprehension/loop building the future map has finished."""
    calls: list[int] = []
    real_submit = module.submit_with_context

    def fake_submit(executor: Any, fn: Any, *args: Any, **kwargs: Any) -> Any:
        calls.append(1)
        if len(calls) == 2:
            raise KeyboardInterrupt
        return real_submit(executor, fn, *args, **kwargs)

    monkeypatch.setattr(module, "submit_with_context", fake_submit)
    return calls


def test_dynamic_tree_signal_during_submission_is_a_cancellation_not_unboundlocalerror(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls = _raise_keyboard_interrupt_on_second_call(monkeypatch, dynamic_module)

    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "d1",
                "flow": "tree",
                "effects": [
                    {"type": "prompt", "name": f"b{i}", "template": f"b{i}"}
                    for i in range(3)
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})

    with pytest.raises(KeyboardInterrupt):
        DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
            store=store
        )

    assert len(calls) == 2, "the second submit_with_context call must be the one that raises"


def test_loop_tree_signal_during_submission_is_a_cancellation_not_unboundlocalerror(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls = _raise_keyboard_interrupt_on_second_call(monkeypatch, loop_module)

    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "lp",
                "flow": "tree",
                "each": {"in": "input.items", "as": "item"},
                "body": [{"type": "prompt", "name": "step", "template": "{{item}}"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": ["a", "b", "c"]}})

    with pytest.raises(KeyboardInterrupt):
        DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
            store=store
        )

    assert len(calls) == 2, "the second submit_with_context call must be the one that raises"
