"""cof and electricity agree on an interrupted run (issue #796): an
interrupted `use` effect, and a signal that arrives while a `finally:`
runs, both behave the same way on either engine.

Every scenario below delivers its own signal(s) from *inside* the
orchestration itself -- a shell step running `kill -TERM $PPID` against
its own parent (the engine process), never a fixed sleep in this test
and never a signal sent from the test process. This sidesteps the usual
"wait for `--live-state` to show the step started" dance entirely: the
shell step's own `;`-sequenced commands already guarantee the kill has
been delivered and is being handled before the step moves on to its own
`sleep`, and (for the two-signal scenarios) a short `sleep 1` between
two self-inflicted `kill`s gives the kernel enough real time to treat
them as two separate deliveries rather than coalescing into one (POSIX
signals of the same number are not queued).

Three timings, three shapes, nine scenarios, each run on both `cof` and
`electricity` and compared against each other:

- **Timing `body_interrupt`**: the signal lands while the run's own body
  (not `finally:`) is still going. The leaf step's own subprocess is
  killed; its container (a `use`, a tree loop's `use` branch, or the
  document root) must end `ok: false` too -- never `ok: true` -- the
  same way an interrupted `dynamic` already does (#796 part 1).
- **Timing `finally_interrupt`**: the first signal lands while
  `cleanup_one` (the first `finally:` step) is running. `cleanup_one`'s
  own shell command still exits 0 (the signal reached the *engine*, not
  this step), so its own `end` event is `ok: true`; `cleanup_two` still
  runs after it; the enclosing container still ends `ok: false`,
  interrupted, once `finally:` is done (#796 part 2).
- **Timing `second_signal`**: a second signal lands while `cleanup_one`
  is still running. `cleanup_one` is killed this time (no `end` event
  for it), `cleanup_two` never runs, and the process exits at once --
  unaffected by #796, confirmed here on both engines.

Three shapes place the same body/`finally:` pair at a different nesting:
`top_level` (the document's own root), `use_child` (a `use` effect's
child document), and `tree_loop_use` (a `use` inside a tree-flow loop's
one branch).

On `main`, every `finally_interrupt`/`second_signal` scenario fails on
cof (`cleanup_one` is killed outright and `cleanup_two` never runs,
unlike electricity's own already-correct behaviour), and every
`body_interrupt` scenario with a `use` in it (`use_child`, `tree_loop_use`)
fails on electricity (the `use`'s own `start` is left unpaired, with no
matching `end` event at all, unlike cof's own -- itself wrong in a
different way before this change, ending `ok: true` instead of
`ok: false`). `top_level`/`body_interrupt` has no `use` in it at all, so
it already passes on both engines before this change -- a baseline.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import conformance.harness as harness
import pytest
from _signal_test_support import _diagnose_and_fail, _sandboxed_env
from conformance.normalize import assert_events_equal

#: Keys whose own value is expected to differ between two separate runs
#: (a timestamp, a run id, a wall-clock duration, a pid) even when every
#: observable *decision* the two engines made is identical -- replaced
#: with one placeholder apiece before `_states_match_ignoring_timing`
#: compares the rest of the tree structurally. Unlike
#: `conformance.normalize`'s own `_FIELD_RULES` (built for a *finished*
#: run, where every timestamp field is always a string), `None` is left
#: as `None` here: an interrupted leaf's own `meta.completed_at` never
#: gets set at all on some of this module's own shapes (a `tool` step
#: a real signal interrupts directly on the main thread, #796's own
#: unrelated, pre-existing behaviour), and the two engines still have to
#: agree on *that* -- collapsing `None` into the same placeholder a real
#: timestamp gets would hide exactly that mismatch.
_VOLATILE_KEYS = frozenset({
    "created_at", "completed_at", "started_at", "resolved_at",
    "run_id", "_run_id", "_timestamp", "wall_time_s", "elapsed_s",
    "eta_s", "ms", "pid",
})

ENGINES = ("cof", "electricity")
#: Every container kind an interruption can pass through on its way from
#: a leaf `tool` step to the root: the document root itself (`top_level`),
#: a `use` (`use_child`), a named `dynamic` (`dynamic_use`), a named `if`
#: branch (`if_branch_use`), a chain (sequential) named loop's one pass
#: (`loop_pass_use`), a tree-flow named loop's one branch
#: (`tree_loop_use`), and a `use` inside another `use`'s own child
#: (`use_in_use`).
SHAPES = (
    "top_level", "use_child", "dynamic_use", "if_branch_use", "loop_pass_use",
    "tree_loop_use", "use_in_use",
)
TIMINGS = ("body_interrupt", "finally_interrupt", "second_signal")

#: Generous but bounded -- `finally_interrupt` waits out a real `sleep 5`
#: inside the child process, `second_signal` a real `sleep 1` plus
#: however long the kernel takes to schedule the second `kill`.
_COMMUNICATE_TIMEOUT_SECONDS = 30.0


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


def _shell_step(name: str, script: str, *, indent: str) -> str:
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


def _body_and_finally(timing: str, *, indent: str) -> str:
    """The `effects:`/`finally:` pair shared by every shape, varied only
    by *timing* -- see the module docstring for what each one does."""
    if timing == "body_interrupt":
        body_script = "touch started; kill -TERM $PPID; exec sleep 60"
        cleanup_one_script = "touch cleanup_one_ran"
    elif timing == "finally_interrupt":
        body_script = "touch started"
        cleanup_one_script = "touch cleanup_started; kill -TERM $PPID; exec sleep 5"
    else:
        assert timing == "second_signal"
        body_script = "touch started"
        # `$$` is this `bash -c` process's own pid -- the test asserts it
        # is dead after the run exits, proving the second signal's own
        # `kill_cleanup_processes_too` actually reaches this step's
        # process and not just `cleanup_two` never running (#796).
        cleanup_one_script = (
            "echo $$ > cleanup_one.pid; touch cleanup_started; "
            "kill -TERM $PPID; sleep 1; kill -TERM $PPID; exec sleep 30"
        )
    body = _shell_step("body", body_script, indent=indent)
    cleanup_one = _shell_step("cleanup_one", cleanup_one_script, indent=indent)
    cleanup_two = _shell_step("cleanup_two", "touch finally_ran", indent=indent)
    return (
        f"{indent[:-2]}effects:\n{body}"
        f"{indent[:-2]}finally:\n{cleanup_one}{cleanup_two}"
    )


#: shape -> the orchestration.yml that wraps `child.yml` (which always
#: holds *pair*, the body/`finally:` document root itself) -- every shape
#: but `top_level` (no wrapper at all: *pair* **is** orchestration.yml)
#: and `use_in_use` (two levels of `use`, handled separately below).
_WRAPPER_ORCHESTRATIONS: dict[str, str] = {
    "use_child": "effects:\n  - type: use\n    name: child\n    path: child.yml\n",
    "tree_loop_use": (
        "interface:\n  inputs:\n    items: {default: [0]}\n"
        "effects:\n  - type: loop\n    name: lp\n    flow: tree\n"
        "    each: {in: input.items, as: x}\n    body:\n"
        "      - type: use\n        name: child\n        path: child.yml\n"
    ),
    "dynamic_use": (
        "effects:\n  - type: dynamic\n    name: dyn\n    effects:\n"
        "      - type: use\n        name: child\n        path: child.yml\n"
    ),
    "if_branch_use": (
        "effects:\n  - type: if\n    name: gate\n"
        '    if: {mode: cel, expr: "true"}\n    then:\n'
        "      - type: use\n        name: child\n        path: child.yml\n"
    ),
    "loop_pass_use": (
        "interface:\n  inputs:\n    items: {default: [0]}\n"
        "effects:\n  - type: loop\n    name: lp\n"
        "    each: {in: input.items, as: x}\n    body:\n"
        "      - type: use\n        name: child\n        path: child.yml\n"
    ),
}


def _write_case(shape: str, timing: str, case_dir: Path) -> None:
    case_dir.mkdir(parents=True)
    pair = _body_and_finally(timing, indent="  ")
    if shape == "top_level":
        (case_dir / "orchestration.yml").write_text(pair, encoding="utf-8")
    elif shape == "use_in_use":
        (case_dir / "orchestration.yml").write_text(
            "effects:\n  - type: use\n    name: outer\n    path: outer_child.yml\n",
            encoding="utf-8",
        )
        (case_dir / "outer_child.yml").write_text(
            "effects:\n  - type: use\n    name: child\n    path: child.yml\n",
            encoding="utf-8",
        )
        (case_dir / "child.yml").write_text(pair, encoding="utf-8")
    else:
        (case_dir / "orchestration.yml").write_text(
            _WRAPPER_ORCHESTRATIONS[shape], encoding="utf-8"
        )
        (case_dir / "child.yml").write_text(pair, encoding="utf-8")


def _assert_process_dead(pid: int, *, timeout: float, label: Any) -> None:
    """Poll *pid* until `kill(pid, 0)` reports it gone, or fail after
    *timeout* seconds -- a second signal's own `kill_cleanup_processes_too`
    must actually reach a `finally:` step's own tracked process, not just
    leave `cleanup_two` never running (#796)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return
        time.sleep(0.05)
    pytest.fail(f"cleanup_one's own process (pid {pid}) is still alive: {label}")


def _run(
    engine: str, binary: Path, case_dir: Path, env: dict[str, str],
) -> subprocess.CompletedProcess[str]:
    proc = subprocess.Popen(
        _cmd(
            engine, binary,
            ["orchestration.yml", "--events", "events.jsonl", "--out", "out.json"],
        ),
        cwd=case_dir, env=env,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    try:
        stdout, stderr = proc.communicate(timeout=_COMMUNICATE_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        _diagnose_and_fail(
            proc, timeout=_COMMUNICATE_TIMEOUT_SECONDS,
            label=f"waiting for {engine} to finish ({case_dir.name})",
        )
        stdout, stderr = "", ""  # unreachable
    return subprocess.CompletedProcess(proc.args, proc.returncode, stdout, stderr)


def _end_events_by_path(events_text: str) -> dict[str, dict[str, Any]]:
    """*path* -> its own `end` event (`ok`/`error` only), from a raw
    `--events` JSONL stream. A path with more than one `end` (none of
    this module's own scenarios have one) keeps the last."""
    out: dict[str, dict[str, Any]] = {}
    for line in events_text.splitlines():
        if not line.strip():
            continue
        event = json.loads(line)
        if event.get("ev") == "end":
            out[event["path"]] = {"ok": event.get("ok"), "error": event.get("error")}
    return out


def _without_volatile_values(value: Any) -> Any:
    """*value*, with every `_VOLATILE_KEYS` leaf collapsed to one shared
    placeholder (`None` kept as `None` -- see that set's own doc
    comment), so `test_interrupt_matches_across_engines` can compare two
    engines' own `--out` state structurally without either of them
    ever actually agreeing on a wall-clock timestamp or a run id."""
    if isinstance(value, dict):
        return {
            k: ("<volatile>" if k in _VOLATILE_KEYS and v is not None else _without_volatile_values(v))
            for k, v in value.items()
        }
    if isinstance(value, list):
        return [_without_volatile_values(v) for v in value]
    return value


def _effects_run(state: dict[str, Any]) -> int | None:
    totals = state.get("runtime", {}).get("last_run", {}).get("totals")
    if isinstance(totals, dict):
        return totals.get("effects_run")
    return None


#: shape -> the path prefix its own `child.yml` (holding the body/
#: `finally:` pair) writes under, and every container between that and
#: the root, inclusive -- none of them may end `ok: true` once the run
#: itself was interrupted (#796's own parity rule).
_SHAPE_CONTAINERS: dict[str, tuple[str, tuple[str, ...]]] = {
    "use_child": ("prime.child", ("prime.child", "prime")),
    "dynamic_use": ("prime.dyn.child", ("prime.dyn.child", "prime.dyn", "prime")),
    "if_branch_use": ("prime.gate.child", ("prime.gate.child", "prime.gate", "prime")),
    "loop_pass_use": (
        "prime.lp.iter_0.child",
        ("prime.lp.iter_0.child", "prime.lp", "prime"),
    ),
    "tree_loop_use": (
        "prime.lp.iter_0.child",
        ("prime.lp.iter_0.child", "prime.lp", "prime"),
    ),
    "use_in_use": ("prime.outer.child", ("prime.outer.child", "prime.outer", "prime")),
}


def _leaf_path(shape: str, name: str) -> str:
    if shape == "top_level":
        return f"prime.{name}"
    prefix, _ = _SHAPE_CONTAINERS[shape]
    return f"{prefix}.{name}"


def _container_paths(shape: str) -> list[str]:
    """Every container between the leaf steps and the root, inclusive --
    none of them may end `ok: true` once the run itself was interrupted
    (#796's own `use`/`dynamic`/`if`/named-loop/tree-loop-branch parity
    rule)."""
    if shape == "top_level":
        return ["prime"]
    _, containers = _SHAPE_CONTAINERS[shape]
    return list(containers)


@pytest.mark.parametrize("timing", TIMINGS)
@pytest.mark.parametrize("shape", SHAPES)
def test_interrupt_matches_across_engines(
    tmp_path: Path, electricity_binary: Path, shape: str, timing: str,
) -> None:
    env = _sandboxed_env(tmp_path)
    results: dict[str, subprocess.CompletedProcess[str]] = {}
    case_dirs: dict[str, Path] = {}
    for engine in ENGINES:
        case_dir = tmp_path / engine
        _write_case(shape, timing, case_dir)
        case_dirs[engine] = case_dir
        results[engine] = _run(engine, electricity_binary, case_dir, env)

    cof_result, elec_result = results["cof"], results["electricity"]
    assert cof_result.returncode == elec_result.returncode == 143, (
        timing, shape, cof_result.stdout, cof_result.stderr,
        elec_result.stdout, elec_result.stderr,
    )

    cof_dir, elec_dir = case_dirs["cof"], case_dirs["electricity"]
    assert (cof_dir / "started").exists() and (elec_dir / "started").exists()

    finally_ran_cof = (cof_dir / "finally_ran").exists()
    finally_ran_elec = (elec_dir / "finally_ran").exists()
    assert finally_ran_cof == finally_ran_elec, (timing, shape)
    if timing in ("body_interrupt", "finally_interrupt"):
        assert finally_ran_cof, (
            "finally: must run to completion for this timing", timing, shape,
        )
    else:
        assert timing == "second_signal"
        assert not finally_ran_cof, (
            "a second signal must cut finally: short", timing, shape,
        )
        for case_dir in (cof_dir, elec_dir):
            pid = int((case_dir / "cleanup_one.pid").read_text(encoding="utf-8").strip())
            _assert_process_dead(pid, timeout=5.0, label=(case_dir, timing, shape))

    cof_events_text = (cof_dir / "events.jsonl").read_text(encoding="utf-8")
    elec_events_text = (elec_dir / "events.jsonl").read_text(encoding="utf-8")
    cof_ends = _end_events_by_path(cof_events_text)
    elec_ends = _end_events_by_path(elec_events_text)

    body_path = _leaf_path(shape, "body")
    cleanup_one_path = _leaf_path(shape, "cleanup_one")
    cleanup_two_path = _leaf_path(shape, "cleanup_two")
    container_paths = _container_paths(shape)

    if timing == "body_interrupt":
        for ends in (cof_ends, elec_ends):
            # The leaf `tool` step itself never fires its own `end` event
            # at all: a real signal interrupts the main thread directly,
            # mid-step, before the tool's own exception handling ever runs
            # (`top_level`/`use_child`); a tree branch's own worker thread,
            # which a real signal never reaches directly, instead only
            # ever sees its tracked subprocess die via
            # `CancellationToken.request`'s kill, which both engines now
            # recognise as a cancellation rather than an ordinary failure
            # (`tree_loop_use`).
            assert body_path not in ends, (body_path, ends)
            for container in container_paths:
                # Never `ok: true` once the leaf it contains was
                # interrupted (#796's own parity rule) -- the exact
                # error text varies with how the kill reached the leaf
                # (a real signal straight into the main thread reports
                # "Interrupted (...)"; a tree branch's own worker thread
                # only ever sees its subprocess die, wrapped with the
                # branch's own path prefix) -- checked for being the
                # *same* text on both engines below, by
                # `assert_events_equal`, not pinned here.
                assert ends[container]["ok"] is False, (container, ends)
            assert ends[cleanup_one_path]["ok"] is True
            assert ends[cleanup_two_path]["ok"] is True
            # The root always reads the run's own top-level interrupt
            # text, regardless of how deep the leaf that was killed sits.
            assert ends["prime"]["error"] == "Interrupted (SIGTERM)", ends
    elif timing == "finally_interrupt":
        for ends in (cof_ends, elec_ends):
            # cleanup_one's own shell command always runs to completion
            # and exits 0 -- the signal landed on the engine, not on this
            # step -- so its own `end` event is never missing.
            assert ends[cleanup_one_path]["ok"] is True, (cleanup_one_path, ends)
            assert ends[cleanup_two_path]["ok"] is True
            for container in container_paths:
                assert ends[container]["ok"] is False, (container, ends)
            assert ends["prime"]["error"] == "Interrupted (SIGTERM)", ends
    else:
        assert timing == "second_signal"
        for ends in (cof_ends, elec_ends):
            assert cleanup_one_path not in ends, (
                (
                    "a second signal must kill cleanup_one before its own end "
                    "event -- no `--out` is written past this point either"
                ),
                ends,
            )
            assert cleanup_two_path not in ends

    if timing != "second_signal":
        # A second signal ends the process via `os._exit` (#356) --
        # `--events` never gets its own closing `run_end` line, so
        # `assert_events_equal`'s own internal-consistency check (every
        # stream ends with `run_end`) would reject both sides as
        # malformed before ever comparing them. The per-path checks
        # above already prove the two engines agree for this timing.
        assert_events_equal(cof_events_text, elec_events_text)

        assert (cof_dir / "out.json").exists() and (elec_dir / "out.json").exists()
        # Each engine's own `effective_settings` echoes back its own
        # absolute case directory (`_orchestration_dir`, the `--out` path)
        # -- the two run from different directories, so that leaks through
        # as a real difference `_without_volatile_values` has no key name
        # to catch. Redacted by literal substitution first, the same way
        # `conformance.normalize.redact_runtime_strings` does for the
        # shared suite.
        cof_text = (cof_dir / "out.json").read_text(encoding="utf-8").replace(
            str(cof_dir), "<case>"
        )
        elec_text = (elec_dir / "out.json").read_text(encoding="utf-8").replace(
            str(elec_dir), "<case>"
        )
        cof_state = json.loads(cof_text)
        elec_state = json.loads(elec_text)
        assert _effects_run(cof_state) == _effects_run(elec_state), (
            cof_state.get("runtime"), elec_state.get("runtime"),
        )
        assert _without_volatile_values(cof_state) == _without_volatile_values(elec_state)
    elif timing == "second_signal":
        assert not (cof_dir / "out.json").exists()
        assert not (elec_dir / "out.json").exists()


_NESTED_FINALLY_DOC = """\
effects:
  - type: tool
    name: body
    provider: shell
    params:
      command: bash
      args: ["-c", "touch started"]
      allowed_commands: ["bash"]
finally:
  - type: dynamic
    name: inner
    effects:
      - type: tool
        name: inner_body
        provider: shell
        params:
          command: bash
          args: ["-c", "touch inner_started"]
          allowed_commands: ["bash"]
    finally:
      - type: tool
        name: inner_cleanup
        provider: shell
        params:
          command: bash
          args: ["-c", "touch inner_cleanup_started; kill -TERM $PPID; exec sleep 5"]
          allowed_commands: ["bash"]
  - type: tool
    name: outer_cleanup
    provider: shell
    params:
      command: bash
      args: ["-c", "touch outer_cleanup_ran"]
      allowed_commands: ["bash"]
"""


def test_nested_finally_signal_does_not_cut_outer_cleanup_short(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """A first signal landing during a *nested* `finally:` -- a dynamic
    with its own `finally:`, itself one of the root's own cleanup steps --
    must not cut the root's own remaining cleanup short (#796): `inner`
    finishes both its body and its own `finally:` (reading `ok: true`,
    the same way a plain `cleanup_one` tool step does in the single-level
    scenarios above -- the signal landed on the engine, not on `inner`),
    `outer_cleanup` still runs after it, and only then does the run exit
    interrupted. On `main`, cof's own nested `dynamic` raises once its
    own `finally:` returns, which aborts the root's own `finally:` chain
    before `outer_cleanup` ever runs -- electricity already runs it.
    """
    env = _sandboxed_env(tmp_path)
    results: dict[str, subprocess.CompletedProcess[str]] = {}
    case_dirs: dict[str, Path] = {}
    for engine in ENGINES:
        case_dir = tmp_path / engine
        case_dir.mkdir()
        (case_dir / "orchestration.yml").write_text(_NESTED_FINALLY_DOC, encoding="utf-8")
        case_dirs[engine] = case_dir
        results[engine] = _run(engine, electricity_binary, case_dir, env)

    cof_result, elec_result = results["cof"], results["electricity"]
    assert cof_result.returncode == elec_result.returncode == 143, (
        cof_result.stdout, cof_result.stderr, elec_result.stdout, elec_result.stderr,
    )

    for case_dir in case_dirs.values():
        assert (case_dir / "started").exists()
        assert (case_dir / "inner_started").exists()
        assert (case_dir / "inner_cleanup_started").exists()
        assert (case_dir / "outer_cleanup_ran").exists(), (
            "a signal during a nested finally: must not skip the root's "
            "own remaining cleanup steps"
        )

    cof_dir, elec_dir = case_dirs["cof"], case_dirs["electricity"]
    cof_ends = _end_events_by_path((cof_dir / "events.jsonl").read_text(encoding="utf-8"))
    elec_ends = _end_events_by_path((elec_dir / "events.jsonl").read_text(encoding="utf-8"))
    for ends in (cof_ends, elec_ends):
        assert ends["prime.inner"]["ok"] is True, ends
        assert ends["prime.inner.inner_cleanup"]["ok"] is True, ends
        assert ends["prime.outer_cleanup"]["ok"] is True, ends
        assert ends["prime"]["ok"] is False, ends


#: issue #799's own three table rows -- a `dynamic` ("work") whose body
#: always fails, and whose own `finally:` always sends the engine a
#: first SIGTERM while *its* `cleanup` step is itself running:
#: - `absorbed_last`: `work`'s own `on_error: continue` absorbs the
#:   body's failure, and nothing else follows it -- the row the bug was
#:   filed on (on `main`, the run ends `ok: true`, exit 0, losing the
#:   interrupt entirely).
#: - `absorbed_followed`: same `on_error: continue`, but another effect
#:   (`after`) follows `work` -- already correct on `main` on both
#:   engines, kept here as a control: `after` must still never run.
#: - `unhandled`: `work` has no `on_error` (`fail`, the default) -- the
#:   existing rule that an unmasked failure wins over a later signal
#:   stays: exits 1, not 130/143/129, whether or not this fix changes
#:   anything else.
_LOST_INTERRUPT_ROWS = ("absorbed_last", "absorbed_followed", "unhandled")
#: Every placement issue #799 and its own fix must agree on: the
#: document's own root effects, a `use` child's own root effects, and
#: one pass of a named (chain-flow) loop's own body.
_LOST_INTERRUPT_PLACEMENTS = ("top_level", "use_child", "loop_pass")


def _work_dynamic_lines(row: str, *, indent: str) -> str:
    """The `dynamic` (`work`) + its own `finally:` for issue #799's own
    table, indented to sit directly inside *indent* (a document's own
    top-level `effects:`, a `use` child's own top-level `effects:`, or a
    `loop`'s own `body:`)."""
    field = indent + "  "
    on_error = f"{field}on_error: continue\n" if row != "unhandled" else ""
    lines = (
        f"{indent}- type: dynamic\n"
        f"{field}name: work\n"
        f"{on_error}"
        f"{field}effects:\n"
        f"{_shell_step('fails', 'exit 3', indent=field + '  ')}"
        f"{field}finally:\n"
        f"{_shell_step('cleanup', 'touch cleanup_started; kill -TERM $PPID; exec sleep 5', indent=field + '  ')}"
    )
    if row == "absorbed_followed":
        lines += _shell_step("after", "touch after_ran", indent=indent)
    return lines


_LOOP_PASS_DIRECT_WRAPPER = (
    "interface:\n  inputs:\n    items: {default: [0]}\n"
    "effects:\n  - type: loop\n    name: lp\n"
    "    each: {in: input.items, as: x}\n    body:\n"
)


def _write_lost_interrupt_case(placement: str, row: str, case_dir: Path) -> None:
    case_dir.mkdir(parents=True)
    if placement == "top_level":
        doc = "effects:\n" + _work_dynamic_lines(row, indent="  ")
        (case_dir / "orchestration.yml").write_text(doc, encoding="utf-8")
    elif placement == "use_child":
        (case_dir / "orchestration.yml").write_text(
            _WRAPPER_ORCHESTRATIONS["use_child"], encoding="utf-8"
        )
        child = "effects:\n" + _work_dynamic_lines(row, indent="  ")
        (case_dir / "child.yml").write_text(child, encoding="utf-8")
    else:
        assert placement == "loop_pass"
        doc = _LOOP_PASS_DIRECT_WRAPPER + _work_dynamic_lines(row, indent="      ")
        (case_dir / "orchestration.yml").write_text(doc, encoding="utf-8")


def _lost_interrupt_paths(placement: str) -> dict[str, str]:
    """*placement* -> the state path of `work`, its own `fails`/
    `cleanup` leaves, `after` (the `absorbed_followed` row's own
    sibling), and every container between `work` and the root,
    inclusive."""
    if placement == "top_level":
        work, after, containers = "prime.work", "prime.after", ("prime",)
    elif placement == "use_child":
        work = "prime.child.work"
        after = "prime.child.after"
        containers = ("prime.child", "prime")
    else:
        assert placement == "loop_pass"
        work = "prime.lp.iter_0.work"
        after = "prime.lp.iter_0.after"
        containers = ("prime.lp", "prime")
    return {
        "work": work, "fails": f"{work}.fails", "cleanup": f"{work}.cleanup",
        "after": after, "containers": containers,
    }


@pytest.mark.parametrize("placement", _LOST_INTERRUPT_PLACEMENTS)
@pytest.mark.parametrize("row", _LOST_INTERRUPT_ROWS)
def test_interrupt_during_finally_after_an_absorbed_failure_is_never_lost(
    tmp_path: Path, electricity_binary: Path, placement: str, row: str,
) -> None:
    """Issue #799: a signal landing while a `dynamic`'s own `finally:`
    runs, right after its own body failed and that failure was already
    absorbed by `on_error: continue`, still ends the run interrupted --
    even when nothing runs after that `dynamic` (`absorbed_last`, the
    exact shape the issue was filed on: `main` ends this `ok: true`,
    exit 0, on both engines). `absorbed_followed` and `unhandled` are
    controls already correct on `main`, kept here so a future change
    cannot quietly re-break either row while fixing another.
    """
    env = _sandboxed_env(tmp_path)
    results: dict[str, subprocess.CompletedProcess[str]] = {}
    case_dirs: dict[str, Path] = {}
    for engine in ENGINES:
        case_dir = tmp_path / engine
        _write_lost_interrupt_case(placement, row, case_dir)
        case_dirs[engine] = case_dir
        results[engine] = _run(engine, electricity_binary, case_dir, env)

    cof_result, elec_result = results["cof"], results["electricity"]
    expected_returncode = 1 if row == "unhandled" else 143
    assert cof_result.returncode == elec_result.returncode == expected_returncode, (
        row, placement, cof_result.stdout, cof_result.stderr,
        elec_result.stdout, elec_result.stderr,
    )

    cof_dir, elec_dir = case_dirs["cof"], case_dirs["electricity"]
    for case_dir, result in ((cof_dir, cof_result), (elec_dir, elec_result)):
        assert (case_dir / "cleanup_started").exists()
        assert (case_dir / "out.json").exists()
        summary = json.loads(result.stdout)
        assert summary["ok"] is False
        if row == "unhandled":
            assert summary["error"] != "Interrupted (SIGTERM)", summary
        else:
            assert summary["error"] == "Interrupted (SIGTERM)", summary

    paths = _lost_interrupt_paths(placement)
    cof_ends = _end_events_by_path((cof_dir / "events.jsonl").read_text(encoding="utf-8"))
    elec_ends = _end_events_by_path((elec_dir / "events.jsonl").read_text(encoding="utf-8"))
    for ends in (cof_ends, elec_ends):
        assert ends[paths["cleanup"]]["ok"] is True, (paths, ends)
        if row == "unhandled":
            # The body's own failure still wins over the later signal
            # (the existing rule this fix must not disturb): `work`
            # reports its own ordinary failure, never the interrupt.
            assert ends[paths["work"]]["ok"] is False, (paths, ends)
            assert ends[paths["work"]]["error"] != "Interrupted (SIGTERM)", ends
            for container in paths["containers"]:
                assert ends[container]["ok"] is False, (container, ends)
        else:
            assert ends[paths["work"]]["ok"] is False, (paths, ends)
            for container in paths["containers"]:
                assert ends[container]["ok"] is False, (container, ends)
                assert ends[container]["error"] == "Interrupted (SIGTERM)", (container, ends)
        if row == "absorbed_followed":
            assert paths["after"] not in ends, (paths, ends)
            assert not (case_dirs[
                "cof" if ends is cof_ends else "electricity"
            ] / "after_ran").exists()

    # `effects_run` agrees between the two engines either way (the
    # per-path checks above already establish they agree on *which*
    # effects ran).
    cof_state = json.loads((cof_dir / "out.json").read_text(encoding="utf-8"))
    elec_state = json.loads((elec_dir / "out.json").read_text(encoding="utf-8"))
    assert _effects_run(cof_state) == _effects_run(elec_state), (
        cof_state.get("runtime"), elec_state.get("runtime"),
    )


@pytest.mark.parametrize("engine", ENGINES)
def test_interrupt_during_finally_after_absorbed_failure_resumes(
    tmp_path: Path, electricity_binary: Path, engine: str,
) -> None:
    """Issue #799's own `absorbed_last` row, resumed: `work` never
    finished without error (its own `meta.error` is set even though
    `on_error: continue` absorbed it for its parent), so `--resume`
    reruns it from its own first effect -- this time `cleanup` sees
    `proceed` already on disk and skips the kill, and the whole run
    finishes normally, `ok: true`, exit 0.
    """
    binary, _ = harness.resolve_electricity_binary()
    env = _sandboxed_env(tmp_path)
    case_dir = tmp_path / engine
    case_dir.mkdir()
    cleanup_script = (
        "[ -f proceed ] || "
        "{ touch cleanup_started; kill -TERM $PPID; exec sleep 5; }"
    )
    cleanup = "    finally:\n" + _shell_step("cleanup", cleanup_script, indent="      ")
    doc = (
        "effects:\n  - type: dynamic\n    name: work\n    on_error: continue\n"
        "    effects:\n"
        f"{_shell_step('fails', 'exit 3', indent='      ')}"
        f"{cleanup}"
    )
    (case_dir / "orchestration.yml").write_text(doc, encoding="utf-8")

    first = _run(engine, binary, case_dir, env)
    assert first.returncode == 143, (first.stdout, first.stderr)
    assert (case_dir / "out.json").exists()
    summary_before = json.loads(first.stdout)
    assert summary_before["ok"] is False

    (case_dir / "proceed").touch()
    resume_proc = subprocess.Popen(
        _cmd(
            engine, binary,
            [
                "orchestration.yml", "--events", "events-resume.jsonl",
                "--state", "out.json", "--out", "out.json",
                "--resume", "state", "--force",
            ],
        ),
        cwd=case_dir, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    try:
        stdout, stderr = resume_proc.communicate(timeout=_COMMUNICATE_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        _diagnose_and_fail(
            resume_proc, timeout=_COMMUNICATE_TIMEOUT_SECONDS,
            label=f"waiting for {engine}'s own --resume to finish",
        )
        stdout, stderr = "", ""  # unreachable
    # A successful run with both `--out` and a piped (non-tty) stdout
    # prints no JSON summary at all (`app.py`'s own `--print`/no-`--out`
    # gate) -- the exit code and `--events` stream are this run's only
    # signal.
    assert resume_proc.returncode == 0, (engine, stdout, stderr)
    ends = _end_events_by_path(
        (case_dir / "events-resume.jsonl").read_text(encoding="utf-8")
    )
    assert ends["prime"]["ok"] is True, ends
    assert ends["prime.work.fails"]["ok"] is False, ends
    assert ends["prime.work.cleanup"]["ok"] is True, ends
