"""`--state`/`--resume` across engines (issue #727): a run interrupted by
a real SIGINT under one engine resumes under the other -- inside a chain
loop's own pass, inside a tree loop (the whole loop reruns), inside a
tree-flow `dynamic` branch, inside a `use` child with `runtime.state.
record_children` (the use continues inside it), inside a `use` with
`resume: restart` (the child reruns anyway), inside a named `if` branch,
and inside a `finally:` block -- ending in the same final state a
straight-through, uninterrupted run on the resuming engine would have
produced.

Every scenario but one blocks on a plain `shell` step until this test
sends SIGINT, only once the step's own `meta.created_at` shows up in a
polled `--live-state` file (never merely a marker file existing on
disk: that file's first write can land before the step whose start
this test is actually waiting for -- the exact race that broke
`electricity/crates/electricity-cli/tests/signals_loop.rs`'s own
`wait_until_started`, before it switched to the same live-state-polling
shape this module copies). The same step checks a `proceed` marker
before blocking at all -- absent for the interrupted run (and the
resume that continues it), present up front for the baseline -- so the
*same* document resumes into a step that actually finishes this time,
with no second interrupt needed, and a baseline run of that already-
"proceed"-marked document is the "same as if it had never been
interrupted" reference the resumed run is compared against (both
produced by the engine that resumes -- the interrupting engine's own
on-disk state format never has to match the other engine's byte for
byte, only `hydrate_resumed_state`'s read of it does).

The `finally` scenario is the one exception: on both engines,
cancellation is deliberately suppressed for the whole duration of a
`finally:` block (`core/dynamic.py::DynamicRuntime.execute`'s own
`get_token().cleanup()`, ported directly), so a real SIGINT landing
inside one never interrupts anything running there at all -- confirmed
directly against this very step before writing this comment. It uses
the other deterministic interrupt instead: a step
that fails outright the first time (`exit 1` unless `proceed` already
exists), with no signal involved -- the run still ends with `--out`
written and the step's own node unfinished (`meta.error` set), exactly
as an interrupt would leave it for resume's own purposes
(`core/resume.py::effect_completed_ok` cares about *that*, not about
*why* the node never finished).

Every "quick" step besides the one that blocks uses the deterministic
`json` tool (never `uuid`): a baseline run and an interrupted-then-
resumed run must agree on that step's own value for the final-state
comparison to mean anything, and a tool whose own output is different
every time it runs would make that comparison meaningless by
construction, independent of whether resuming actually worked.

Cross-engine comparison of the resumed pass's own live state and events
is established two ways: directly, by comparing the resumed run's final
`--out` state and `--events` stream against a straight-through baseline
run *on the same engine* (`_EXPECT_IDENTICAL_EVENTS` scenarios, where
nothing is skipped on resume so the two streams have no reason to
differ); and transitively for every scenario, every pair in `ENGINES`
being exercised as both `interrupt_engine` and `resume_engine` means a
divergence between what cof and electricity each consider "the same
final state" after an identical interrupt-and-resume would already fail
one of the two single-engine comparisons (`resumed == this engine's own
baseline`) before it could ever hide behind the other.
"""

from __future__ import annotations

import json
import signal
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import conformance.harness as harness
import pytest
from _signal_test_support import _diagnose_and_fail, _sandboxed_env
from conformance.normalize import (
    assert_events_equal,
    assert_states_equal,
    prepare_for_comparison,
)

ENGINES = ("cof", "electricity")

#: Scenarios where nothing earlier in the chain is ever skipped on
#: resume (the blocking step is the *first* thing in its own scope, or
#: the whole scope reruns regardless), so a resumed run's own `--events`
#: must match a straight-through baseline's exactly -- not just the
#: final state. The other scenarios skip a step that already finished
#: (a quick child kept by `record_children`, a tree branch or a
#: `finally:` body that was never re-run), so their own event counts are
#: legitimately smaller than a from-scratch baseline's; checked there
#: only for having something ok to show, not for exact equality.
_EXPECT_IDENTICAL_EVENTS = frozenset(
    {"chain_loop", "tree_loop", "named_if", "use_restart", "reflector_interrupted"}
)


@pytest.fixture(scope="module")
def electricity_binary() -> Path:
    """The shared `electricity` binary -- see `harness.resolve_electricity_binary`."""
    binary, failure = harness.resolve_electricity_binary()
    if failure is not None:
        pytest.fail(failure)
    if binary is None:
        pytest.skip("cargo not available; electricity binary cannot be built")
    return binary


def _cmd(engine: str, binary: Path, args: list[str]) -> list[str]:
    if engine == "cof":
        return [sys.executable, "-m", "circuitry.cli.app", "run", *args]
    return [str(binary), "run", *args]


def _popen(
    engine: str, binary: Path, args: list[str], *, cwd: Path, env: dict[str, str]
) -> subprocess.Popen[str]:
    return subprocess.Popen(
        _cmd(engine, binary, args), cwd=cwd, env=env,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )


def _run_to_completion(
    engine: str, binary: Path, args: list[str], *, cwd: Path, env: dict[str, str],
    timeout: float = 30.0,
) -> subprocess.CompletedProcess[str]:
    proc = _popen(engine, binary, args, cwd=cwd, env=env)
    try:
        stdout, stderr = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        _diagnose_and_fail(proc, timeout=timeout, label=f"waiting for {engine} to finish")
        stdout, stderr = "", ""  # _diagnose_and_fail always raises; unreachable
    return subprocess.CompletedProcess(proc.args, proc.returncode, stdout, stderr)


def _write(path: Path, content: str) -> Path:
    path.write_text(content, encoding="utf-8")
    return path


def _find_created_at(tree: Any, effect_name: str) -> bool:
    """Whether *tree* (a parsed `--live-state` snapshot) holds a node
    named *effect_name*, anywhere, whose own `meta.created_at` is set --
    the one thing that's true starting the instant that effect's own
    execution began, on either engine, regardless of how deep a tree-
    flow loop/dynamic nests it or what index a parallel pass lands at."""
    if isinstance(tree, dict):
        child = tree.get(effect_name)
        if isinstance(child, dict):
            meta = child.get("meta")
            if isinstance(meta, dict) and meta.get("created_at"):
                return True
        return any(_find_created_at(v, effect_name) for v in tree.values())
    if isinstance(tree, list):
        return any(_find_created_at(v, effect_name) for v in tree)
    return False


