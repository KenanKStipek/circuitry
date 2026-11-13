"""`--state`/`--resume` across engines (issue #727): a run interrupted by
a real SIGINT under one engine resumes under the other -- inside a named
loop's own pass, inside a `use` child, and inside a `finally:` block --
ending in the same final state a straight-through, uninterrupted run on
the resuming engine would have produced.

Each case blocks on a plain `shell` step until this test sends SIGINT
(deterministic: the step itself writes a `started` marker the moment it
begins, so the signal never races the step's own start), rather than
timing a real subprocess to land the signal mid-step. The same step
checks a `proceed` marker before blocking at all -- absent for the
interrupted run (and the resume that continues it), present up front for
the baseline -- so the *same* document resumes into a step that actually
finishes this time, with no second interrupt needed, and a baseline run
of that already-"proceed"-marked document is the "same as if it had
never been interrupted" reference the resumed run is compared against
(both produced by the engine that resumes -- the interrupting engine's
own on-disk state format never has to match the other engine's byte for
byte, only `hydrate_resumed_state`'s read of it does).
"""

from __future__ import annotations

import json
import shutil
import signal
import subprocess
import sys
from pathlib import Path

import pytest
from _signal_test_support import _diagnose_and_fail, _sandboxed_env, _wait_for_paths

import conformance.harness as harness
from conformance.normalize import assert_states_equal, prepare_for_comparison

ELECTRICITY_DIR = Path(__file__).resolve().parents[2] / "electricity"

ENGINES = ("cof", "electricity")


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


def _blocking_shell_effect(
    name: str, *, pidfile: str, started: str, proceed: str, done: str, indent: str,
) -> str:
    """A `shell` effect that blocks until killed, unless *proceed* already
    exists on disk -- in which case it finishes at once. Bounded (#661):
    killed by the engine's own cancellation handling on SIGINT, the same
    way `tests/cli/test_run_cancel_parallel.py`'s own branch scripts are,
    never an unbounded wait of its own.

    *pidfile*/*started*/*proceed*/*done* are bare filenames, resolved
    against the shell step's own cwd (the engine's own process cwd, with
    no explicit `params.cwd`) -- never an absolute path, so the same
    orchestration text, byte for byte, works from any directory; two
    different case directories running the identical document then also
    have the identical `runtime.last_run.document_hash`."""
    script = (
        f"if [ -f {proceed} ]; then touch {done}; "
        f"else echo $$ > {pidfile}; touch {started}; exec sleep 60; fi"
    )
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


def _loop_orchestration(markers: "_Markers") -> str:
    step = _blocking_shell_effect(
        "step", pidfile="pidfile", started="started",
        proceed="proceed", done="done", indent="      ",
    )
    return (
        "interface:\n  inputs:\n    items: {default: [0]}\n"
        "effects:\n  - type: loop\n    name: lp\n"
        "    each: {in: input.items, as: x}\n    body:\n"
        f"{step}"
    )


def _use_orchestration(markers: "_Markers") -> str:
    # A relative `path:` (resolved against the parent document's own
    # directory) rather than an absolute one -- so this document's text,
    # byte for byte, is identical between the baseline and interrupt/
    # resume case directories, and so is its `document_hash`.
    return "effects:\n  - type: use\n    name: step\n    path: child.yml\n"


def _use_child_orchestration(markers: "_Markers") -> str:
    # The child's own cwd is still the case directory (the engine's own
    # process cwd; a `use` child never changes it) -- bare filenames
    # resolve the same way they do at the parent document's own level.
    inner = _blocking_shell_effect(
        "inner", pidfile="pidfile", started="started",
        proceed="proceed", done="done", indent="  ",
    )
    return f"effects:\n{inner}"


def _finally_orchestration(markers: "_Markers") -> str:
    cleanup = _blocking_shell_effect(
        "cleanup", pidfile="pidfile", started="started",
        proceed="proceed", done="done", indent="      ",
    )
    return (
        "effects:\n  - type: dynamic\n    name: outer\n"
        "    effects:\n      - type: tool\n        name: body\n"
        "        provider: uuid\n    finally:\n"
        f"{cleanup}"
    )


