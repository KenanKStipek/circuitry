"""`cof run --resume` engine behavior (#270, resume part): an effect that
finished successfully is skipped and its state reused; the failed or
unfinished effect and everything after it rerun. A named loop in chain flow
resumes at its first unfinished pass, keeping finished ``iter_<N>`` nodes.

These exercise the engine directly (``RunRequest``/``run``,
``DynamicRuntime``/``LoopRuntime``) with a counting fake adapter — the CLI's
own source-resolution and safety checks (``--resume last``/<run-id>,
content-hash, input-diff) are covered in ``tests/cli/test_run_resume.py``.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from circuitry.adapters.base import GenerateResult
from circuitry.cli.runtime_shim import RunRequest, run
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


class CountingAdapter:
    """Records every prompt it's asked to generate; fails on demand."""

    name = "counting"

    def __init__(self) -> None:
        self.calls: list[str] = []
        self.fail_prompts: set[str] = set()

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append(prompt)
        if prompt in self.fail_prompts:
            raise RuntimeError(f"boom: {prompt}")
        return GenerateResult(text=f"ok:{prompt}", raw={})


def _write(tmp_path: Path, name: str, content: str) -> Path:
    p = tmp_path / name
    p.write_text(content, encoding="utf-8")
    return p


def _chain_orch(tmp_path: Path) -> Path:
    return _write(
        tmp_path,
        "chain.yml",
        """
effects:
  - type: prompt
    name: step1
    template: "one"
  - type: prompt
    name: step2
    template: "two"
  - type: prompt
    name: step3
    template: "three"
  - type: prompt
    name: step4
    template: "four"
""".lstrip(),
    )


def test_resume_skips_completed_steps_and_reruns_the_failed_one(
    tmp_path: Path,
) -> None:
    orch = _chain_orch(tmp_path)
    adapter = CountingAdapter()
    adapter.fail_prompts = {"three"}

    first = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
        )
    )
    assert first.ok is False
    assert first.state["prime"]["step1"]["value"] == "ok:one"
    assert first.state["prime"]["step2"]["value"] == "ok:two"
    assert adapter.calls == ["one", "two", "three"]

    adapter.fail_prompts = set()
    second = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=first.state,
            resume=True,
        )
    )
    assert second.ok is True, second.error
    # step1/step2 must not have been re-dispatched: no new "one"/"two" calls.
    assert adapter.calls == ["one", "two", "three", "three", "four"]
    assert second.state["prime"]["step1"]["value"] == "ok:one"
    assert second.state["prime"]["step3"]["value"] == "ok:three"
    assert second.state["prime"]["step4"]["value"] == "ok:four"


def test_resume_skips_a_completed_yield_and_reruns_the_failed_step_after_it(
    tmp_path: Path,
) -> None:
    """A `yield` effect (#396) completes/fails through the same
    `meta.completed_at`/`meta.error` contract every other effect does, so
    resume's skip/rerun logic needs no special case for it."""
    orch = _write(
        tmp_path,
        "chain.yml",
        """\
effects:
  - type: yield
    name: brief
    template: "Topic: {{input.topic}}"
  - type: prompt
    name: step2
    template: "two"
""",
    )
    adapter = CountingAdapter()
    adapter.fail_prompts = {"two"}

    first = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state={"input": {"topic": "circuitry"}},
        )
    )
    assert first.ok is False
    assert first.state["prime"]["brief"]["value"] == "Topic: circuitry"
    assert adapter.calls == ["two"]

    adapter.fail_prompts = set()
    second = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=first.state,
            resume=True,
        )
    )
    assert second.ok is True, second.error
    # The yield must not have re-rendered (no way to observe that directly —
    # the adapter call count is the proxy: only step2 reruns).
    assert adapter.calls == ["two", "two"]
    assert second.state["prime"]["brief"]["value"] == "Topic: circuitry"
    assert second.state["prime"]["step2"]["value"] == "ok:two"


