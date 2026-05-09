from __future__ import annotations

import textwrap
import threading
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
    # Tighter quiesce for fast tests; still covers the slow-branch case
    # (test_quiescence_returns_after_all_branches_settle uses a sleep
    # well beyond this threshold).
    m = RunManager(
        quiesce_seconds=0.02,
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


def _single_pending_id(run) -> str:
    assert len(run.pending_prompts) == 1, list(run.pending_prompts)
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

    assert run.status == RunStatus.PAUSED
    pid = _single_pending_id(run)
    assert run.pending_prompts[pid].prompt == "Say hi"

    mgr.submit_response(run_id=run.run_id, prompt_id=pid, response_text="the answer")

    assert _wait_until(lambda: run.status == RunStatus.COMPLETED)
    assert run.state["prime"]["greet"]["value"] == "the answer"


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

    assert run.status == RunStatus.PAUSED
    pid_b = _single_pending_id(run)
    assert run.pending_prompts[pid_b].prompt == "saw a=A-out"
    mgr.submit_response(run_id=run.run_id, prompt_id=pid_b, response_text="B-out")

    pid_c = _single_pending_id(run)
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
              in: names
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
    for i in range(3):
        assert run.status == RunStatus.PAUSED, f"iter {i}: {run.status}"
        pid = _single_pending_id(run)
        text = f"hi-{i}"
        responses.append(text)
        mgr.submit_response(run_id=run.run_id, prompt_id=pid, response_text=text)

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
              in: items
              as: it
            body:
              - type: prompt
                name: ask
                template: "Q: {{it}}"
    """)
    run = mgr.start_run(
        orchestration_path=p, initial_state={"items": ["alpha", "beta"]},
    )

    assert run.status == RunStatus.PAUSED
    assert len(run.pending_prompts) == 2
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
              in: items
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
              in: items
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
    post_pid = _single_pending_id(run)
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
              in: items
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
            template: "for {{tag}}"
    """)
    run_a = mgr.start_run(orchestration_path=p, initial_state={"tag": "A"})
    run_b = mgr.start_run(orchestration_path=p, initial_state={"tag": "B"})

    assert run_a.status == RunStatus.PAUSED
    assert run_b.status == RunStatus.PAUSED
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

    # submit_response returns after _wait_for_quiesce, so the next pending
    # prompt (for 'b') should already be present.
    assert run.status == RunStatus.PAUSED
    pid_b = _single_pending_id(run)
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
    With a 200ms quiesce window and one branch sleeping 50ms before it can
    spawn, all three branches MUST appear in pending_prompts on the
    initial start_run() return — proving quiescence kept polling.
    """
    big_quiesce_mgr = RunManager(
        quiesce_seconds=0.2, quiesce_max_wait_seconds=3.0, worker_poll_interval=0.05
    )
    p = _write_yml(tmp_path, "slow.yml", """
        model: claude-sonnet-4
        adapter: host_claude
        effects:
          - type: loop
            name: par
            flow: tree
            max_concurrency: 3
            each:
              in: items
              as: it
            body:
              - type: prompt
                name: ans
                template: "for {{it}}"
    """)
    run = big_quiesce_mgr.start_run(
        orchestration_path=p, initial_state={"items": ["a", "b", "c"]},
    )
    assert len(run.pending_prompts) == 3, (
        f"quiescence did not wait long enough; saw {len(run.pending_prompts)} branches"
    )

    # Cleanup: cancel so daemon threads don't linger
    big_quiesce_mgr.cancel_run(run.run_id)


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
    assert run.status == RunStatus.PAUSED
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
