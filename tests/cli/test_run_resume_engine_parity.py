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
the lane's own other deterministic-interrupt option instead: a step
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
(the lane's own stretch past #727's bare acceptance criteria) is
established two ways: directly, by comparing the resumed run's final
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
import shutil
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

ELECTRICITY_DIR = Path(__file__).resolve().parents[2] / "electricity"

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
_EXPECT_IDENTICAL_EVENTS = frozenset({"chain_loop", "tree_loop", "named_if", "use_restart"})


@pytest.fixture(scope="module")
def electricity_binary() -> Path:
    """Builds the `electricity` binary once for this module -- same
    pattern as `conformance/test_run_cli_parity.py`'s own fixture."""
    if shutil.which("cargo") is None:
        pytest.skip("cargo not available; electricity binary cannot be built")
    proc = subprocess.run(
        ["cargo", "build", "-p", "electricity-cli", "--bin", "electricity"],
        cwd=ELECTRICITY_DIR, env=harness.cargo_env(), capture_output=True, text=True,
        timeout=600, check=False,
    )
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p electricity-cli failed:\n{proc.stdout}\n{proc.stderr}")
    binary = ELECTRICITY_DIR / "target" / "debug" / "electricity"
    assert binary.exists(), f"cargo build succeeded but {binary} is missing"
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


class _Scenario:
    def __init__(
        self, *, orchestration: str, child: str | None, config: str, blocking_name: str,
        wait_mode: str = "live_state", interrupt_mode: str = "signal",
    ) -> None:
        self.orchestration = orchestration
        self.child = child
        self.config = config
        self.blocking_name = blocking_name
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
    assert name == "finally"
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


SCENARIOS = (
    "chain_loop", "tree_loop", "tree_dynamic", "use_record_children", "use_restart",
    "named_if", "finally",
)


def _write_case(scenario_name: str, case_dir: Path) -> _Scenario:
    scenario = _scenario(scenario_name)
    case_dir.mkdir(parents=True)
    _write(case_dir / "config.json", scenario.config)
    _write(case_dir / "orchestration.yml", scenario.orchestration)
    if scenario.child is not None:
        _write(case_dir / "child.yml", scenario.child)
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