def test_resume_without_the_flag_reruns_everything(tmp_path: Path) -> None:
    """Plain `--state` carryover (no `--resume`) keeps today's behavior: a
    step that already succeeded still reruns — this is the existing
    "resume only loads state, doesn't skip" contract #270 asks for an
    *opt-in* change to, not a default-behavior change."""
    orch = _chain_orch(tmp_path)
    adapter = CountingAdapter()

    first = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
        )
    )
    assert first.ok is True
    assert adapter.calls == ["one", "two", "three", "four"]

    second = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=first.state,
            resume=False,
        )
    )
    assert second.ok is True
    assert adapter.calls == [
        "one", "two", "three", "four", "one", "two", "three", "four",
    ]


def test_resume_skips_a_fully_succeeded_dynamic_container(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "nested.yml",
        """
effects:
  - type: dynamic
    name: group
    flow: chain
    effects:
      - type: prompt
        name: inner1
        template: "a"
      - type: prompt
        name: inner2
        template: "b"
  - type: prompt
    name: after
    template: "c"
""".lstrip(),
    )
    adapter = CountingAdapter()
    adapter.fail_prompts = {"c"}

    first = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
        )
    )
    assert first.ok is False
    assert adapter.calls == ["a", "b", "c"]

    adapter.fail_prompts = set()
    second = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=first.state,
            resume=True,
        )
    )
    assert second.ok is True
    # The whole `group` dynamic already finished without error — skipped
    # wholesale, so neither inner prompt is called again.
    assert adapter.calls == ["a", "b", "c", "c"]


def _run_loop(orch: dict[str, Any], state: dict[str, Any], *, adapter: Any, resume: bool) -> dict[str, Any]:
    root = compile_orchestration(orch=orch, root_name="prime")
    DynamicRuntime(
        root, adapter=adapter, model="unit-test", resume=resume
    ).execute(store=Store(state))
    return state


def _each_loop_orch() -> dict[str, Any]:
    return {
        "effects": [
            {
                "type": "loop",
                "name": "frames",
                "flow": "chain",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {
                        "type": "prompt",
                        "name": "render",
                        "template": "frame-{{item}}",
                    }
                ],
            }
        ]
    }


def test_loop_resumes_at_first_unfinished_pass(tmp_path: Path) -> None:
    adapter = CountingAdapter()
    adapter.fail_prompts = {"frame-2"}
    state: dict[str, Any] = {"input": {"items": ["0", "1", "2", "3"]}}

    orch = _each_loop_orch()
    try:
        _run_loop(orch, state, adapter=adapter, resume=False)
    except RuntimeError:
        pass

    assert adapter.calls == ["frame-0", "frame-1", "frame-2"]
    assert state["prime"]["frames"]["iter_0"]["render"]["value"] == "ok:frame-0"
    assert state["prime"]["frames"]["iter_1"]["render"]["value"] == "ok:frame-1"

    adapter.fail_prompts = set()
    _run_loop(orch, state, adapter=adapter, resume=True)

    # Passes 0 and 1 are not re-rendered; only the failed pass 2 and the
    # never-reached pass 3 dispatch this time.
    assert adapter.calls == ["frame-0", "frame-1", "frame-2", "frame-2", "frame-3"]
    node = state["prime"]["frames"]
    assert node["iter_0"]["render"]["value"] == "ok:frame-0"
    assert node["iter_1"]["render"]["value"] == "ok:frame-1"
    assert node["iter_2"]["render"]["value"] == "ok:frame-2"
    assert node["iter_3"]["render"]["value"] == "ok:frame-3"
    assert node["value"]["iterations"] == 4


def test_loop_without_resume_flag_reruns_every_pass(tmp_path: Path) -> None:
    adapter = CountingAdapter()
    state: dict[str, Any] = {"input": {"items": ["0", "1"]}}
    orch = _each_loop_orch()

    _run_loop(orch, state, adapter=adapter, resume=False)
    assert adapter.calls == ["frame-0", "frame-1"]

    _run_loop(orch, state, adapter=adapter, resume=False)
    assert adapter.calls == ["frame-0", "frame-1", "frame-0", "frame-1"]


