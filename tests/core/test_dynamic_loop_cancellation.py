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

import signal
import threading
import time
from dataclasses import dataclass
from typing import Any

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.cli.interrupts import sigterm_as_interrupt
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


@pytest.mark.skipif(
    not hasattr(signal, "pthread_kill"), reason="signal.pthread_kill is POSIX-only"
)
def test_stop_on_error_tree_waits_for_a_running_sibling_before_a_signal_resets_the_token() -> None:
    """#385 review P1 (F1): ``stop_on_error`` breaks ``_await_tree_branches``
    after the *first* failure while a sibling can still be running --
    `dynamic.py`'s own ``else:`` clause then did a bare, unprotected
    ``executor.shutdown(wait=True)``, so a signal landing on the main
    thread there raised `KeyboardInterrupt` straight out of `execute()`
    without ever waiting for that sibling. `cli.interrupts.
    sigterm_as_interrupt`'s own `finally:` then reset the cancellation
    token right away, well before the still-running sibling's blocking
    call returned -- so when it finally did, its chain's next
    `get_token().check()` found nothing set and ran its next step anyway,
    after the run had already been reported cancelled. Fixed by waiting
    for every future with `wait_for_cancelled_branches` *inside* the `try`,
    right after `_await_tree_branches` returns, so a signal there is
    caught by the matching `except BaseException:` instead -- whose own
    `wait_for_cancelled_branches` keeps the token set until the sibling
    actually finishes.
    """
    ran: list[str] = []
    block_started = threading.Event()
    block_release = threading.Event()

    @dataclass
    class BranchAdapter:
        name: str = "branch"

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            if prompt == "fail-now":
                # A real delay, not an instant raise -- the sibling branch
                # must actually have started (and set `block_started`)
                # before this one fails and `_await_tree_branches` breaks,
                # same reasoning as `test_dynamic_fields.py`'s own
                # `SlowFailAdapter`: a correct implementation must not
                # depend on timing to get this right, but the *test* still
                # needs the sibling genuinely running, not merely queued.
                time.sleep(0.2)
                raise RuntimeError("boom")
            if prompt == "block":
                block_started.set()
                assert block_release.wait(timeout=5.0), "never released"
                ran.append("block")
                return GenerateResult(text=prompt, raw={})
            if prompt == "after":
                ran.append("after")
                return GenerateResult(text=prompt, raw={})
            raise AssertionError(f"unexpected prompt {prompt!r}")

    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "d1",
                "flow": "tree",
                "stop_on_error": True,
                "effects": [
                    {"type": "prompt", "name": "a", "template": "fail-now"},
                    {
                        "type": "dynamic",
                        "name": "chain_b",
                        "flow": "chain",
                        "effects": [
                            {"type": "prompt", "name": "block", "template": "block"},
                            {"type": "prompt", "name": "after", "template": "after"},
                        ],
                    },
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    main_tid = threading.main_thread().ident
    assert main_tid is not None

    def deliver_then_release() -> None:
        assert block_started.wait(timeout=5.0), "sibling branch never started"
        # Past branch A's own 0.2s sleep-then-fail, so the main thread has
        # already left ``_await_tree_branches`` (broken early by
        # ``stop_on_error``) and is now blocked waiting on this still-
        # running sibling -- exactly the point the bug was in, not the
        # ordinary wait for either branch to complete that the pre-
        # existing ``except`` path already covered correctly.
        time.sleep(0.3)
        signal.pthread_kill(main_tid, signal.SIGINT)
        time.sleep(0.5)
        block_release.set()

    with sigterm_as_interrupt():
        threading.Thread(target=deliver_then_release, daemon=True).start()
        with pytest.raises(KeyboardInterrupt):
            DynamicRuntime(root, adapter=BranchAdapter(), model="unit-test").execute(
                store=store
            )

    assert "block" in ran, "the running sibling must finish before cancellation re-raises"
    assert "after" not in ran, "no step may start on a sibling after cancellation"


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