def _wait_for_step_started(
    proc: subprocess.Popen[str], live_state_path: Path, effect_name: str, *, timeout: float = 20.0,
) -> None:
    """Polls *live_state_path* until *effect_name*'s own `meta.created_at`
    appears -- never a marker file's mere existence (this module's own
    docstring: that races the write this test actually needs to have
    already happened)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if live_state_path.exists():
            try:
                text = live_state_path.read_text(encoding="utf-8")
            except OSError:
                text = ""
            if text:
                try:
                    tree = json.loads(text)
                except json.JSONDecodeError:
                    tree = None
                if tree is not None and _find_created_at(tree, effect_name):
                    return
        if proc.poll() is not None:
            break
        time.sleep(0.02)
    _diagnose_and_fail(
        proc, timeout=timeout, label=f"waiting for {effect_name}'s own meta.created_at"
    )


def _wait_for_marker_file(
    proc: subprocess.Popen[str], marker_path: Path, *, timeout: float = 20.0,
) -> None:
    """Polls for *marker_path* existing -- the blocking step's own
    script writes it only once that specific step actually starts
    running, so unlike `--live-state`'s own first write (this module's
    own docstring on `_wait_for_step_started`), there is no earlier,
    unrelated event this could fire on instead. Used only where `--
    live-state` itself can't be trusted (`_Scenario.wait_mode`'s own
    doc comment)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if marker_path.exists():
            return
        if proc.poll() is not None:
            break
        time.sleep(0.02)
    _diagnose_and_fail(proc, timeout=timeout, label=f"waiting for {marker_path.name} to appear")


def _blocking_shell_effect(
    name: str, *, proceed: str = "proceed", started: str = "started", indent: str,
) -> str:
    """A `shell` effect that blocks until killed, unless *proceed*
    already exists on disk -- in which case it finishes at once.
    Bounded (#661): killed by the engine's own cancellation handling on
    SIGINT, the same way `tests/cli/test_run_cancel_parallel.py`'s own
    branch scripts are, never an unbounded wait of its own. *proceed*/
    *started* are bare filenames, resolved against the shell step's own
    cwd (the engine's own process cwd, with no explicit `params.cwd`)
    -- never an absolute path, so the same orchestration text, byte for
    byte, works from any directory; two different case directories
    running the identical document then also have the identical
    `runtime.last_run.document_hash`. *started* is only ever read by a
    `_Scenario.wait_mode == "marker_file"` scenario (that attribute's
    own doc comment); writing it costs nothing for a `live_state`
    scenario, which never looks at it."""
    script = f"[ -f {proceed} ] || {{ touch {started}; exec sleep 60; }}"
    field_indent = indent + "  "
    return (
        f"{indent}- type: tool\n"
        f"{field_indent}name: {name}\n"
        f"{field_indent}provider: shell\n"
        f"{field_indent}params:\n"
        f"{field_indent}  command: bash\n"
        f'{field_indent}  args: ["-c", "{script}"]\n'
        f'{field_indent}  allowed_commands: ["bash"]\n'
    )


def _failing_shell_effect(name: str, *, proceed: str = "proceed", indent: str) -> str:
    """A `shell` effect that fails outright unless *proceed* already
    exists on disk -- the `finally` scenario's own deterministic-
    interrupt option (this module's own docstring on why it needs one
    other than a blocking step plus SIGINT)."""
    script = f"[ -f {proceed} ]"
    field_indent = indent + "  "
    return (
        f"{indent}- type: tool\n"
        f"{field_indent}name: {name}\n"
        f"{field_indent}provider: shell\n"
        f"{field_indent}params:\n"
        f"{field_indent}  command: bash\n"
        f'{field_indent}  args: ["-c", "{script}"]\n'
        f'{field_indent}  allowed_commands: ["bash"]\n'
    )


def _quick_step(name: str, *, indent: str) -> str:
    """A deterministic step (never `uuid`): a baseline run and a
    resumed run must agree on its own value for the final-state
    comparison below to mean anything."""
    field_indent = indent + "  "
    return (
        f"{indent}- type: tool\n"
        f"{field_indent}name: {name}\n"
        f"{field_indent}provider: json\n"
        f"{field_indent}params: {{mode: parse, input: '{{\"ok\": true}}'}}\n"
    )


def _step_log_child_orchestration(*, log: str = "steplog.txt") -> str:
    """A plain chain of three `shell` steps, each appending its own name
    to *log* every time it runs (never just once, however many times
    the orchestration re-executes it): s1 (quick), s2 (blocks until
    `proceed` exists on disk, logging itself before it blocks), s3
    (quick). Lets a test tell a step `--resume` correctly skipped
    (logged once total) apart from one it reran from scratch (logged
    twice) -- unlike comparing final state, which can't: `s1` finishing
    a second identical time looks the same in `--out` either way."""
    s2_script = f"echo s2 >> {log}; [ -f proceed ] || {{ touch started; exec sleep 60; }}"
    return (
        "effects:\n"
        "  - type: tool\n    name: s1\n    provider: shell\n    params:\n"
        "      command: bash\n"
        f'      args: ["-c", "echo s1 >> {log}"]\n'
        '      allowed_commands: ["bash"]\n'
        "  - type: tool\n    name: s2\n    provider: shell\n    params:\n"
        "      command: bash\n"
        f'      args: ["-c", "{s2_script}"]\n'
        '      allowed_commands: ["bash"]\n'
        "  - type: tool\n    name: s3\n    provider: shell\n    params:\n"
        "      command: bash\n"
        f'      args: ["-c", "echo s3 >> {log}"]\n'
        '      allowed_commands: ["bash"]\n'
    )


class _Scenario:
    def __init__(
        self, *, orchestration: str, child: str | None, config: str, blocking_name: str,
        wait_mode: str = "live_state", interrupt_mode: str = "signal",
        replies_file: str | None = None,
    ) -> None:
        self.orchestration = orchestration
        self.child = child
        self.config = config
        self.blocking_name = blocking_name
        # A `scripted`-adapter replies file (#585/#584) -- written as
        # `scripted-replies.yaml` in the case directory when set, for a
        # scenario whose document names `adapter: scripted` (a
        # reflector's own planning prompt).
        self.replies_file = replies_file
        # "signal" (the default): spawn, wait for the blocking step to
        # start, send SIGINT. "fail_first_time": just run to completion
        # once with no `proceed` marker and no signal at all -- the
        # `finally` scenario's own case (this module's own docstring on
        # why).
        self.interrupt_mode = interrupt_mode
        # "live_state": poll `--live-state` for `blocking_name`'s own
        # `meta.created_at` (the default, and what every scenario below
        # uses except the two named here). "marker_file": poll for a
        # `started` file the blocking step's own script writes instead
        # -- `--live-state` never republishes a `use` child's own
        # effects past its first write (confirmed directly: the same
        # document finishes correctly and instantly once `proceed`
        # already exists, so this is a live-state-mirroring gap, not a
        # hang -- out of scope for this PR's own `--resume`/`--state`
        # fix set, flagged separately).
        self.wait_mode = wait_mode