def test_resume_reruns_everything_after_an_absorbed_on_error_continue_failure(
    tmp_path: Path,
) -> None:
    """#270 F2 (case 2): an `on_error: continue` failure that the chain
    absorbed must not leave later, already-finished effects skippable on a
    later resume — the chain reran past that point last time, so it must
    again, even though those later nodes still look finished."""
    orch = _write(
        tmp_path,
        "chain.yml",
        """
effects:
  - type: prompt
    name: step1
    template: "one"
  - type: prompt
    name: step2
    template: "two"
    on_error: continue
  - type: prompt
    name: step3
    template: "three"
""".lstrip(),
    )
    adapter = CountingAdapter()
    adapter.fail_prompts = {"two"}

    first = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
        )
    )
    assert first.ok is True, first.error
    assert adapter.calls == ["one", "two", "three"]
    assert first.state["prime"]["step2"]["meta"]["error"]

    adapter.fail_prompts = set()
    second = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=first.state,
            resume=True,
        )
    )
    assert second.ok is True, second.error
    # step1 stays skipped; step2 reruns (it never finished without error)
    # and step3 reruns too, even though its old node already looked
    # finished — positional, not per-effect.
    assert adapter.calls == ["one", "two", "three", "two", "three"]
    assert not second.state["prime"]["step2"]["meta"]["error"]


def test_resume_reruns_a_stale_node_left_after_the_actual_stop_point(
    tmp_path: Path,
) -> None:
    """#270 F2 (case 1): a state seeded from an earlier, fully-successful
    run (--state prev.json carryover) that then fails partway through this
    run must not reuse the earlier run's now-stale later nodes."""
    orch = _chain_orch(tmp_path)
    state: dict[str, Any] = {
        "prime": {
            "step1": {"value": "ok:one", "meta": {"completed_at": "t", "error": None}},
            "step2": {"value": "ok:two", "meta": {"completed_at": "t", "error": None}},
            # This run's own stop point: never finished.
            "step3": {"value": None, "meta": {"completed_at": None, "error": None}},
            # Stale: left over from the earlier run this state was seeded
            # from, at a position after the stop point above.
            "step4": {"value": "ok:four-stale", "meta": {"completed_at": "t", "error": None}},
        }
    }
    adapter = CountingAdapter()

    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=state,
            resume=True,
        )
    )
    assert result.ok is True, result.error
    assert adapter.calls == ["three", "four"]
    assert result.state["prime"]["step4"]["value"] == "ok:four"


def test_resume_skips_a_finished_child_of_a_tree_dynamic(tmp_path: Path) -> None:
    """#270 F3: a tree-flow dynamic's children are each checked (and
    skipped) independently, not always rerun wholesale."""
    orch = _write(
        tmp_path,
        "tree.yml",
        """
effects:
  - type: dynamic
    name: group
    flow: tree
    effects:
      - type: prompt
        name: a
        template: "a"
      - type: prompt
        name: b
        template: "b"
""".lstrip(),
    )
    adapter = CountingAdapter()
    adapter.fail_prompts = {"b"}

    first = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
        )
    )
    assert first.ok is False
    assert sorted(adapter.calls) == ["a", "b"]
    assert first.state["prime"]["group"]["a"]["value"] == "ok:a"

    adapter.fail_prompts = set()
    adapter.calls = []
    second = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=first.state,
            resume=True,
        )
    )
    assert second.ok is True, second.error
    # "a" already finished the first time — not re-dispatched.
    assert adapter.calls == ["b"]


