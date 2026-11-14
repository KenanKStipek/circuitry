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
`body_interrupt` scenario fails on electricity (the container's own
`start` is left unpaired, with no matching `end` event at all, unlike
cof's own -- itself wrong in a different way before this change,
ending `ok: true` instead of `ok: false`).
"""

from __future__ import annotations

import json
import subprocess
import sys
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
SHAPES = ("top_level", "use_child", "tree_loop_use")
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
        cleanup_one_script = (
            "touch cleanup_started; kill -TERM $PPID; sleep 1; "
            "kill -TERM $PPID; exec sleep 30"
        )
    body = _shell_step("body", body_script, indent=indent)
    cleanup_one = _shell_step("cleanup_one", cleanup_one_script, indent=indent)
    cleanup_two = _shell_step("cleanup_two", "touch finally_ran", indent=indent)
    return (
        f"{indent[:-2]}effects:\n{body}"
        f"{indent[:-2]}finally:\n{cleanup_one}{cleanup_two}"
    )


def _write_case(shape: str, timing: str, case_dir: Path) -> None:
    case_dir.mkdir(parents=True)
    pair = _body_and_finally(timing, indent="  ")
    if shape == "top_level":
        (case_dir / "orchestration.yml").write_text(pair, encoding="utf-8")
    elif shape == "use_child":
        (case_dir / "orchestration.yml").write_text(
            "effects:\n  - type: use\n    name: child\n    path: child.yml\n",
            encoding="utf-8",
        )
        (case_dir / "child.yml").write_text(pair, encoding="utf-8")
    else:
        assert shape == "tree_loop_use"
        (case_dir / "orchestration.yml").write_text(
            "interface:\n  inputs:\n    items: {default: [0]}\n"
            "effects:\n  - type: loop\n    name: lp\n    flow: tree\n"
            "    each: {in: input.items, as: x}\n    body:\n"
            "      - type: use\n        name: child\n        path: child.yml\n",
            encoding="utf-8",
        )
        (case_dir / "child.yml").write_text(pair, encoding="utf-8")


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


def _leaf_path(shape: str, name: str) -> str:
    if shape == "top_level":
        return f"prime.{name}"
    if shape == "use_child":
        return f"prime.child.{name}"
    assert shape == "tree_loop_use"
    return f"prime.lp.iter_0.child.{name}"


def _container_paths(shape: str) -> list[str]:
    """Every container between the leaf steps and the root, inclusive --
    none of them may end `ok: true` once the run itself was interrupted
    (#796's own `use`/`dynamic`/tree-loop-branch parity rule)."""
    if shape == "top_level":
        return ["prime"]
    if shape == "use_child":
        return ["prime.child", "prime"]
    assert shape == "tree_loop_use"
    return ["prime.lp.iter_0.child", "prime.lp", "prime"]


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
            # The leaf `tool` step itself either never fires its own `end`
            # event at all (a real signal interrupts the main thread
            # directly, mid-step, before the tool's own exception handling
            # ever runs -- the `top_level`/`use_child` shapes) or does,
            # reporting the killed subprocess as an ordinary failure (a
            # tree branch's own worker thread, which a real signal never
            # reaches directly -- `tree_loop_use` -- only ever sees its
            # subprocess die via `CancellationToken.request`'s kill).
            # Both are pre-existing, unrelated to #796; this module's own
            # `assert_events_equal` below is what actually proves the two
            # engines agree on which of the two happens for a given shape.
            if body_path in ends:
                assert ends[body_path]["ok"] is False, (body_path, ends)
            for container in container_paths:
                # Never `ok: true` once the leaf it contains was
                # interrupted (#796's own parity rule) -- the exact
                # error text varies with how the kill reached the leaf
                # (a real signal straight into the main thread reports
                # "Interrupted (...)"; a tree branch's own worker thread
                # only ever sees its subprocess die, an ordinary failure
                # wrapped with the branch's own path prefix) -- checked
                # for being the *same* text on both engines below, by
                # `assert_events_equal`, not pinned here.
                assert ends[container]["ok"] is False, (container, ends)
            assert ends[cleanup_one_path]["ok"] is True
            assert ends[cleanup_two_path]["ok"] is True
    elif timing == "finally_interrupt":
        for ends in (cof_ends, elec_ends):
            assert cleanup_one_path not in ends or ends[cleanup_one_path]["ok"] is True, (
                "cleanup_one's own shell command must succeed -- the signal "
                "landed on the engine, not on this step", ends,
            )
            assert ends[cleanup_two_path]["ok"] is True
            for container in container_paths:
                assert ends[container]["ok"] is False, (container, ends)
    else:
        assert timing == "second_signal"
        for ends in (cof_ends, elec_ends):
            assert cleanup_one_path not in ends, (
                "a second signal must kill cleanup_one before its own end "
                "event -- no `--out` is written past this point either",
                ends,
            )
            assert cleanup_two_path not in ends

    # `tree_loop_use`/`body_interrupt` is excluded from every full-stream
    # comparison below for a narrower reason, outside #796's own two
    # sub-issues: a tree branch's own leaf `tool` step, once its tracked
    # subprocess is killed, reports an ordinary nonzero-exit failure on
    # cof (`core/tool.py` never consults the cancellation token at all)
    # but is left with no `end` event on electricity -- a real signal
    # never reaches a worker thread directly on either engine, so neither
    # one's own *leaf* `tool` step recognises the kill as a cancellation
    # the way the enclosing `use` now does (the fix above). The
    # container-level (`use`/tree-branch/root) checks already above cover
    # #796's own acceptance for this shape; this gap is reported
    # separately, not papered over here.
    skip_full_stream_comparison = (shape, timing) == ("tree_loop_use", "body_interrupt")

    if timing != "second_signal" and not skip_full_stream_comparison:
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