_PLAIN_CONFIG = "{}"
_RECORD_CHILDREN_CONFIG = '{"runtime": {"state": {"record_children": true}}}'


def _scenario(name: str) -> _Scenario:
    if name == "chain_loop":
        step = _blocking_shell_effect("step", indent="      ")
        return _Scenario(
            orchestration=(
                "interface:\n  inputs:\n    items: {default: [0]}\n"
                "effects:\n  - type: loop\n    name: lp\n"
                "    each: {in: input.items, as: x}\n    body:\n"
                f"{step}"
            ),
            child=None, config=_PLAIN_CONFIG, blocking_name="step",
        )
    if name == "tree_loop":
        step = _blocking_shell_effect("step", indent="      ")
        return _Scenario(
            orchestration=(
                "interface:\n  inputs:\n    items: {default: [0, 1]}\n"
                "effects:\n  - type: loop\n    name: lp\n    flow: tree\n"
                "    each: {in: input.items, as: x}\n    body:\n"
                f"{step}"
            ),
            child=None, config=_PLAIN_CONFIG, blocking_name="step",
        )
    if name == "tree_dynamic":
        fast = _quick_step("fast", indent="      ")
        slow = _blocking_shell_effect("slow", indent="      ")
        return _Scenario(
            orchestration=(
                "effects:\n  - type: dynamic\n    name: outer\n    flow: tree\n"
                "    effects:\n"
                f"{fast}{slow}"
            ),
            child=None, config=_PLAIN_CONFIG, blocking_name="slow",
        )
    if name == "use_record_children":
        quick = _quick_step("quick", indent="  ")
        blocking = _blocking_shell_effect("blocking", indent="  ")
        return _Scenario(
            orchestration="effects:\n  - type: use\n    name: step\n    path: child.yml\n",
            child=f"effects:\n{quick}{blocking}",
            config=_RECORD_CHILDREN_CONFIG, blocking_name="blocking",
            wait_mode="marker_file",
        )
    if name == "use_restart":
        quick = _quick_step("quick", indent="  ")
        blocking = _blocking_shell_effect("blocking", indent="  ")
        return _Scenario(
            orchestration=(
                "effects:\n  - type: use\n    name: step\n    resume: restart\n"
                "    path: child.yml\n"
            ),
            child=f"effects:\n{quick}{blocking}",
            config=_RECORD_CHILDREN_CONFIG, blocking_name="blocking",
            wait_mode="marker_file",
        )
    if name == "named_if":
        branch = _blocking_shell_effect("step", indent="      ")
        return _Scenario(
            orchestration=(
                "effects:\n  - type: if\n    name: gate\n"
                "    if: {mode: cel, expr: \"true\"}\n    then:\n"
                f"{branch}"
            ),
            child=None, config=_PLAIN_CONFIG, blocking_name="step",
        )
    if name == "finally":
        body = _quick_step("body", indent="      ")
        cleanup = _failing_shell_effect("cleanup", indent="      ")
        return _Scenario(
            orchestration=(
                "effects:\n  - type: dynamic\n    name: outer\n"
                f"    effects:\n{body}    finally:\n{cleanup}"
            ),
            child=None, config=_PLAIN_CONFIG, blocking_name="cleanup",
            interrupt_mode="fail_first_time",
        )
    _SCRIPTED_CONFIG = (
        '{"runtime": {"adapters": {"scripted": {"replies_file": '
        '"scripted-replies.yaml"}}}}'
    )
    if name == "reflector_finished":
        # issue #585: a finished reflector is skipped whole on resume --
        # the scripted replies file provides exactly one reply, so a
        # resumed run that incorrectly re-invoked the planner would run
        # out of replies and fail, instead of this scenario's own
        # `later` step (deterministic, no signal) finally succeeding.
        later = _failing_shell_effect("later", indent="  ")
        return _Scenario(
            orchestration=(
                'adapter: scripted\nmodel: test\n'
                "effects:\n  - type: reflector\n    name: plan_it\n"
                '    prime_template: "Plan it."\n    effects:\n'
                "      - type: prompt\n        name: propose_steps\n"
                '        template: "propose steps"\n'
                f"{later}"
            ),
            child=None, config=_SCRIPTED_CONFIG, blocking_name="later",
            interrupt_mode="fail_first_time",
            replies_file=(
                "prime.plan_it.inner.propose_steps:\n  - text: |\n      done: true\n"
            ),
        )
    assert name == "reflector_interrupted"
    # issue #585: a reflector interrupted mid-plan reruns from its own
    # start on resume -- the scripted replies file provides two
    # identical replies, one for the interrupted attempt's own
    # planning call and one for the resumed attempt's fresh one (an
    # unfinished reflector never continues inside a generated plan).
    reply = (
        "  - text: |\n"
        "      done: false\n"
        "      effects:\n"
        "        - type: tool\n"
        "          name: step\n"
        "          provider: shell\n"
        "          params:\n"
        "            command: bash\n"
        '            args: ["-c", "[ -f proceed ] || { touch started; exec sleep 60; }"]\n'
        '            allowed_commands: ["bash"]\n'
    )
    return _Scenario(
        orchestration=(
            'adapter: scripted\nmodel: test\n'
            "effects:\n  - type: reflector\n    name: plan_it\n"
            '    prime_template: "Plan it."\n    effects:\n'
            "      - type: prompt\n        name: propose_steps\n"
            '        template: "propose steps"\n'
        ),
        child=None, config=_SCRIPTED_CONFIG, blocking_name="step",
        replies_file=f"prime.plan_it.inner.propose_steps:\n{reply}{reply}",
    )


SCENARIOS = (
    "chain_loop", "tree_loop", "tree_dynamic", "use_record_children", "use_restart",
    "named_if", "finally", "reflector_finished", "reflector_interrupted",
)