def test_resume_twice_does_not_rerun_an_if_that_already_succeeded(
    tmp_path: Path,
) -> None:
    """#270 F4: a conditional that failed once and then succeeded on a
    resume must not be stuck looking "still failed" (stale meta.error) on
    a *later* resume triggered by something else further down the chain."""
    orch = _write(
        tmp_path,
        "chain.yml",
        """
effects:
  - type: if
    name: gate
    if:
      mode: cel
      expr: "true"
    then:
      - type: prompt
        name: inner
        template: "inner"
  - type: prompt
    name: after
    template: "after"
""".lstrip(),
    )
    adapter = CountingAdapter()
    adapter.fail_prompts = {"inner"}

    first = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
        )
    )
    assert first.ok is False
    assert adapter.calls == ["inner"]

    adapter.fail_prompts = {"after"}
    second = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=first.state,
            resume=True,
        )
    )
    assert second.ok is False
    # `gate` succeeds this time (meta.error/completed_at freshly reset at
    # the top of this call) and `after` fails.
    assert adapter.calls == ["inner", "inner", "after"]
    assert not second.state["prime"]["gate"]["meta"]["error"]

    adapter.fail_prompts = set()
    third = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=second.state,
            resume=True,
        )
    )
    assert third.ok is True, third.error
    # `gate` stays skipped — without clearing its stale meta.error on the
    # second run's success, this would re-run `inner` a third time.
    assert adapter.calls == ["inner", "inner", "after", "after"]


def test_loop_resume_twice_does_not_rerun_a_pass_that_already_succeeded(
    tmp_path: Path,
) -> None:
    """#270 F4/F5: same stale-error/stale-completed_passes concern, for a
    named loop resumed twice."""
    adapter = CountingAdapter()
    adapter.fail_prompts = {"frame-1"}
    state: dict[str, Any] = {"input": {"items": ["0", "1"]}}
    orch = _each_loop_orch()

    try:
        _run_loop(orch, state, adapter=adapter, resume=False)
    except RuntimeError:
        pass
    assert adapter.calls == ["frame-0", "frame-1"]

    adapter.fail_prompts = set()
    _run_loop(orch, state, adapter=adapter, resume=True)
    assert adapter.calls == ["frame-0", "frame-1", "frame-1"]
    assert not state["prime"]["frames"]["meta"]["error"]

    # A second resume (as if triggered by an unrelated later failure) must
    # not re-render either pass: the loop's own completed_passes record
    # (not a stale meta.error from the first failed attempt) says both
    # already finished.
    _run_loop(orch, state, adapter=adapter, resume=True)
    assert adapter.calls == ["frame-0", "frame-1", "frame-1"]


def test_resume_skips_body_but_always_reruns_finally(tmp_path: Path) -> None:
    """`cof run --resume` (#330) is positional-skip over the body only —
    `finally:` (#272) always runs again on a resumed run, regardless of
    whether its own node already completed OK in the prior run: cleanup
    (stopping a server, releasing a lock) must happen again every time the
    body finishes, not just the first time it ever ran."""
    orch = _write(
        tmp_path,
        "resume_finally.yml",
        """
effects:
  - type: prompt
    name: step1
    template: "one"
  - type: prompt
    name: step2
    template: "two"
finally:
  - type: prompt
    name: cleanup
    template: "cleanup"
""".lstrip(),
    )
    adapter = CountingAdapter()
    adapter.fail_prompts = {"two"}

    first = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
        )
    )
    assert first.ok is False
    # `finally:` still ran despite the body failing on step2.
    assert adapter.calls == ["one", "two", "cleanup"]
    assert first.state["prime"]["cleanup"]["value"] == "ok:cleanup"

    adapter.fail_prompts = set()
    second = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=first.state,
            resume=True,
        )
    )
    assert second.ok is True, second.error
    # step1 is skipped (no new "one" call); step2 reruns (it failed last
    # time); cleanup reruns too even though it already succeeded in the
    # first run — a second "cleanup" call proves `finally:` wasn't skipped
    # by resume the way a completed body effect would be.
    assert adapter.calls == ["one", "two", "cleanup", "two", "cleanup"]
    assert second.state["prime"]["step1"]["value"] == "ok:one"
    assert second.state["prime"]["step2"]["value"] == "ok:two"
    assert second.state["prime"]["cleanup"]["value"] == "ok:cleanup"


