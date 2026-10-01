from __future__ import annotations

import textwrap
import time
from pathlib import Path

import pytest

from circuitry.mcp.runs import RunManager, RunStatus

# Test orchestrations explicitly pin `model: claude-sonnet-4` so they don't
# depend on the user's local config default (which may be a non-Claude model
# that would be rejected by host_claude's strict gate).


def _write_yml(tmp_path: Path, name: str, body: str) -> Path:
    p = tmp_path / name
    p.write_text(textwrap.dedent(body), encoding="utf-8")
    return p


@pytest.fixture
def mgr() -> RunManager:
    m = RunManager(
        quiesce_max_wait_seconds=2.0,
        cancel_join_timeout=2.0,
        worker_poll_interval=0.05,
    )
    yield m
    # Cancel any runs still alive: their daemon worker thread is fine, but a
    # tree-flow orchestration leaves non-daemon ThreadPoolExecutor workers
    # blocked on response queues, which would hang interpreter shutdown.
    for run_id in list(m._runs):
        run = m._runs[run_id]
        if not run.status.is_terminal:
            try:
                m.cancel_run(run_id)
            except KeyError:
                pass


def _wait_until(predicate, timeout: float = 2.0, interval: float = 0.01) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()


def _single_pending_id(run, exclude: frozenset[str] = frozenset()) -> str:
    # After a submit, pending_prompts can be stably {answered_id} for a beat
    # before the worker pops it (runs.py's finally-block race, same family
    # as issue #191) — exclude lets callers wait past their own prior id.
    def _ready() -> bool:
        return (
            len(run.pending_prompts) == 1
            and next(iter(run.pending_prompts)) not in exclude
        )

    assert _wait_until(_ready), list(run.pending_prompts)
    return next(iter(run.pending_prompts))


# ---------------------------------------------------------------------------
# 1. Simple chain pause then completion
# ---------------------------------------------------------------------------


def test_simple_chain_pauses_then_completes(mgr: RunManager, tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "hello.yml", """
        model: claude-sonnet-4
        adapter: host_claude
        effects:
          - type: prompt
            name: greet
            template: "Say hi"
    """)
    run = mgr.start_run(orchestration_path=p)

    assert _wait_until(lambda: run.status == RunStatus.PAUSED)
    pid = _single_pending_id(run)
    assert run.pending_prompts[pid].prompt == "Say hi"

    mgr.submit_response(run_id=run.run_id, prompt_id=pid, response_text="the answer")

    assert _wait_until(lambda: run.status == RunStatus.COMPLETED)
    assert run.state["prime"]["greet"]["value"] == "the answer"


def test_submit_response_never_returns_with_the_answered_prompt_still_pending(
    mgr: RunManager, tmp_path: Path
) -> None:
    """An artificial scheduling delay between the worker thread retrieving
    the response off its queue and actually popping the prompt from
    `pending_prompts` must not let `submit_response` return while that same
    prompt id is still listed as pending (#237) — the exact client-visible
    staleness the issue describes: "a client re-reading state sees an old
    prompt as current". The delay happens *before* the worker touches
    `run._lock` (exactly where a real scheduling delay would land), not
    while holding it — holding the lock during the delay would merely
    block the reader rather than let it observe a stale pre-pop snapshot.
    """
    import queue
    import time

    p = _write_yml(tmp_path, "hello.yml", """
        model: claude-sonnet-4
        adapter: host_claude
        effects:
          - type: prompt
            name: greet
            template: "Say hi"
    """)
    run = mgr.start_run(orchestration_path=p)
    assert _wait_until(lambda: run.status == RunStatus.PAUSED)
    pid = _single_pending_id(run)

    class _SlowQueue(queue.Queue):
        """Delays *after* the item is retrieved, before the worker thread
        reaches the `with run._lock:` pop — simulating the worker thread
        not yet being scheduled, not lock contention."""

        def get(self, *args, **kwargs):  # type: ignore[no-untyped-def]
            item = super().get(*args, **kwargs)
            time.sleep(0.3)
            return item

    slow_queue: _SlowQueue = _SlowQueue(maxsize=1)
    run.pending_prompts[pid].response_queue = slow_queue

    mgr.submit_response(run_id=run.run_id, prompt_id=pid, response_text="the answer")

    assert pid not in run.pending_prompts


# ---------------------------------------------------------------------------
# 2. Three-prompt chain with substitution between effects
# ---------------------------------------------------------------------------