def _write_case(scenario_name: str, case_dir: Path) -> _Scenario:
    scenario = _scenario(scenario_name)
    case_dir.mkdir(parents=True)
    _write(case_dir / "config.json", scenario.config)
    _write(case_dir / "orchestration.yml", scenario.orchestration)
    if scenario.child is not None:
        _write(case_dir / "child.yml", scenario.child)
    if scenario.replies_file is not None:
        _write(case_dir / "scripted-replies.yaml", scenario.replies_file)
    return scenario


@pytest.mark.parametrize("scenario_name", SCENARIOS)
@pytest.mark.parametrize("interrupt_engine", ENGINES)
@pytest.mark.parametrize("resume_engine", ENGINES)
def test_a_state_file_one_engine_wrote_resumes_on_the_other_with_the_same_final_state(
    tmp_path: Path, electricity_binary: Path, scenario_name: str, interrupt_engine: str,
    resume_engine: str,
) -> None:
    env = _sandboxed_env(tmp_path)

    baseline_dir = tmp_path / "baseline"
    _write_case(scenario_name, baseline_dir)
    (baseline_dir / "proceed").write_text("")
    baseline = _run_to_completion(
        resume_engine, electricity_binary,
        [
            "orchestration.yml", "--config", "config.json", "--out", "out.json",
            "--events", "events.jsonl",
        ],
        cwd=baseline_dir, env=env,
    )
    assert baseline.returncode == 0, (baseline.stdout, baseline.stderr)
    baseline_state = json.loads((baseline_dir / "out.json").read_text(encoding="utf-8"))

    case_dir = tmp_path / "case"
    scenario = _write_case(scenario_name, case_dir)

    if scenario.interrupt_mode == "fail_first_time":
        interrupted = _run_to_completion(
            interrupt_engine, electricity_binary,
            ["orchestration.yml", "--config", "config.json", "--out", "out.json"],
            cwd=case_dir, env=env,
        )
        assert interrupted.returncode == 1, (
            interrupt_engine, interrupted.stdout, interrupted.stderr,
        )
    else:
        live_state_path = case_dir / "live.json"
        proc = _popen(
            interrupt_engine, electricity_binary,
            [
                "orchestration.yml", "--config", "config.json", "--out", "out.json",
                "--live-state", "live.json",
            ],
            cwd=case_dir, env=env,
        )
        if scenario.wait_mode == "marker_file":
            _wait_for_marker_file(proc, case_dir / "started")
        else:
            _wait_for_step_started(proc, live_state_path, scenario.blocking_name)
        proc.send_signal(signal.SIGINT)
        try:
            interrupted_stdout, interrupted_stderr = proc.communicate(timeout=20.0)
        except subprocess.TimeoutExpired:
            _diagnose_and_fail(proc, timeout=20.0, label="waiting for SIGINT to stop the run")
            interrupted_stdout, interrupted_stderr = "", ""  # unreachable
        assert proc.returncode == 130, (interrupt_engine, interrupted_stdout, interrupted_stderr)
    assert (case_dir / "out.json").exists()

    (case_dir / "proceed").write_text("")
    resumed = _run_to_completion(
        resume_engine, electricity_binary,
        [
            "orchestration.yml", "--config", "config.json", "--out", "out.json",
            "--state", "out.json", "--resume", "state", "--force",
            "--events", "events.jsonl",
        ],
        cwd=case_dir, env=env,
    )
    assert resumed.returncode == 0, (resumed.stdout, resumed.stderr)
    resumed_state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))

    # `runtime.last_run.totals.effects_run` is a per-*process* counter,
    # not merged with whatever an earlier, interrupted attempt already
    # ran -- a resumed run that skips something a from-scratch baseline
    # didn't legitimately counts fewer effects, on cof as much as on
    # electricity. Dropped from both sides before comparing (uniformly,
    # not only for a scenario that actually skips something): the rest
    # of `totals` (`wall_time_s`) is already normalized away as a
    # `duration` by `normalize()` itself.
    for state in (resumed_state, baseline_state):
        totals = state.get("runtime", {}).get("last_run", {}).get("totals")
        if isinstance(totals, dict):
            totals.pop("effects_run", None)

    # No `replacements` needed here, unlike most of this suite's own
    # cross-directory comparisons: every marker filename in the
    # orchestration text is bare (`_blocking_shell_effect`'s own doc
    # comment), so nothing in either state leaks its own case directory.
    assert_states_equal(
        prepare_for_comparison(resumed_state, []),
        prepare_for_comparison(baseline_state, []),
    )

    if scenario_name in _EXPECT_IDENTICAL_EVENTS:
        assert_events_equal(
            (case_dir / "events.jsonl").read_text(encoding="utf-8"),
            (baseline_dir / "events.jsonl").read_text(encoding="utf-8"),
        )
    else:
        # Not identical by design (this module's own docstring): a step
        # `record_children`/the positional continue rule skipped on
        # resume never refires its own events. Still has to be a
        # well-formed, non-empty stream.
        resumed_events = (case_dir / "events.jsonl").read_text(encoding="utf-8")
        assert resumed_events.strip(), "the resumed run wrote no events at all"
        for line in resumed_events.splitlines():
            json.loads(line)