def test_resume_skips_a_finished_reflector_and_reruns_the_failed_step_after_it(
    tmp_path: Path,
) -> None:
    """issue #585: a reflector that finished without error now records
    `meta.completed_at`, so `--resume` skips it whole exactly as it would
    any other finished effect -- no planning call at all, and the chain
    step after it (which never finished in the saved state) reruns.
    """
    orch = _write(
        tmp_path,
        "reflector_resume.yml",
        """
effects:
  - type: reflector
    name: plan_it
    prime_template: "Plan it."
    effects:
      - type: prompt
        name: propose_steps
        template: "propose"
  - type: prompt
    name: later
    template: "later"
""".lstrip(),
    )
    adapter = CountingAdapter()
    state: dict[str, Any] = {
        "prime": {
            "plan_it": {
                "value": True,
                "meta": {
                    "iterations": [{"i": 0, "done": True, "stop": True}],
                    "completed_at": "t",
                    "error": None,
                },
            },
            "later": {"value": None, "meta": {"completed_at": None, "error": None}},
        }
    }

    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=state,
            resume=True,
        )
    )
    assert result.ok is True, result.error
    # The reflector is skipped whole: no planning call at all.
    assert adapter.calls == ["later"]
    assert result.state["prime"]["plan_it"]["meta"]["completed_at"] == "t"
    assert result.state["prime"]["later"]["value"] == "ok:later"


def test_resume_reruns_a_reflector_that_did_not_finish(tmp_path: Path) -> None:
    """issue #585: a reflector whose saved node shows no ``meta.
    completed_at`` at all -- the process stopped (a crash, an interrupt)
    before this effect ever finished -- reruns from its start on resume,
    same as any other effect: the generic ``effect_completed_ok`` check
    this relies on requires a truthy ``completed_at``, and this node has
    none.
    """
    orch = _write(
        tmp_path,
        "reflector_resume_fail.yml",
        """
effects:
  - type: reflector
    name: plan_it
    prime_template: "Plan it."
    effects:
      - type: prompt
        name: propose_steps
        template: "propose"
""".lstrip(),
    )
    # A dedicated planner reply (never `CountingAdapter`'s generic
    # `ok:<prompt>` echo, which the reflector's own YAML-plan parsing
    # would reject): "done: true" stops cleanly after one iteration.
    class _StopReflectorAdapter:
        name = "stop-reflector"

        def __init__(self) -> None:
            self.calls: list[str] = []

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            self.calls.append(prompt)
            return GenerateResult(text="done: true", raw={})

    adapter = _StopReflectorAdapter()
    state: dict[str, Any] = {
        "prime": {
            "plan_it": {"value": None, "meta": {"iterations": []}},
        }
    }

    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=state,
            resume=True,
        )
    )
    assert result.ok is True, result.error
    # Reran from its start: one fresh planning call.
    assert len(adapter.calls) == 1
    assert not result.state["prime"]["plan_it"]["meta"].get("error")
    assert result.state["prime"]["plan_it"]["meta"]["completed_at"]


def test_resume_reruns_a_failed_reflector_and_clears_its_old_error(
    tmp_path: Path,
) -> None:
    """issue #585 follow-up: a reflector that failed on a prior attempt
    and then succeeds on resume reads as finished -- ``meta.error`` is
    cleared at the start of this run, not left over from the attempt
    that failed, which `effect_completed_ok` would otherwise keep
    reading as a failure on every later resume even after a run that
    actually succeeded.
    """
    orch = _write(
        tmp_path,
        "reflector_resume_clears_error.yml",
        """
effects:
  - type: reflector
    name: plan_it
    prime_template: "Plan it."
    effects:
      - type: prompt
        name: propose_steps
        template: "propose"
""".lstrip(),
    )

    class _StopReflectorAdapter:
        name = "stop-reflector"

        def __init__(self) -> None:
            self.calls: list[str] = []

        def generate(
            self, *, model: str, prompt: str, timeout_seconds: int = 120
        ) -> GenerateResult:
            self.calls.append(prompt)
            return GenerateResult(text="done: true", raw={})

    adapter = _StopReflectorAdapter()
    # A prior attempt that failed: completed_at *and* error both set --
    # exactly what `reflector.py::ReflectorRuntime.execute`'s own
    # failure path writes, left over from before this issue's fix.
    state: dict[str, Any] = {
        "prime": {
            "plan_it": {
                "value": False,
                "meta": {
                    "iterations": [{"i": 0, "error": "boom", "stop": False}],
                    "completed_at": "t",
                    "error": "boom",
                },
            },
        }
    }

    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=state,
            resume=True,
        )
    )
    assert result.ok is True, result.error
    # `effect_completed_ok` required a rerun (meta.error was truthy) --
    # one fresh planning call.
    assert len(adapter.calls) == 1
    plan_it = result.state["prime"]["plan_it"]
    assert plan_it["value"] is True
    assert not plan_it["meta"].get("error")
    assert plan_it["meta"]["completed_at"]
    from circuitry.core.resume import effect_completed_ok

    assert effect_completed_ok(plan_it) is True