def test_three_prompt_chain(mgr: RunManager, tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "chain.yml", """
        model: claude-sonnet-4
        adapter: host_claude
        effects:
          - type: prompt
            name: a
            template: "first"
          - type: prompt
            name: b
            template: "saw a={{prime.a.value}}"
          - type: prompt
            name: c
            template: "saw b={{prime.b.value}}"
    """)
    run = mgr.start_run(orchestration_path=p)

    pid_a = _single_pending_id(run)
    assert run.pending_prompts[pid_a].prompt == "first"
    mgr.submit_response(run_id=run.run_id, prompt_id=pid_a, response_text="A-out")

    assert _wait_until(lambda: run.status == RunStatus.PAUSED)
    pid_b = _single_pending_id(run, exclude=frozenset({pid_a}))
    assert run.pending_prompts[pid_b].prompt == "saw a=A-out"
    mgr.submit_response(run_id=run.run_id, prompt_id=pid_b, response_text="B-out")

    pid_c = _single_pending_id(run, exclude=frozenset({pid_b}))
    assert run.pending_prompts[pid_c].prompt == "saw b=B-out"
    mgr.submit_response(run_id=run.run_id, prompt_id=pid_c, response_text="C-out")

    assert _wait_until(lambda: run.status == RunStatus.COMPLETED)
    assert run.state["prime"]["c"]["value"] == "C-out"


# ---------------------------------------------------------------------------
# 3. Sequential loop
# ---------------------------------------------------------------------------


def test_sequential_loop(mgr: RunManager, tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "seq_loop.yml", """
        model: claude-sonnet-4
        adapter: host_claude
        effects:
          - type: loop
            name: greet_each
            each:
              in: input.names
              as: who
            body:
              - type: prompt
                name: greet
                template: "Greet {{who}}"
    """)
    run = mgr.start_run(
        orchestration_path=p, initial_state={"names": ["Ada", "Bob", "Cyd"]},
    )

    responses = []
    prev_pid: str | None = None
    for i in range(3):
        assert _wait_until(lambda: run.status == RunStatus.PAUSED), f"iter {i}: {run.status}"
        exclude = frozenset({prev_pid}) if prev_pid is not None else frozenset()
        pid = _single_pending_id(run, exclude=exclude)
        text = f"hi-{i}"
        responses.append(text)
        mgr.submit_response(run_id=run.run_id, prompt_id=pid, response_text=text)
        prev_pid = pid

    assert _wait_until(lambda: run.status == RunStatus.COMPLETED)
    loop_state = run.state["prime"]["greet_each"]
    for i, text in enumerate(responses):
        assert loop_state[f"iter_{i}"]["greet"]["value"] == text


# ---------------------------------------------------------------------------
# 4. Tree-flow with two parallel branches via parallel loop
# ---------------------------------------------------------------------------


def test_tree_flow_two_branches(mgr: RunManager, tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "tree2.yml", """
        model: claude-sonnet-4
        adapter: host_claude
        effects:
          - type: loop
            name: parallel
            flow: tree
            each:
              in: input.items
              as: it
            body:
              - type: prompt
                name: ask
                template: "Q: {{it}}"
    """)
    run = mgr.start_run(
        orchestration_path=p, initial_state={"items": ["alpha", "beta"]},
    )

    assert _wait_until(lambda: run.status == RunStatus.PAUSED and len(run.pending_prompts) == 2)
    pids = list(run.pending_prompts)
    prompts = {pp.prompt for pp in run.pending_prompts.values()}
    assert prompts == {"Q: alpha", "Q: beta"}
    assert len(set(pids)) == 2

    # Submit in arbitrary order — order must not matter.
    for pid in reversed(pids):
        mgr.submit_response(run_id=run.run_id, prompt_id=pid, response_text=f"R-{pid[:4]}")

    assert _wait_until(lambda: run.status == RunStatus.COMPLETED)
    iter0 = run.state["prime"]["parallel"]["iter_0"]["ask"]["value"]
    iter1 = run.state["prime"]["parallel"]["iter_1"]["ask"]["value"]
    assert {iter0, iter1} == {f"R-{pids[0][:4]}", f"R-{pids[1][:4]}"}


# ---------------------------------------------------------------------------
# 5. Parallel loop with 4 iterations
# ---------------------------------------------------------------------------