class _Markers:
    """Bare filenames (`_blocking_shell_effect`'s own doc comment on why)
    alongside the real `Path`s this test polls/touches, both rooted at
    the same case directory."""

    def __init__(self, case_dir: Path) -> None:
        self.pidfile = case_dir / "pidfile"
        self.started = case_dir / "started"
        self.proceed = case_dir / "proceed"
        self.done = case_dir / "done"


def _write_case(scenario: str, case_dir: Path) -> _Markers:
    case_dir.mkdir(parents=True)
    _write(case_dir / "config.json", "{}")
    markers = _Markers(case_dir)
    if scenario == "loop":
        _write(case_dir / "orchestration.yml", _loop_orchestration(markers))
    elif scenario == "use":
        _write(case_dir / "child.yml", _use_child_orchestration(markers))
        _write(case_dir / "orchestration.yml", _use_orchestration(markers))
    else:
        assert scenario == "finally"
        _write(case_dir / "orchestration.yml", _finally_orchestration(markers))
    return markers


@pytest.mark.parametrize("scenario", ["loop", "use", "finally"])
@pytest.mark.parametrize("interrupt_engine", ENGINES)
@pytest.mark.parametrize("resume_engine", ENGINES)
def test_a_state_file_one_engine_wrote_resumes_on_the_other_with_the_same_final_state(
    tmp_path: Path, electricity_binary: Path, scenario: str, interrupt_engine: str,
    resume_engine: str,
) -> None:
    env = _sandboxed_env(tmp_path)

    baseline_dir = tmp_path / "baseline"
    baseline_markers = _write_case(scenario, baseline_dir)
    baseline_markers.proceed.write_text("")
    baseline = _run_to_completion(
        resume_engine, electricity_binary,
        ["orchestration.yml", "--config", "config.json", "--out", "out.json"],
        cwd=baseline_dir, env=env,
    )
    assert baseline.returncode == 0, (baseline.stdout, baseline.stderr)
    baseline_state = json.loads((baseline_dir / "out.json").read_text(encoding="utf-8"))

    case_dir = tmp_path / "case"
    markers = _write_case(scenario, case_dir)
    proc = _popen(
        interrupt_engine, electricity_binary,
        ["orchestration.yml", "--config", "config.json", "--out", "out.json"],
        cwd=case_dir, env=env,
    )
    try:
        _wait_for_paths([markers.started])
    except TimeoutError:
        _diagnose_and_fail(
            proc, timeout=20.0, label="waiting for the blocking step to start"
        )
    proc.send_signal(signal.SIGINT)
    try:
        interrupted_stdout, interrupted_stderr = proc.communicate(timeout=20.0)
    except subprocess.TimeoutExpired:
        _diagnose_and_fail(proc, timeout=20.0, label="waiting for SIGINT to stop the run")
        interrupted_stdout, interrupted_stderr = "", ""  # unreachable
    assert proc.returncode == 130, (interrupt_engine, interrupted_stdout, interrupted_stderr)
    assert (case_dir / "out.json").exists()

    markers.proceed.write_text("")
    resumed = _run_to_completion(
        resume_engine, electricity_binary,
        [
            "orchestration.yml", "--config", "config.json", "--out", "out.json",
            "--state", "out.json", "--resume", "state", "--force",
        ],
        cwd=case_dir, env=env,
    )
    assert resumed.returncode == 0, (resumed.stdout, resumed.stderr)
    resumed_state = json.loads((case_dir / "out.json").read_text(encoding="utf-8"))

    # No `replacements` needed here, unlike most of this suite's own
    # cross-directory comparisons: every marker filename in the
    # orchestration text is bare (`_blocking_shell_effect`'s own doc
    # comment), so nothing in either state leaks its own case directory.
    assert_states_equal(
        prepare_for_comparison(resumed_state, []),
        prepare_for_comparison(baseline_state, []),
    )