@pytest.mark.parametrize("resume_engine", ENGINES)
@pytest.mark.parametrize("interrupt_engine", ENGINES)
def test_a_use_childs_interrupted_step_runs_again_only_when_it_never_finished(
    tmp_path: Path, electricity_binary: Path, interrupt_engine: str, resume_engine: str,
) -> None:
    """issue #727 follow-up: an interrupted `use` child's own finished
    steps stay finished across `--resume`, never rerun just because the
    interrupting engine's own saved state lost their record. s1 (quick)
    must run exactly once, s2 (blocking, interrupted mid-step) exactly
    twice (once before the interrupt, once on resume, since it never
    finished the first time), s3 (quick, never reached before the
    interrupt) exactly once -- on every pair of `{cof, electricity}` as
    the interrupting and the resuming engine. A log, not the final
    state, is what tells a correctly skipped `s1` apart from one
    rerun from scratch: both leave the same value in `--out`.
    """
    env = _sandboxed_env(tmp_path)
    case_dir = tmp_path / "case"
    case_dir.mkdir()
    log_name = "steplog.txt"
    _write(case_dir / "child.yml", _step_log_child_orchestration(log=log_name))
    _write(
        case_dir / "orchestration.yml",
        "effects:\n  - type: use\n    name: step\n    path: child.yml\n",
    )
    _write(case_dir / "config.json", _RECORD_CHILDREN_CONFIG)

    proc = _popen(
        interrupt_engine, electricity_binary,
        [
            "orchestration.yml", "--config", "config.json", "--out", "out.json",
            "--live-state", "live.json",
        ],
        cwd=case_dir, env=env,
    )
    _wait_for_marker_file(proc, case_dir / "started")
    proc.send_signal(signal.SIGINT)
    try:
        interrupted_stdout, interrupted_stderr = proc.communicate(timeout=20.0)
    except subprocess.TimeoutExpired:
        _diagnose_and_fail(proc, timeout=20.0, label="waiting for SIGINT to stop the run")
        interrupted_stdout, interrupted_stderr = "", ""  # unreachable
    assert proc.returncode == 130, (interrupt_engine, interrupted_stdout, interrupted_stderr)
    assert (case_dir / "out.json").exists()

    (case_dir / "proceed").write_text("")
    resumed = _run_to_completion(
        resume_engine, electricity_binary,
        [
            "orchestration.yml", "--config", "config.json", "--out", "out.json",
            "--state", "out.json", "--resume", "state", "--force",
        ],
        cwd=case_dir, env=env,
    )
    assert resumed.returncode == 0, (resumed.stdout, resumed.stderr)

    log_lines = (case_dir / log_name).read_text(encoding="utf-8").splitlines()
    counts = {name: log_lines.count(name) for name in ("s1", "s2", "s3")}
    assert counts == {"s1": 1, "s2": 2, "s3": 1}, (interrupt_engine, resume_engine, log_lines)


#: issue #584: a top-level `use` reading a declared `interface.inputs`
#: value, and a tool that fails unless `proceed` already exists on
#: disk -- the same deterministic, signal-free interrupt `finally`'s
#: own scenario uses (this module's own docstring on why).
_CHANGED_INPUTS_ORCHESTRATION = (
    "interface:\n  inputs:\n    x: {type: string, default: a}\n"
    "effects:\n  - type: use\n    name: part\n    path: child.yml\n"
    "    inputs: {x: {from: input.x}}\n"
)
_CHANGED_INPUTS_CHILD = (
    "interface:\n  inputs:\n    x: {type: string, required: true}\n"
    "effects:\n  - type: tool\n    name: noop\n    provider: json\n"
    "    params: {mode: parse, input: '{\"ok\": true}'}\n"
)


def _write_changed_inputs_case(case_dir: Path) -> None:
    case_dir.mkdir(parents=True)
    _write(case_dir / "config.json", _PLAIN_CONFIG)
    _write(case_dir / "child.yml", _CHANGED_INPUTS_CHILD)
    _write(
        case_dir / "orchestration.yml",
        _CHANGED_INPUTS_ORCHESTRATION
        + "  - type: tool\n    name: later\n    provider: shell\n    params:\n"
        '      command: bash\n      args: ["-c", "[ -f proceed ]"]\n'
        '      allowed_commands: ["bash"]\n',
    )


_RESUMED_INPUT_WARNING = (
    "WARNING: Resuming with changed input(s): x: 'a' -> 'b'. "
    "Other finished effects keep their saved results."
)


@pytest.mark.parametrize("resume_engine", ENGINES)
@pytest.mark.parametrize("interrupt_engine", ENGINES)
def test_a_finished_use_whose_resolved_inputs_changed_reruns_on_resume(
    tmp_path: Path, electricity_binary: Path, interrupt_engine: str, resume_engine: str,
) -> None:
    """issue #584: `part`'s own `meta.inputs` records whatever `input.x`
    resolved to when it ran. A first run with `-e x=a` finishes `part`
    and then fails at `later` (deterministic, no signal needed -- same
    technique as the `finally` scenario above). Resumed with `-e x=b`,
    `part`'s own inputs no longer match what it ran with, so it reruns
    (a fresh `meta.created_at`, `meta.inputs.x: "b"`) and the run warns,
    on both engines, with the exact same text. Resumed instead with the
    unchanged `-e x=a`, `part` is skipped whole (same `meta.created_at`
    as the first run) and nothing warns.
    """
    env = _sandboxed_env(tmp_path)

    case_dir = tmp_path / "case"
    _write_changed_inputs_case(case_dir)
    first = _run_to_completion(
        interrupt_engine, electricity_binary,
        ["orchestration.yml", "--config", "config.json", "--out", "out.json", "-e", "x=a"],
        cwd=case_dir, env=env,
    )
    assert first.returncode == 1, (interrupt_engine, first.stdout, first.stderr)
    first_state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))
    first_created_at = first_state["prime"]["part"]["meta"]["created_at"]
    assert first_state["prime"]["part"]["meta"]["inputs"] == {"x": "a"}

    # Changed: `part` reruns, and the run warns.
    (case_dir / "proceed").write_text("")
    changed = _run_to_completion(
        resume_engine, electricity_binary,
        [
            "orchestration.yml", "--config", "config.json", "--out", "out.json",
            "--state", "out.json", "--resume", "state", "--force", "-e", "x=b",
        ],
        cwd=case_dir, env=env,
    )
    assert changed.returncode == 0, (resume_engine, changed.stdout, changed.stderr)
    assert _RESUMED_INPUT_WARNING in changed.stderr, (resume_engine, changed.stderr)
    changed_state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))
    assert changed_state["prime"]["part"]["meta"]["inputs"] == {"x": "b"}
    assert changed_state["prime"]["part"]["meta"]["created_at"] != first_created_at

    # Unchanged (a fresh case, same first run): `part` is skipped whole,
    # and nothing warns.
    unchanged_dir = tmp_path / "unchanged"
    _write_changed_inputs_case(unchanged_dir)
    first_unchanged = _run_to_completion(
        interrupt_engine, electricity_binary,
        ["orchestration.yml", "--config", "config.json", "--out", "out.json", "-e", "x=a"],
        cwd=unchanged_dir, env=env,
    )
    assert first_unchanged.returncode == 1, (
        interrupt_engine, first_unchanged.stdout, first_unchanged.stderr,
    )
    first_unchanged_state = json.loads(
        (unchanged_dir / "out.json").read_text(encoding="utf-8")
    )
    first_unchanged_created_at = first_unchanged_state["prime"]["part"]["meta"]["created_at"]
    (unchanged_dir / "proceed").write_text("")
    unchanged = _run_to_completion(
        resume_engine, electricity_binary,
        [
            "orchestration.yml", "--config", "config.json", "--out", "out.json",
            "--state", "out.json", "--resume", "state", "--force", "-e", "x=a",
        ],
        cwd=unchanged_dir, env=env,
    )
    assert unchanged.returncode == 0, (resume_engine, unchanged.stdout, unchanged.stderr)
    assert "Resuming with changed input(s)" not in unchanged.stderr, (
        resume_engine, unchanged.stderr,
    )
    unchanged_state = json.loads((unchanged_dir / "out.json").read_text(encoding="utf-8"))
    assert unchanged_state["prime"]["part"]["meta"]["inputs"] == {"x": "a"}
    assert (
        unchanged_state["prime"]["part"]["meta"]["created_at"]
        == first_unchanged_created_at
    )