def test_parallel_loop_iterations(mgr: RunManager, tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "parloop4.yml", """
        model: claude-sonnet-4
        adapter: host_claude
        effects:
          - type: loop
            name: par
            flow: tree
            each:
              in: input.items
              as: it
            body:
              - type: prompt
                name: ans
                template: "for {{it}}"
    """)
    run = mgr.start_run(
        orchestration_path=p, initial_state={"items": ["w", "x", "y", "z"]},
    )

    assert _wait_until(lambda: len(run.pending_prompts) == 4, timeout=2.0)
    pids = list(run.pending_prompts)
    for pid in pids:
        mgr.submit_response(run_id=run.run_id, prompt_id=pid, response_text=f"resp-{pid[:4]}")

    assert _wait_until(lambda: run.status == RunStatus.COMPLETED)
    for i in range(4):
        v = run.state["prime"]["par"][f"iter_{i}"]["ans"]["value"]
        assert v.startswith("resp-")


# ---------------------------------------------------------------------------
# 6. Nested parallel: chain → tree → chain
# ---------------------------------------------------------------------------


def test_nested_parallel_tree_in_chain(mgr: RunManager, tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "nested.yml", """
        model: claude-sonnet-4
        adapter: host_claude
        effects:
          - type: prompt
            name: pre
            template: "pre"
          - type: loop
            name: middle
            flow: tree
            each:
              in: input.items
              as: it
            body:
              - type: prompt
                name: branch
                template: "{{it}}"
          - type: prompt
            name: post
            template: "post"
    """)
    run = mgr.start_run(
        orchestration_path=p, initial_state={"items": ["a", "b", "c"]},
    )

    # Stage 1: single pre prompt
    pid = _single_pending_id(run)
    mgr.submit_response(run_id=run.run_id, prompt_id=pid, response_text="pre-out")

    # Stage 2: 3 parallel branches
    assert _wait_until(lambda: len(run.pending_prompts) == 3, timeout=2.0)
    branch_pids = list(run.pending_prompts)
    for bp in branch_pids:
        mgr.submit_response(run_id=run.run_id, prompt_id=bp, response_text=f"b-{bp[:4]}")

    # Stage 3: single post prompt
    assert _wait_until(lambda: run.status == RunStatus.PAUSED and len(run.pending_prompts) == 1, timeout=2.0)
    post_pid = _single_pending_id(run, exclude=frozenset(branch_pids))
    assert run.pending_prompts[post_pid].prompt == "post"
    mgr.submit_response(run_id=run.run_id, prompt_id=post_pid, response_text="post-out")

    assert _wait_until(lambda: run.status == RunStatus.COMPLETED)
    assert run.state["prime"]["pre"]["value"] == "pre-out"
    assert run.state["prime"]["post"]["value"] == "post-out"
    for i in range(3):
        assert run.state["prime"]["middle"][f"iter_{i}"]["branch"]["value"].startswith("b-")


# ---------------------------------------------------------------------------
# 7. Cancel while paused (parallel branches)
# ---------------------------------------------------------------------------


def test_cancel_while_paused(mgr: RunManager, tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "cancel.yml", """
        model: claude-sonnet-4
        adapter: host_claude
        effects:
          - type: loop
            name: par
            flow: tree
            each:
              in: input.items
              as: it
            body:
              - type: prompt
                name: ask
                template: "{{it}}"
    """)
    run = mgr.start_run(
        orchestration_path=p, initial_state={"items": ["a", "b"]},
    )
    assert _wait_until(lambda: len(run.pending_prompts) == 2, timeout=2.0)

    pids_before_cancel = list(run.pending_prompts)
    mgr.cancel_run(run.run_id)

    assert run.status == RunStatus.CANCELLED
    assert run.pending_prompts == {}
    assert run.thread is None or not run.thread.is_alive()

    # Subsequent submit_response is a structured error
    with pytest.raises(KeyError):
        mgr.submit_response(
            run_id=run.run_id,
            prompt_id=pids_before_cancel[0],
            response_text="late",
        )


# ---------------------------------------------------------------------------
# 8. Runtime error propagates to FAILED
# ---------------------------------------------------------------------------


def test_runtime_error_propagates_to_failed(mgr: RunManager, tmp_path: Path) -> None:
    # Unknown effect type fails at compile time inside the worker thread.
    p = _write_yml(tmp_path, "boom.yml", """
        model: claude-sonnet-4
        adapter: host_claude
        effects:
          - type: this_effect_type_does_not_exist
            name: oops
    """)
    run = mgr.start_run(orchestration_path=p)

    assert _wait_until(lambda: run.status.is_terminal, timeout=3.0)
    assert run.status == RunStatus.FAILED
    assert run.error
    assert run.pending_prompts == {}


# ---------------------------------------------------------------------------
# 9. Concurrent runs are isolated
# ---------------------------------------------------------------------------


