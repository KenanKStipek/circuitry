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
from concurrent.futures import Future, ThreadPoolExecutor
from dataclasses import dataclass
from typing import Any

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.cli.interrupts import sigterm_as_interrupt
from circuitry.core import cancellation
from circuitry.core import dynamic as dynamic_module
from circuitry.core import loop as loop_module
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@pytest.fixture(autouse=True)
def _restore_process_signal_state():
    """Same fixture as `tests.cli.test_interrupts`: several tests here
    drive `sigterm_as_interrupt` with a real SIGINT -- a cancelled run
    leaves SIGHUP `SIG_IGN` on its way out by design and nothing put it
    back, so without this, a later `cof run`
    subprocess test (`tests/cli/test_run_sighup.py`) could find SIGHUP
    already ignored and skip installing its own handler. Undoes both the
    process-wide signal handlers and the module-level `CancellationToken`
    singleton after every test here, regardless of what the test itself
    left behind.
    """
    sigs = [signal.SIGINT, signal.SIGTERM]
    if hasattr(signal, "SIGHUP"):
        sigs.append(signal.SIGHUP)
    previous = {sig: signal.getsignal(sig) for sig in sigs}
    yield
    for sig, handler in previous.items():
        signal.signal(sig, handler)
    cancellation.get_token().reset()


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


def _submit_then_self_sigint_once(
    monkeypatch: pytest.MonkeyPatch, started: threading.Event
) -> threading.Event:
    """Patches `ThreadPoolExecutor.submit` itself (not `submit_with_
    context`) to call the real `submit`, then deliver a real SIGINT to
    this very process, right before returning -- the installed
    `sigterm_as_interrupt` handler then runs synchronously on the main
    thread *inside* `submit`, exactly where the 60-run stress batch's
    own failure put it (#555 follow-up). Only the first call fires it:
    a second signal inside the same dispatch loop would take the
    handler's own immediate `os._exit` path instead of the cancellation
    this test means to drive.

    Waits for *started* before delivering the signal, rather than
    firing it unconditionally right after `submit()` returns: the
    branch's own worker thread must already have
    passed its own pre-step1 `get_token().check()` (`loop.py`/
    `dynamic.py`) and be running step1 by the time the signal lands, or
    this test would pass even against the bug it means to catch --
    step1 never running at all (because the token happened to be set
    before the worker's own check) makes step2 never run for a reason
    that has nothing to do with whether its future was lost. Nothing
    before this fix guaranteed that ordering; it depended on whichever
    thread the scheduler happened to run first.
    """
    real_submit = ThreadPoolExecutor.submit
    signalled = threading.Event()

    def _patched(self: ThreadPoolExecutor, fn: Any, *args: Any, **kwargs: Any) -> Any:
        future = real_submit(self, fn, *args, **kwargs)
        if not signalled.is_set():
            signalled.set()
            assert started.wait(timeout=5.0), "branch's step1 never started"
            # `pthread_kill` targeting the main thread's own tid, not
            # `os.kill` on the whole process: POSIX is free to deliver
            # a process-directed signal to *any* thread that doesn't
            # have it blocked -- if
            # the kernel picked a different one, the handler could run
            # after this dispatch region has already exited, and this
            # test would then also pass against the pre-fix code it
            # means to catch. Targeting the main thread directly makes
            # the handler run before this function returns, every time.
            main_tid = threading.main_thread().ident
            assert main_tid is not None
            signal.pthread_kill(main_tid, signal.SIGINT)
        return future

    monkeypatch.setattr(ThreadPoolExecutor, "submit", _patched)
    return signalled