#: issue #584: a `use` whose own `after` sibling in the same chain
#: finished *successfully* the first time -- unlike `later` above,
#: which always fails the first run regardless of whether the fix
#: works, `after` here is the `uuid` tool (never cof's `CountingAdapter`
#: reply, never electricity's own `uuid` provider mock: both engines
#: ship a real `uuid` tool), so a changed `value` after `--resume`
#: proves it actually reran, not merely that it failed before.
def _write_use_followed_by_uuid_case(case_dir: Path) -> None:
    case_dir.mkdir(parents=True)
    _write(case_dir / "config.json", _PLAIN_CONFIG)
    _write(case_dir / "child.yml", _CHANGED_INPUTS_CHILD)
    _write(
        case_dir / "orchestration.yml",
        _CHANGED_INPUTS_ORCHESTRATION
        + "  - type: tool\n    name: after\n    provider: uuid\n",
    )


@pytest.mark.parametrize("resume_engine", ENGINES)
@pytest.mark.parametrize("first_engine", ENGINES)
def test_a_finished_step_after_a_changed_use_reruns_too(
    tmp_path: Path, electricity_binary: Path, first_engine: str, resume_engine: str,
) -> None:
    """issue #584: `after` already finished *successfully* on the first
    run (never failed the way `later` elsewhere in this module does),
    so only `part`'s own changed inputs -- never a prior failure -- can
    explain a fresh `after` value on resume. Proves the positional
    "everything after it reruns too" rule on a step that has nothing
    else driving it to rerun, on every engine pair.
    """
    env = _sandboxed_env(tmp_path)
    case_dir = tmp_path / "case"
    _write_use_followed_by_uuid_case(case_dir)

    first = _run_to_completion(
        first_engine, electricity_binary,
        ["orchestration.yml", "--config", "config.json", "--out", "out.json", "-e", "x=a"],
        cwd=case_dir, env=env,
    )
    assert first.returncode == 0, (first_engine, first.stdout, first.stderr)
    first_state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))
    first_after = first_state["prime"]["after"]["value"]

    changed = _run_to_completion(
        resume_engine, electricity_binary,
        [
            "orchestration.yml", "--config", "config.json", "--out", "out.json",
            "--state", "out.json", "--resume", "state", "--force", "-e", "x=b",
        ],
        cwd=case_dir, env=env,
    )
    assert changed.returncode == 0, (resume_engine, changed.stdout, changed.stderr)
    assert _RESUMED_INPUT_WARNING in changed.stderr, (resume_engine, changed.stderr)
    changed_state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))
    assert changed_state["prime"]["part"]["meta"]["inputs"] == {"x": "b"}
    assert changed_state["prime"]["after"]["value"] != first_after, (
        first_engine, resume_engine, "the step after the changed use did not rerun"
    )

    # Unchanged: neither reruns.
    unchanged_dir = tmp_path / "unchanged"
    _write_use_followed_by_uuid_case(unchanged_dir)
    first_unchanged = _run_to_completion(
        first_engine, electricity_binary,
        ["orchestration.yml", "--config", "config.json", "--out", "out.json", "-e", "x=a"],
        cwd=unchanged_dir, env=env,
    )
    assert first_unchanged.returncode == 0, (
        first_engine, first_unchanged.stdout, first_unchanged.stderr,
    )
    first_unchanged_state = json.loads(
        (unchanged_dir / "out.json").read_text(encoding="utf-8")
    )
    unchanged = _run_to_completion(
        resume_engine, electricity_binary,
        [
            "orchestration.yml", "--config", "config.json", "--out", "out.json",
            "--state", "out.json", "--resume", "state", "--force", "-e", "x=a",
        ],
        cwd=unchanged_dir, env=env,
    )
    assert unchanged.returncode == 0, (resume_engine, unchanged.stdout, unchanged.stderr)
    assert "Resuming with changed input(s)" not in unchanged.stderr, (
        resume_engine, unchanged.stderr,
    )
    unchanged_state = json.loads((unchanged_dir / "out.json").read_text(encoding="utf-8"))
    assert (
        unchanged_state["prime"]["after"]["value"]
        == first_unchanged_state["prime"]["after"]["value"]
    )


#: issue #584: a `use` inside a named chain-flow loop's own single
#: pass, followed by a step that fails unless `proceed` already
#: exists -- the same deterministic, signal-free interrupt technique
#: `_failing_shell_effect` already gives this module, needed here so
#: the loop's own top node (and so the whole loop) isn't itself
#: `effect_completed_ok` after the first run, which would skip the
#: loop whole on resume and never descend into the pass body at all.
def _write_use_in_loop_pass_case(case_dir: Path) -> None:
    case_dir.mkdir(parents=True)
    _write(case_dir / "config.json", _PLAIN_CONFIG)
    _write(case_dir / "child.yml", _CHANGED_INPUTS_CHILD)
    after = _failing_shell_effect("after", indent="      ")
    _write(
        case_dir / "orchestration.yml",
        "interface:\n  inputs:\n    x: {type: string, default: a}\n    items: {default: [0]}\n"
        "effects:\n  - type: loop\n    name: lp\n"
        "    each: {in: input.items, as: item}\n    body:\n"
        "      - type: use\n        name: part\n        path: child.yml\n"
        "        inputs: {x: {from: input.x}}\n"
        f"{after}",
    )