def test_concurrent_runs_isolated(mgr: RunManager, tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "iso.yml", """
        model: claude-sonnet-4
        adapter: host_claude
        effects:
          - type: prompt
            name: x
            template: "for {{input.tag}}"
    """)
    run_a = mgr.start_run(orchestration_path=p, initial_state={"tag": "A"})
    run_b = mgr.start_run(orchestration_path=p, initial_state={"tag": "B"})

    assert _wait_until(lambda: run_a.status == RunStatus.PAUSED)
    assert _wait_until(lambda: run_b.status == RunStatus.PAUSED)
    assert run_a.run_id != run_b.run_id

    pid_a = _single_pending_id(run_a)
    mgr.submit_response(run_id=run_a.run_id, prompt_id=pid_a, response_text="answer-A")

    assert _wait_until(lambda: run_a.status == RunStatus.COMPLETED)
    # Run B still paused, unaffected
    assert run_b.status == RunStatus.PAUSED
    assert len(run_b.pending_prompts) == 1

    pid_b = _single_pending_id(run_b)
    mgr.submit_response(run_id=run_b.run_id, prompt_id=pid_b, response_text="answer-B")
    assert _wait_until(lambda: run_b.status == RunStatus.COMPLETED)

    assert run_a.state["prime"]["x"]["value"] == "answer-A"
    assert run_b.state["prime"]["x"]["value"] == "answer-B"


# ---------------------------------------------------------------------------
# 10. get_state during pause returns a snapshot, doesn't disturb the run
# ---------------------------------------------------------------------------


def test_get_state_during_pause(mgr: RunManager, tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "snap.yml", """
        model: claude-sonnet-4
        adapter: host_claude
        effects:
          - type: prompt
            name: a
            template: "first"
          - type: prompt
            name: b
            template: "second"
    """)
    run = mgr.start_run(orchestration_path=p)
    pid_a = _single_pending_id(run)
    mgr.submit_response(run_id=run.run_id, prompt_id=pid_a, response_text="A-out")

    # _wait_for_quiesce isn't a guarantee the worker has already reached the
    # next prompt (a stable-but-empty pending_prompts set right after the
    # thread pops 'a' looks the same as settled) — poll rather than assert.
    assert _wait_until(lambda: run.status == RunStatus.PAUSED)
    pid_b = _single_pending_id(run, exclude=frozenset({pid_a}))
    assert pid_b != pid_a
    assert run.pending_prompts[pid_b].prompt == "second"

    snapshot = mgr.get_state(run.run_id)
    # State already contains 'a', not yet 'b'.
    assert snapshot["prime"]["a"]["value"] == "A-out"
    assert snapshot.get("prime", {}).get("b", {}).get("value") is None

    # Mutating the snapshot must not affect the live state.
    snapshot["prime"]["a"]["value"] = "MUTATED"
    mgr.submit_response(run_id=run.run_id, prompt_id=pid_b, response_text="B-out")
    assert _wait_until(lambda: run.status == RunStatus.COMPLETED)
    assert run.state["prime"]["a"]["value"] == "A-out"
    assert run.state["prime"]["b"]["value"] == "B-out"


# ---------------------------------------------------------------------------
# 11. Quiescence detection waits for slow-spawn parallel branches
# ---------------------------------------------------------------------------


def test_quiescence_returns_after_all_branches_settle(tmp_path: Path) -> None:
    """
    An artificial scheduling delay well beyond any fixed debounce window
    (500ms per branch, three branches) must still leave all three branches
    in `pending_prompts` on the initial `start_run()` return. A time-based
    quiescence window — however large — can always be raced by a long
    enough delay; event-based quiescence (#237) cannot, because it waits
    for the `flow: tree` loop's own `concurrent_dispatch` signal (the real
    branch count, known before any of them run) rather than guessing from
    "no change observed for N seconds". This is exactly the regression the
    old `_wait_for_quiesce` had: on main, this delay reliably makes
    `start_run()` return with 0-2 branches registered, not 3.
    """
    import time
    from unittest.mock import patch

    delay_mgr = RunManager(quiesce_max_wait_seconds=3.0, worker_poll_interval=0.05)
    p = _write_yml(tmp_path, "slow.yml", """
        model: claude-sonnet-4
        adapter: host_claude
        effects:
          - type: loop
            name: par
            flow: tree
            max_concurrency: 3
            each:
              in: input.items
              as: it
            body:
              - type: prompt
                name: ans
                template: "for {{it}}"
    """)

    real_handler_for = delay_mgr._handler_for

    def _delayed_handler_for(run, host_req):  # type: ignore[no-untyped-def]
        # Every branch's own thread sleeps before registering its prompt —
        # simulating exactly the "worker thread hasn't been scheduled yet"
        # race the issue describes, at a magnitude no fixed window survives.
        time.sleep(0.5)
        return real_handler_for(run, host_req)

    with patch.object(delay_mgr, "_handler_for", side_effect=_delayed_handler_for):
        run = delay_mgr.start_run(
            orchestration_path=p, initial_state={"items": ["a", "b", "c"]},
        )
        # Intentionally asserted immediately (not polled): this test exists
        # to prove start_run's own wait already blocked for the real signal,
        # so polling here would defeat the point.
        assert len(run.pending_prompts) == 3, (
            f"quiescence did not wait for the real signal; saw "
            f"{len(run.pending_prompts)} branches"
        )

    # Cleanup: cancel so daemon threads don't linger
    delay_mgr.cancel_run(run.run_id)