@pytest.mark.skipif(
    not hasattr(signal, "pthread_kill"), reason="signal.pthread_kill is POSIX-only"
)
def test_loop_tree_future_not_lost_when_signal_lands_inside_executor_submit(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Reproduces the 60-run stress batch's own failure (#555
    follow-up): a signal landing on the main thread *while
    `ThreadPoolExecutor.submit` itself is still running*, not merely
    between submits (the test above). Before this fix, the future
    `submit` was about to return was discarded the moment
    `submit_with_context`'s own region raised, before this loop's own
    `future_to_idx[future] = idx` line ever ran -- so nothing ever
    waited for that branch; `sigterm_as_interrupt` reset the token on
    its way out while the branch was still mid-step1, and step2 ran once
    its own (now-cleared) `get_token().check()` found nothing set.
    """
    ran: list[str] = []
    step1_started = threading.Event()
    _submit_then_self_sigint_once(monkeypatch, step1_started)

    @dataclass
    class BranchAdapter:
        name: str = "branch"

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            if prompt == "step1":
                step1_started.set()
                time.sleep(0.3)
                return GenerateResult(text=prompt, raw={})
            if prompt == "step2":
                ran.append("step2")
                return GenerateResult(text=prompt, raw={})
            raise AssertionError(f"unexpected prompt {prompt!r}")

    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "lp",
                "flow": "tree",
                "each": {"in": "input.items", "as": "x"},
                "body": [
                    {"type": "prompt", "name": "step1", "template": "step1"},
                    {"type": "prompt", "name": "step2", "template": "step2"},
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": [1]}})

    with sigterm_as_interrupt():
        with pytest.raises(KeyboardInterrupt):
            DynamicRuntime(root, adapter=BranchAdapter(), model="unit-test").execute(
                store=store
            )

    # Lets the branch's own worker thread, still running in the
    # background after the main thread's exception already unwound
    # (`executor.shutdown(wait=False, ...)` never interrupts work
    # already running), reach step2 if it's going to -- well past
    # step1's own 0.3s sleep.
    time.sleep(1.0)
    assert "step2" not in ran, (
        "step2 ran even though the signal landed inside "
        "ThreadPoolExecutor.submit -- the future submit() was about to "
        "return was lost"
    )


@pytest.mark.skipif(
    not hasattr(signal, "pthread_kill"), reason="signal.pthread_kill is POSIX-only"
)
def test_dynamic_tree_future_not_lost_when_signal_lands_inside_executor_submit(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The same reproduction as the `loop` test above (#555
    follow-up), against `dynamic.py`'s own tree-flow dispatch instead.
    """
    ran: list[str] = []
    step1_started = threading.Event()
    _submit_then_self_sigint_once(monkeypatch, step1_started)

    @dataclass
    class BranchAdapter:
        name: str = "branch"

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            if prompt == "step1":
                step1_started.set()
                time.sleep(0.3)
                return GenerateResult(text=prompt, raw={})
            if prompt == "step2":
                ran.append("step2")
                return GenerateResult(text=prompt, raw={})
            raise AssertionError(f"unexpected prompt {prompt!r}")

    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "d1",
                "flow": "tree",
                "effects": [
                    {
                        "type": "dynamic",
                        "name": "b0",
                        "flow": "chain",
                        "effects": [
                            {"type": "prompt", "name": "step1", "template": "step1"},
                            {"type": "prompt", "name": "step2", "template": "step2"},
                        ],
                    }
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})

    with sigterm_as_interrupt():
        with pytest.raises(KeyboardInterrupt):
            DynamicRuntime(root, adapter=BranchAdapter(), model="unit-test").execute(
                store=store
            )

    time.sleep(1.0)
    assert "step2" not in ran, (
        "step2 ran even though the signal landed inside "
        "ThreadPoolExecutor.submit -- the future submit() was about to "
        "return was lost"
    )


def _raise_runtime_error_on_second_call(
    monkeypatch: pytest.MonkeyPatch, module: Any
) -> list[int]:
    """Like `_raise_keyboard_interrupt_on_second_call` above, but raises a
    plain `RuntimeError` instead -- simulating the cancel path being
    entered by something that is *not* a signal (`executor.submit`
    itself, or an observer callback) rather than a real Ctrl-C: a
    *first* signal then lands while the main thread is inside the
    cancel path's own wait, not during dispatch.
    """
    calls: list[int] = []
    real_submit = module.submit_with_context

    def fake_submit(executor: Any, fn: Any, *args: Any, **kwargs: Any) -> Any:
        calls.append(1)
        if len(calls) == 2:
            raise RuntimeError("boom: simulated non-signal failure mid-dispatch")
        return real_submit(executor, fn, *args, **kwargs)

    monkeypatch.setattr(module, "submit_with_context", fake_submit)
    return calls