def test_resume_reruns_a_finished_use_whose_resolved_inputs_changed(
    tmp_path: Path,
) -> None:
    """issue #584: a finished `use`'s inputs, resolved against the
    resumed run's own inputs, are checked against its recorded
    `meta.inputs`. Changed, it reruns (and so does the chain step after
    it); unchanged, it stays skipped (`test_resume_skips_a_finished_use_
    whose_resolved_inputs_are_unchanged` below).
    """
    _write(
        tmp_path,
        "child.yml",
        """
interface:
  inputs:
    x: {type: string, required: true}
effects:
  - type: tool
    name: noop
    provider: json
    params: {mode: parse, input: '{"ok": true}'}
""".lstrip(),
    )
    orch = _write(
        tmp_path,
        "use_resume.yml",
        """
interface:
  inputs:
    x: {type: string, default: a}
effects:
  - type: use
    name: part
    path: child.yml
    inputs: {x: {from: input.x}}
  - type: prompt
    name: later
    template: "later"
""".lstrip(),
    )
    adapter = CountingAdapter()
    adapter.fail_prompts = {"later"}

    first = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state={"input": {"x": "a"}},
        )
    )
    assert first.ok is False
    assert first.state["prime"]["part"]["meta"]["inputs"] == {"x": "a"}
    first_created_at = first.state["prime"]["part"]["meta"]["created_at"]

    adapter.fail_prompts = set()
    resumed_state: dict[str, Any] = first.state
    resumed_state["input"]["x"] = "b"
    second = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=resumed_state,
            resume=True,
        )
    )
    assert second.ok is True, second.error
    assert second.state["prime"]["part"]["meta"]["inputs"] == {"x": "b"}
    assert second.state["prime"]["part"]["meta"]["created_at"] != first_created_at
    assert second.state["prime"]["later"]["value"] == "ok:later"


def test_resume_skips_a_finished_use_whose_resolved_inputs_are_unchanged(
    tmp_path: Path,
) -> None:
    """issue #584's other half: an unchanged `-e`/run input leaves a
    finished `use` skipped whole, as before this issue."""
    _write(
        tmp_path,
        "child.yml",
        """
interface:
  inputs:
    x: {type: string, required: true}
effects:
  - type: tool
    name: noop
    provider: json
    params: {mode: parse, input: '{"ok": true}'}
""".lstrip(),
    )
    orch = _write(
        tmp_path,
        "use_resume_unchanged.yml",
        """
interface:
  inputs:
    x: {type: string, default: a}
effects:
  - type: use
    name: part
    path: child.yml
    inputs: {x: {from: input.x}}
  - type: prompt
    name: later
    template: "later"
""".lstrip(),
    )
    adapter = CountingAdapter()
    adapter.fail_prompts = {"later"}

    first = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state={"input": {"x": "a"}},
        )
    )
    assert first.ok is False
    first_created_at = first.state["prime"]["part"]["meta"]["created_at"]

    adapter.fail_prompts = set()
    second = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=first.state,
            resume=True,
        )
    )
    assert second.ok is True, second.error
    assert second.state["prime"]["part"]["meta"]["inputs"] == {"x": "a"}
    assert second.state["prime"]["part"]["meta"]["created_at"] == first_created_at