@pytest.mark.parametrize("resume_engine", ENGINES)
@pytest.mark.parametrize("first_engine", ENGINES)
def test_a_use_inside_a_named_loop_pass_whose_inputs_changed_reruns_along_with_what_follows(
    tmp_path: Path, electricity_binary: Path, first_engine: str, resume_engine: str,
) -> None:
    """issue #584: a `use` that is the first effect of a named loop's
    own unfinished pass gets the same changed-inputs check a top-level
    chain step does -- `part`'s own recorded inputs no longer match,
    so it (and `after`, right after it in the same pass) reruns.
    """
    env = _sandboxed_env(tmp_path)
    case_dir = tmp_path / "case"
    _write_use_in_loop_pass_case(case_dir)

    first = _run_to_completion(
        first_engine, electricity_binary,
        ["orchestration.yml", "--config", "config.json", "--out", "out.json", "-e", "x=a"],
        cwd=case_dir, env=env,
    )
    assert first.returncode == 1, (first_engine, first.stdout, first.stderr)
    first_state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))
    assert first_state["prime"]["lp"]["iter_0"]["part"]["meta"]["inputs"] == {"x": "a"}

    (case_dir / "proceed").write_text("")
    changed = _run_to_completion(
        resume_engine, electricity_binary,
        [
            "orchestration.yml", "--config", "config.json", "--out", "out.json",
            "--state", "out.json", "--resume", "state", "--force", "-e", "x=b",
        ],
        cwd=case_dir, env=env,
    )
    assert changed.returncode == 0, (resume_engine, changed.stdout, changed.stderr)
    assert _RESUMED_INPUT_WARNING in changed.stderr, (resume_engine, changed.stderr)
    changed_state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))
    iter_0 = changed_state["prime"]["lp"]["iter_0"]
    assert iter_0["part"]["meta"]["inputs"] == {"x": "b"}, (first_engine, resume_engine)
    assert iter_0["after"]["meta"]["exit_code"] == 0

    # Unchanged: the pass is skipped whole once the first run actually
    # finishes (unlike above, nothing here left it unfinished).
    unchanged_dir = tmp_path / "unchanged"
    _write_use_in_loop_pass_case(unchanged_dir)
    (unchanged_dir / "proceed").write_text("")
    first_unchanged = _run_to_completion(
        first_engine, electricity_binary,
        ["orchestration.yml", "--config", "config.json", "--out", "out.json", "-e", "x=a"],
        cwd=unchanged_dir, env=env,
    )
    assert first_unchanged.returncode == 0, (
        first_engine, first_unchanged.stdout, first_unchanged.stderr,
    )
    first_unchanged_state = json.loads(
        (unchanged_dir / "out.json").read_text(encoding="utf-8")
    )
    first_created_at = first_unchanged_state["prime"]["lp"]["iter_0"]["part"]["meta"][
        "created_at"
    ]
    unchanged = _run_to_completion(
        resume_engine, electricity_binary,
        [
            "orchestration.yml", "--config", "config.json", "--out", "out.json",
            "--state", "out.json", "--resume", "state", "--force", "-e", "x=a",
        ],
        cwd=unchanged_dir, env=env,
    )
    assert unchanged.returncode == 0, (resume_engine, unchanged.stdout, unchanged.stderr)
    assert "Resuming with changed input(s)" not in unchanged.stderr, (
        resume_engine, unchanged.stderr,
    )
    unchanged_state = json.loads((unchanged_dir / "out.json").read_text(encoding="utf-8"))
    assert (
        unchanged_state["prime"]["lp"]["iter_0"]["part"]["meta"]["created_at"]
        == first_created_at
    )


#: issue #584: a `use` inside a named `if`'s own branch, followed by a
#: step that fails unless `proceed` already exists -- same technique
#: and same reason as the loop-pass case above (the `if`'s own node
#: must not be `effect_completed_ok` after the first run).
def _write_use_in_if_branch_case(case_dir: Path) -> None:
    case_dir.mkdir(parents=True)
    _write(case_dir / "config.json", _PLAIN_CONFIG)
    _write(case_dir / "child.yml", _CHANGED_INPUTS_CHILD)
    after = _failing_shell_effect("after", indent="      ")
    _write(
        case_dir / "orchestration.yml",
        "interface:\n  inputs:\n    x: {type: string, default: a}\n"
        "effects:\n  - type: if\n    name: gate\n"
        "    if: {mode: cel, expr: \"true\"}\n    then:\n"
        "      - type: use\n        name: part\n        path: child.yml\n"
        "        inputs: {x: {from: input.x}}\n"
        f"{after}",
    )


@pytest.mark.parametrize("resume_engine", ENGINES)
@pytest.mark.parametrize("first_engine", ENGINES)
def test_a_use_inside_a_named_if_branch_whose_inputs_changed_reruns_along_with_what_follows(
    tmp_path: Path, electricity_binary: Path, first_engine: str, resume_engine: str,
) -> None:
    """issue #584: a `use` that is the first effect of a named `if`'s
    own unfinished branch gets the same changed-inputs check a
    top-level chain step does.
    """
    env = _sandboxed_env(tmp_path)
    case_dir = tmp_path / "case"
    _write_use_in_if_branch_case(case_dir)

    first = _run_to_completion(
        first_engine, electricity_binary,
        ["orchestration.yml", "--config", "config.json", "--out", "out.json", "-e", "x=a"],
        cwd=case_dir, env=env,
    )
    assert first.returncode == 1, (first_engine, first.stdout, first.stderr)
    first_state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))
    assert first_state["prime"]["gate"]["part"]["meta"]["inputs"] == {"x": "a"}

    (case_dir / "proceed").write_text("")
    changed = _run_to_completion(
        resume_engine, electricity_binary,
        [
            "orchestration.yml", "--config", "config.json", "--out", "out.json",
            "--state", "out.json", "--resume", "state", "--force", "-e", "x=b",
        ],
        cwd=case_dir, env=env,
    )
    assert changed.returncode == 0, (resume_engine, changed.stdout, changed.stderr)
    assert _RESUMED_INPUT_WARNING in changed.stderr, (resume_engine, changed.stderr)
    changed_state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))
    gate = changed_state["prime"]["gate"]
    assert gate["part"]["meta"]["inputs"] == {"x": "b"}, (first_engine, resume_engine)
    assert gate["after"]["meta"]["exit_code"] == 0

    # Unchanged: the branch is skipped whole once the first run finishes.
    unchanged_dir = tmp_path / "unchanged"
    _write_use_in_if_branch_case(unchanged_dir)
    (unchanged_dir / "proceed").write_text("")
    first_unchanged = _run_to_completion(
        first_engine, electricity_binary,
        ["orchestration.yml", "--config", "config.json", "--out", "out.json", "-e", "x=a"],
        cwd=unchanged_dir, env=env,
    )
    assert first_unchanged.returncode == 0, (
        first_engine, first_unchanged.stdout, first_unchanged.stderr,
    )
    first_unchanged_state = json.loads(
        (unchanged_dir / "out.json").read_text(encoding="utf-8")
    )
    first_created_at = first_unchanged_state["prime"]["gate"]["part"]["meta"]["created_at"]
    unchanged = _run_to_completion(
        resume_engine, electricity_binary,
        [
            "orchestration.yml", "--config", "config.json", "--out", "out.json",
            "--state", "out.json", "--resume", "state", "--force", "-e", "x=a",
        ],
        cwd=unchanged_dir, env=env,
    )
    assert unchanged.returncode == 0, (resume_engine, unchanged.stdout, unchanged.stderr)
    assert "Resuming with changed input(s)" not in unchanged.stderr, (
        resume_engine, unchanged.stderr,
    )
    unchanged_state = json.loads((unchanged_dir / "out.json").read_text(encoding="utf-8"))
    assert (
        unchanged_state["prime"]["gate"]["part"]["meta"]["created_at"]
        == first_created_at
    )