@pytest.mark.skipif(
    not hasattr(signal, "pthread_kill"), reason="signal.pthread_kill is POSIX-only"
)
def test_dynamic_tree_a_first_signal_during_the_cancel_paths_wait_does_not_cut_it_short(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The cancel path's own wait -- `executor.shutdown(wait=False,
    cancel_futures=True)` followed by `wait_for_cancelled_branches` --
    must not be cut short by a *first* signal landing during it, or a
    still-running branch the kill can't reach passes its next
    `get_token().check()` and starts its next step after
    `sigterm_as_interrupt`'s own `finally:` has already reset the token
    on the way out. Reproduced step by step: the cancel path is entered
    by a non-signal exception (the *second*
    `submit_with_context` call raising `RuntimeError`, simulating
    `executor.submit`/an observer callback failing), branch 0's own
    step1 blocks on an `Event`, a *first* real SIGINT is delivered via
    `signal.pthread_kill` once the main thread is in the cancel path's
    wait, and only then is step1 released.

    Before the fix (shutdown and the wait in two separate regions, or
    the wait in none at all), the signal would raise immediately inside
    the wait, unwind out of `sigterm_as_interrupt` and reset the token
    well before step1 is released below -- so step1's eventual
    completion would let the branch straight through to step2. The fix
    keeps both calls in one `deferred_signal_region`: the signal's own
    exception stays parked until `wait_for_cancelled_branches` has
    actually finished waiting for branch 0, by which point `token.
    check()` (already set by the signal's own `request()` call,
    independent of the deferral) stops it from ever reaching step2.
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
            if prompt == "step1":
                block_started.set()
                assert block_release.wait(timeout=5.0), "never released"
                # Recorded only *after* the release: proves step1 was
                # still blocked, waited out, and only then completed --
                # not that it happened to run and finish before the
                # signal ever did anything, which an assertion checking
                # only for step2 below would not have told apart.
                ran.append("step1")
                return GenerateResult(text=prompt, raw={})
            if prompt == "step2":
                ran.append("step2")
                return GenerateResult(text=prompt, raw={})
            raise AssertionError(f"unexpected prompt {prompt!r}")

    _raise_runtime_error_on_second_call(monkeypatch, dynamic_module)

    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "d1",
                "flow": "tree",
                "effects": [
                    {
                        "type": "dynamic",
                        "name": "b0",
                        "flow": "chain",
                        "effects": [
                            {"type": "prompt", "name": "step1", "template": "step1"},
                            {"type": "prompt", "name": "step2", "template": "step2"},
                        ],
                    },
                    {"type": "prompt", "name": "b1", "template": "b1"},
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    main_tid = threading.main_thread().ident
    assert main_tid is not None

    def deliver_then_release() -> None:
        assert block_started.wait(timeout=5.0), "branch 0's step1 never started"
        # Past dispatch (the RuntimeError on the second submit_with_
        # context call) and into the cancel path's own wait by now --
        # everything in between is a handful of main-thread bytecode
        # instructions, not a blocking call.
        time.sleep(0.3)
        signal.pthread_kill(main_tid, signal.SIGINT)
        # Long enough that the pre-fix code -- where the wait wasn't in
        # the same region as the shutdown call ahead of it -- would
        # already have let the signal cut it short, unwound all the way
        # out of sigterm_as_interrupt, and reset the token, well before
        # step1 is released below.
        time.sleep(0.5)
        block_release.set()

    with sigterm_as_interrupt():
        threading.Thread(target=deliver_then_release, daemon=True).start()
        with pytest.raises(KeyboardInterrupt):
            DynamicRuntime(root, adapter=BranchAdapter(), model="unit-test").execute(
                store=store
            )

    # Proves the wait actually outlasted the release, instead of the
    # signal cutting it short and letting execute() return well before
    # step1 ever got to run at all -- on the pre-fix code, the `with`
    # block above exits the instant the signal lands, long before
    # `deliver_then_release`'s own `block_release.set()` runs 0.5s
    # later, so step1 is still blocked right here and this assertion
    # would fail.
    assert "step1" in ran, (
        "the with sigterm_as_interrupt() block exited before step1 ever "
        "finished -- the cancel path's own wait was cut short instead of "
        "actually waiting for the still-running branch"
    )

    # Lets the branch's own worker thread, now past step1, reach step2
    # if it's going to -- same margin the lost-future tests use.
    time.sleep(1.0)
    assert "step2" not in ran, (
        "step2 ran after a first signal landed during the cancel path's "
        "own wait -- the wait was cut short instead of waiting for the "
        "still-running branch, letting token.reset() clear the flag "
        "before it finished"
    )


@pytest.mark.skipif(
    not hasattr(signal, "pthread_kill"), reason="signal.pthread_kill is POSIX-only"
)
def test_loop_tree_a_first_signal_during_the_cancel_paths_wait_does_not_cut_it_short(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The same reproduction as the `dynamic` test above, against
    `loop.py`'s own tree-flow dispatch instead.
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
            if prompt == "step1":
                block_started.set()
                assert block_release.wait(timeout=5.0), "never released"
                # Recorded only *after* the release -- see the dynamic
                # test above.
                ran.append("step1")
                return GenerateResult(text=prompt, raw={})
            if prompt == "step2":
                ran.append("step2")
                return GenerateResult(text=prompt, raw={})
            raise AssertionError(f"unexpected prompt {prompt!r}")

    _raise_runtime_error_on_second_call(monkeypatch, loop_module)

    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "lp",
                "flow": "tree",
                "each": {"in": "input.items", "as": "x"},
                "body": [
                    {"type": "prompt", "name": "step1", "template": "step1"},
                    {"type": "prompt", "name": "step2", "template": "step2"},
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": [1, 2]}})
    main_tid = threading.main_thread().ident
    assert main_tid is not None

    def deliver_then_release() -> None:
        assert block_started.wait(timeout=5.0), "pass 0's step1 never started"
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

    # See the dynamic test above: proves the wait outlasted the release.
    assert "step1" in ran, (
        "the with sigterm_as_interrupt() block exited before step1 ever "
        "finished -- the cancel path's own wait was cut short instead of "
        "actually waiting for the still-running pass"
    )

    time.sleep(1.0)
    assert "step2" not in ran, (
        "step2 ran after a first signal landed during the cancel path's "
        "own wait -- the wait was cut short instead of waiting for the "
        "still-running pass, letting token.reset() clear the flag before "
        "it finished"
    )


def test_dynamic_tree_submit_dispatch_runs_inside_a_deferred_signal_region(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Pins dynamic.py's own dispatch-loop region: if the ``with
    deferred_signal_region():`` wrapping the ``submit_with_context``
    call (and recording its future) were ever removed, this would fail
    directly, at the call site -- not only via ``submit_with_context``'s
    own internal assert that the caller already holds one.
    """
    seen_deferring: list[bool] = []
    real_submit = dynamic_module.submit_with_context

    def _spy(executor: Any, fn: Any, *args: Any, **kwargs: Any) -> Any:
        seen_deferring.append(cancellation.is_deferring_first_signal())
        return real_submit(executor, fn, *args, **kwargs)

    monkeypatch.setattr(dynamic_module, "submit_with_context", _spy)

    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "d1",
                "flow": "tree",
                "effects": [
                    {"type": "prompt", "name": f"b{i}", "template": f"b{i}"}
                    for i in range(2)
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    assert seen_deferring, "submit_with_context was never called"
    assert all(seen_deferring), (
        "dynamic.py's tree-flow dispatch must call submit_with_context "
        "only from inside its own deferred_signal_region"
    )


def test_loop_tree_submit_dispatch_runs_inside_a_deferred_signal_region(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Pins loop.py's own dispatch-loop region -- see the identical
    `dynamic` test above.
    """
    seen_deferring: list[bool] = []
    real_submit = loop_module.submit_with_context

    def _spy(executor: Any, fn: Any, *args: Any, **kwargs: Any) -> Any:
        seen_deferring.append(cancellation.is_deferring_first_signal())
        return real_submit(executor, fn, *args, **kwargs)

    monkeypatch.setattr(loop_module, "submit_with_context", _spy)

    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "lp",
                "flow": "tree",
                "each": {"in": "input.items", "as": "x"},
                "body": [{"type": "prompt", "name": "step", "template": "{{x}}"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": [1, 2]}})
    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    assert seen_deferring, "submit_with_context was never called"
    assert all(seen_deferring), (
        "loop.py's tree-flow dispatch must call submit_with_context only "
        "from inside its own deferred_signal_region"
    )


def test_dynamic_tree_cancel_paths_shutdown_and_wait_share_one_deferred_signal_region(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Pins the fix keeping the cancel path's wait from being cut
    short: if ``executor.shutdown(wait=False, cancel_futures=True)`` and
    ``wait_for_cancelled_branches`` were ever split back into two
    separate ``deferred_signal_region`` scopes (or ``wait_for_cancelled_
    branches`` taken out of one entirely), the region would have
    already exited -- and ``is_deferring_first_signal`` would read
    ``False`` -- by the time ``wait_for_cancelled_branches`` is called,
    below.
    """
    seen: list[tuple[str, bool]] = []
    real_shutdown = ThreadPoolExecutor.shutdown
    real_wait = dynamic_module.wait_for_cancelled_branches

    def _spy_shutdown(self: ThreadPoolExecutor, *args: Any, **kwargs: Any) -> Any:
        if kwargs.get("cancel_futures"):
            seen.append(("shutdown", cancellation.is_deferring_first_signal()))
        return real_shutdown(self, *args, **kwargs)

    def _spy_wait(futures: Any) -> Any:
        seen.append(("wait", cancellation.is_deferring_first_signal()))
        return real_wait(futures)

    monkeypatch.setattr(ThreadPoolExecutor, "shutdown", _spy_shutdown)
    monkeypatch.setattr(dynamic_module, "wait_for_cancelled_branches", _spy_wait)
    _raise_runtime_error_on_second_call(monkeypatch, dynamic_module)

    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "d1",
                "flow": "tree",
                "effects": [
                    {"type": "prompt", "name": f"b{i}", "template": f"b{i}"}
                    for i in range(2)
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})

    with pytest.raises(RuntimeError):
        DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
            store=store
        )

    assert [name for name, _ in seen] == ["shutdown", "wait"]
    assert all(deferring for _, deferring in seen), (
        "dynamic.py's cancel path must call executor.shutdown(cancel_futures="
        "True) and wait_for_cancelled_branches from inside the same "
        "deferred_signal_region"
    )


def test_loop_tree_cancel_paths_shutdown_and_wait_share_one_deferred_signal_region(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Pins loop.py's own identical fix -- see the `dynamic` test above.
    """
    seen: list[tuple[str, bool]] = []
    real_shutdown = ThreadPoolExecutor.shutdown
    real_wait = loop_module.wait_for_cancelled_branches

    def _spy_shutdown(self: ThreadPoolExecutor, *args: Any, **kwargs: Any) -> Any:
        if kwargs.get("cancel_futures"):
            seen.append(("shutdown", cancellation.is_deferring_first_signal()))
        return real_shutdown(self, *args, **kwargs)

    def _spy_wait(futures: Any) -> Any:
        seen.append(("wait", cancellation.is_deferring_first_signal()))
        return real_wait(futures)

    monkeypatch.setattr(ThreadPoolExecutor, "shutdown", _spy_shutdown)
    monkeypatch.setattr(loop_module, "wait_for_cancelled_branches", _spy_wait)
    _raise_runtime_error_on_second_call(monkeypatch, loop_module)

    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "lp",
                "flow": "tree",
                "each": {"in": "input.items", "as": "x"},
                "body": [{"type": "prompt", "name": "step", "template": "{{x}}"}],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": [1, 2]}})

    with pytest.raises(RuntimeError):
        DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
            store=store
        )

    assert [name for name, _ in seen] == ["shutdown", "wait"]
    assert all(deferring for _, deferring in seen), (
        "loop.py's cancel path must call executor.shutdown(cancel_futures="
        "True) and wait_for_cancelled_branches from inside the same "
        "deferred_signal_region"
    )


def test_dynamic_tree_stop_on_error_pending_cancel_runs_inside_a_deferred_signal_region(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Pins `_await_tree_branches`'s own ``pending.cancel()`` region: a
    ``stop_on_error`` tree cancels every sibling that hasn't started yet
    the moment the first one fails -- ``max_concurrency: 1`` keeps the
    second branch queued, not yet dequeued, when that happens.
    """
    seen_deferring: list[bool] = []
    real_cancel = Future.cancel

    def _spy_cancel(self: Future[Any]) -> Any:
        seen_deferring.append(cancellation.is_deferring_first_signal())
        return real_cancel(self)

    monkeypatch.setattr(Future, "cancel", _spy_cancel)

    @dataclass
    class BranchAdapter:
        name: str = "branch"

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            if prompt == "fail-now":
                raise RuntimeError("boom")
            return GenerateResult(text=prompt, raw={})

    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "d1",
                "flow": "tree",
                "stop_on_error": True,
                "max_concurrency": 1,
                "effects": [
                    {"type": "prompt", "name": "a", "template": "fail-now"},
                    {"type": "prompt", "name": "b", "template": "queued"},
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})

    # The outer root container is itself a plain chain (`root_name=
    # "prime""`), so it re-wraps d1's own `TreeExecutionError` as a
    # `RuntimeError` of its own -- see `_execute_chain`'s `except
    # Exception as e: raise RuntimeError(...) from e`.
    with pytest.raises(RuntimeError):
        DynamicRuntime(root, adapter=BranchAdapter(), model="unit-test").execute(
            store=store
        )

    assert seen_deferring, "Future.cancel() was never called"
    assert all(seen_deferring), (
        "dynamic.py's stop_on_error cancel of a not-yet-started sibling "
        "must run inside its own deferred_signal_region"
    )