# ---------------------------------------------------------------------------
# 12. Unknown run/prompt IDs raise KeyError (structured at server layer)
# ---------------------------------------------------------------------------


def test_unknown_run_id_raises_keyerror(mgr: RunManager) -> None:
    with pytest.raises(KeyError, match="Unknown run_id"):
        mgr.submit_response(run_id="nope", prompt_id="x", response_text="y")
    with pytest.raises(KeyError, match="Unknown run_id"):
        mgr.get_state("nope")
    with pytest.raises(KeyError, match="Unknown run_id"):
        mgr.cancel_run("nope")


def test_unknown_prompt_id_raises_keyerror(mgr: RunManager, tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "one.yml", """
        model: claude-sonnet-4
        adapter: host_claude
        effects:
          - type: prompt
            name: x
            template: "hi"
    """)
    run = mgr.start_run(orchestration_path=p)
    valid_pid = _single_pending_id(run)

    with pytest.raises(KeyError, match="Unknown prompt_id"):
        mgr.submit_response(run_id=run.run_id, prompt_id="bogus", response_text="x")

    # Run still alive; valid pid still works.
    assert run.status == RunStatus.PAUSED
    assert valid_pid in run.pending_prompts
    mgr.submit_response(run_id=run.run_id, prompt_id=valid_pid, response_text="ok")
    assert _wait_until(lambda: run.status == RunStatus.COMPLETED)


# ---------------------------------------------------------------------------
# 13. override_model wires through to the adapter
# ---------------------------------------------------------------------------


def test_override_model_runs_non_claude_orchestration(
    mgr: RunManager, tmp_path: Path
) -> None:
    p = _write_yml(tmp_path, "non_claude.yml", """
        model: gpt-4o
        adapter: host_claude
        effects:
          - type: prompt
            name: ask
            template: "hi"
    """)
    run = mgr.start_run(orchestration_path=p, override_model=True)
    assert _wait_until(lambda: run.status == RunStatus.PAUSED)
    pid = _single_pending_id(run)
    mgr.submit_response(run_id=run.run_id, prompt_id=pid, response_text="resp")
    assert _wait_until(lambda: run.status == RunStatus.COMPLETED)
    assert run.state["prime"]["ask"]["value"] == "resp"


def test_default_strict_rejects_non_claude_pin(mgr: RunManager, tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "rejected.yml", """
        model: gpt-4o
        adapter: host_claude
        effects:
          - type: prompt
            name: ask
            template: "hi"
    """)
    run = mgr.start_run(orchestration_path=p)  # override_model=False default
    assert _wait_until(lambda: run.status.is_terminal, timeout=2.0)
    assert run.status == RunStatus.FAILED
    assert "Claude-family" in (run.error or "")


# ---------------------------------------------------------------------------
# 14. An untrusted project config in cwd surfaces its skip warning (#278)
# ---------------------------------------------------------------------------


def test_untrusted_project_config_warning_reaches_the_run(
    mgr: RunManager, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / "circuitry.config.json").write_text(
        '{"default_model": "project-model"}', encoding="utf-8"
    )
    monkeypatch.chdir(tmp_path)
    p = _write_yml(tmp_path, "hello.yml", """
        model: claude-sonnet-4
        adapter: host_claude
        effects:
          - type: prompt
            name: greet
            template: "Say hi"
    """)
    run = mgr.start_run(orchestration_path=p)

    pid = _single_pending_id(run)
    mgr.submit_response(run_id=run.run_id, prompt_id=pid, response_text="the answer")

    assert _wait_until(lambda: run.status == RunStatus.COMPLETED)
    assert any("Skipped project config" in w for w in run.warnings)