#: issue #584: a tree-flow `dynamic` whose only branch is a `use`
#: reading a declared run input -- its own skip check is independent
#: of the chain's positional one, so a changed run input must never
#: rerun it.
def _write_tree_dynamic_use_branch_case(case_dir: Path) -> None:
    case_dir.mkdir(parents=True)
    _write(case_dir / "config.json", _PLAIN_CONFIG)
    _write(case_dir / "child.yml", _CHANGED_INPUTS_CHILD)
    _write(
        case_dir / "orchestration.yml",
        "interface:\n  inputs:\n    x: {type: string, default: a}\n"
        "effects:\n  - type: dynamic\n    name: outer\n    flow: tree\n"
        "    effects:\n      - type: use\n        name: part\n"
        "        path: child.yml\n        inputs: {x: {from: input.x}}\n",
    )


@pytest.mark.parametrize("resume_engine", ENGINES)
@pytest.mark.parametrize("first_engine", ENGINES)
def test_a_tree_dynamics_use_branch_stays_skipped_despite_changed_run_inputs(
    tmp_path: Path, electricity_binary: Path, first_engine: str, resume_engine: str,
) -> None:
    """issue #584: unaffected by design -- a tree-flow branch has its
    own, independent skip check, never the chain's positional one a
    top-level or named-pass/branch `use` gets.
    """
    env = _sandboxed_env(tmp_path)
    case_dir = tmp_path / "case"
    _write_tree_dynamic_use_branch_case(case_dir)

    first = _run_to_completion(
        first_engine, electricity_binary,
        ["orchestration.yml", "--config", "config.json", "--out", "out.json", "-e", "x=a"],
        cwd=case_dir, env=env,
    )
    assert first.returncode == 0, (first_engine, first.stdout, first.stderr)
    first_state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))
    first_created_at = first_state["prime"]["outer"]["part"]["meta"]["created_at"]
    assert first_state["prime"]["outer"]["part"]["meta"]["inputs"] == {"x": "a"}

    resumed = _run_to_completion(
        resume_engine, electricity_binary,
        [
            "orchestration.yml", "--config", "config.json", "--out", "out.json",
            "--state", "out.json", "--resume", "state", "--force", "-e", "x=b",
        ],
        cwd=case_dir, env=env,
    )
    assert resumed.returncode == 0, (resume_engine, resumed.stdout, resumed.stderr)
    assert _RESUMED_INPUT_WARNING.replace("'a' -> 'b'", "'a' -> 'b'") in resumed.stderr
    resumed_state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))
    part = resumed_state["prime"]["outer"]["part"]
    assert part["meta"]["inputs"] == {"x": "a"}, (
        first_engine, resume_engine, "a tree branch must stay skipped despite changed inputs"
    )
    assert part["meta"]["created_at"] == first_created_at, (first_engine, resume_engine)


#: issue #584: two declared inputs plus one undeclared -- pins the
#: warning's own ordering rule (declared, in interface order, then
#: undeclared, sorted) across every engine pair, never with only one
#: input to order.
_ORDER_ORCHESTRATION = (
    "interface:\n  inputs:\n    first: {type: string, default: a}\n"
    "    second: {type: string, default: a}\n"
    "effects:\n  - type: tool\n    name: noop\n    provider: uuid\n"
)
_ORDER_WARNING = (
    "WARNING: Resuming with changed input(s): first: 'a' -> 'b', second: 'a' -> 'b', "
    "zzz: 'a' -> 'b'. Other finished effects keep their saved results."
)


@pytest.mark.parametrize("resume_engine", ENGINES)
@pytest.mark.parametrize("first_engine", ENGINES)
def test_the_resume_warning_orders_declared_inputs_then_undeclared_sorted(
    tmp_path: Path, electricity_binary: Path, first_engine: str, resume_engine: str,
) -> None:
    """issue #584: `first`/`second` are declared, in that order; `zzz`
    is not declared at all -- the warning names `first`, then `second`,
    then `zzz`, on the exact same text on both engines, regardless of
    the order the matching `-e` flags were given in on either run.
    """
    env = _sandboxed_env(tmp_path)
    case_dir = tmp_path / "case"
    case_dir.mkdir(parents=True)
    _write(case_dir / "config.json", _PLAIN_CONFIG)
    _write(case_dir / "orchestration.yml", _ORDER_ORCHESTRATION)

    first = _run_to_completion(
        first_engine, electricity_binary,
        [
            "orchestration.yml", "--config", "config.json", "--out", "out.json",
            "-e", "first=a", "-e", "second=a", "-e", "zzz=a",
        ],
        cwd=case_dir, env=env,
    )
    assert first.returncode == 0, (first_engine, first.stdout, first.stderr)

    resumed = _run_to_completion(
        resume_engine, electricity_binary,
        [
            "orchestration.yml", "--config", "config.json", "--out", "out.json",
            "--state", "out.json", "--resume", "state", "--force",
            "-e", "zzz=b", "-e", "second=b", "-e", "first=b",
        ],
        cwd=case_dir, env=env,
    )
    assert resumed.returncode == 0, (resume_engine, resumed.stdout, resumed.stderr)
    assert _ORDER_WARNING in resumed.stderr, (first_engine, resume_engine, resumed.stderr)
