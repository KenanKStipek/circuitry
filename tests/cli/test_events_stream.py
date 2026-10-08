"""`cof run --events <file>` end to end (#419): real `cof run` subprocesses
with shell `sleep`/tool steps, no adapter needed (tool-only orchestrations
resolve the `_noop` adapter) — see `electricity/docs/spec/scripted-replies.md`
and `electricity/docs/spec/runtime-semantics.md`'s "Event stream" section
for the format this exercises. Unit tests for `EventLog` itself live in
`test_events.py`.
"""

from __future__ import annotations

import json
import signal
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

from _signal_test_support import _diagnose_and_fail, _sandboxed_env, _wait_for_paths


def _run_cof(
    orch: Path,
    *,
    out_path: Path,
    events_path: Path,
    state_path: Path | None = None,
    live_state_path: Path | None = None,
    extra_args: list[str] | None = None,
) -> subprocess.Popen[str]:
    tmp_path = orch.parent
    args = [sys.executable, "-m", "circuitry.cli.app", "run", str(orch)]
    if state_path is not None:
        args += ["--state", str(state_path)]
    args += ["--out", str(out_path), "--events", str(events_path), "--quiet"]
    if live_state_path is not None:
        args += ["--live-state", str(live_state_path)]
    args += extra_args or []
    return subprocess.Popen(
        args,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=_sandboxed_env(tmp_path),
        cwd=tmp_path,
    )


def _events(path: Path) -> list[dict[str, Any]]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


def _communicate(proc: subprocess.Popen[str], *, timeout: float, label: str) -> tuple[str, str]:
    try:
        return proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired as exc:
        _diagnose_and_fail(proc, timeout=timeout, label=label)
        raise AssertionError("unreachable") from exc  # _diagnose_and_fail always fails


def test_tree_each_max_concurrency_dispatch_and_pairs(tmp_path: Path) -> None:
    """A `flow: tree` `each` loop, `max_concurrency: 2`, over 3 passes: one
    `dispatch`, three `start`/`end` pairs with distinct ids, the
    container's own start/end bracketing every pass, and pass 2 starting
    only once pass 0 or pass 1 has already ended."""
    orch = tmp_path / "tree_each.yml"
    orch.write_text(
        """
effects:
  - type: loop
    name: each_tree
    flow: tree
    max_concurrency: 2
    each: {in: input.items, as: x}
    body:
      - type: tool
        name: t_nap
        provider: shell
        params:
          command: sleep
          args: ["0.4"]
          allowed_commands: ["sleep"]
""".lstrip("\n"),
        encoding="utf-8",
    )
    state_path = tmp_path / "state.json"
    state_path.write_text(json.dumps({"items": ["a", "b", "c"]}), encoding="utf-8")
    out_path = tmp_path / "out.json"
    events_path = tmp_path / "events.jsonl"

    proc = _run_cof(orch, out_path=out_path, events_path=events_path, state_path=state_path)
    stdout, stderr = _communicate(proc, timeout=20.0, label="waiting for the run to finish")

    assert proc.returncode == 0, (stdout, stderr)
    events = _events(events_path)
    assert events[0]["ev"] == "run_start"
    assert events[-1]["ev"] == "run_end"
    assert events[-1]["ok"] is True

    dispatches = [e for e in events if e["ev"] == "dispatch"]
    assert len(dispatches) == 1
    assert dispatches[0]["path"] == "prime.each_tree"
    # `branches` is the concurrency ceiling the existing
    # `concurrent_dispatch` callback reports (`min(max_concurrency, total)`),
    # not the loop's total pass count (3 here) — see the reference's
    # `dispatch` row (#419 review finding 1).
    assert dispatches[0]["branches"] == 2

    container_start = next(
        e for e in events if e["ev"] == "start" and e["path"] == "prime.each_tree"
    )
    container_end = next(
        e for e in events if e["ev"] == "end" and e["path"] == "prime.each_tree"
    )
    assert container_end["ok"] is True

    pass_starts = {}
    pass_ends = {}
    for i in range(3):
        path = f"prime.each_tree.iter_{i}.t_nap"
        pass_starts[i] = next(e for e in events if e["ev"] == "start" and e["path"] == path)
        pass_ends[i] = next(e for e in events if e["ev"] == "end" and e["path"] == path)
        assert pass_ends[i]["ok"] is True

    ids = {pass_starts[i]["id"] for i in range(3)}
    assert len(ids) == 3
    for i in range(3):
        assert pass_starts[i]["id"] == pass_ends[i]["id"]

    # Container brackets every pass.
    assert all(container_start["seq"] < pass_starts[i]["seq"] for i in range(3))
    assert all(container_end["seq"] > pass_ends[i]["seq"] for i in range(3))

    # max_concurrency: 2 — pass 2 only ever starts once pass 0 or pass 1
    # has already ended. `seq` gives a strict order; wall-clock `ts` is
    # only millisecond-precision and could tie.
    assert pass_starts[2]["seq"] > min(pass_ends[0]["seq"], pass_ends[1]["seq"])

    # No torn lines anywhere in the stream.
    raw_lines = events_path.read_text(encoding="utf-8").splitlines()
    for line in raw_lines:
        json.loads(line)
    seqs = [e["seq"] for e in events]
    assert seqs == list(range(len(seqs)))

    # Every `start` in the whole stream pairs with exactly one `end`.
    start_ids = [e["id"] for e in events if e["ev"] == "start"]
    end_ids = [e["id"] for e in events if e["ev"] == "end"]
    assert sorted(start_ids) == sorted(end_ids)
    assert len(start_ids) == len(set(start_ids))


def test_unnamed_loop_body_passes_get_distinct_ids(tmp_path: Path) -> None:
    """An unnamed loop fires no start/end of its own, but its body still
    does, once per pass, all sharing the parent's path (#419) — each pass
    still gets its own instance id."""
    orch = tmp_path / "unnamed_loop.yml"
    orch.write_text(
        """
effects:
  - type: loop
    each: {in: input.items, as: x}
    body:
      - type: tool
        name: t
        provider: shell
        params:
          command: echo
          args: ["{{x}}"]
""".lstrip("\n"),
        encoding="utf-8",
    )
    state_path = tmp_path / "state.json"
    state_path.write_text(json.dumps({"items": ["a", "b", "c"]}), encoding="utf-8")
    out_path = tmp_path / "out.json"
    events_path = tmp_path / "events.jsonl"

    proc = _run_cof(orch, out_path=out_path, events_path=events_path, state_path=state_path)
    stdout, stderr = _communicate(proc, timeout=20.0, label="waiting for the run to finish")

    assert proc.returncode == 0, (stdout, stderr)
    events = _events(events_path)
    starts = [e for e in events if e["ev"] == "start" and e["path"] == "prime.t"]
    ends = [e for e in events if e["ev"] == "end" and e["path"] == "prime.t"]
    assert len(starts) == 3
    assert {e["id"] for e in starts} == {e["id"] for e in ends}
    assert len({e["id"] for e in starts}) == 3


def test_unnamed_tree_loop_body_passes_get_distinct_ids_across_threads(tmp_path: Path) -> None:
    """An unnamed `flow: tree` loop runs every pass on its own worker
    thread, all sharing the parent's path (#419 review finding 7) — the
    per-thread stack keyed by path must still pair each pass's own
    `start` with its own `end`, not some other thread's."""
    orch = tmp_path / "unnamed_tree_loop.yml"
    orch.write_text(
        """
effects:
  - type: loop
    flow: tree
    each: {in: input.items, as: x}
    body:
      - type: tool
        name: t
        provider: shell
        params:
          command: sleep
          args: ["0.2"]
          allowed_commands: ["sleep"]
""".lstrip("\n"),
        encoding="utf-8",
    )
    state_path = tmp_path / "state.json"
    state_path.write_text(json.dumps({"items": ["a", "b", "c"]}), encoding="utf-8")
    out_path = tmp_path / "out.json"
    events_path = tmp_path / "events.jsonl"

    proc = _run_cof(orch, out_path=out_path, events_path=events_path, state_path=state_path)
    stdout, stderr = _communicate(proc, timeout=20.0, label="waiting for the run to finish")

    assert proc.returncode == 0, (stdout, stderr)
    events = _events(events_path)
    starts = [e for e in events if e["ev"] == "start" and e["path"] == "prime.t"]
    ends = [e for e in events if e["ev"] == "end" and e["path"] == "prime.t"]
    assert len(starts) == 3
    assert len(ends) == 3
    assert {e["id"] for e in starts} == {e["id"] for e in ends}
    assert len({e["id"] for e in starts}) == 3
    for e in ends:
        assert e["ok"] is True


def test_failing_effect_on_error_continue_truncates_error(tmp_path: Path) -> None:
    """A failing effect's `end` carries `ok: false` and an `error` cut to
    500 characters; `on_error: continue` lets the run still succeed."""
    orch = tmp_path / "continue_on_error.yml"
    orch.write_text(
        """
effects:
  - type: tool
    name: always_fails
    provider: shell
    on_error: continue
    params:
      command: bash
      args: ["-c", "head -c 600 /dev/zero | tr '\\\\0' x >&2; exit 1"]
      allowed_commands: ["bash"]
""".lstrip("\n"),
        encoding="utf-8",
    )
    out_path = tmp_path / "out.json"
    events_path = tmp_path / "events.jsonl"

    proc = _run_cof(orch, out_path=out_path, events_path=events_path)
    stdout, stderr = _communicate(proc, timeout=20.0, label="waiting for the run to finish")

    assert proc.returncode == 0, (stdout, stderr)
    events = _events(events_path)
    end = next(e for e in events if e["ev"] == "end" and e["path"] == "prime.always_fails")
    assert end["ok"] is False
    assert len(end["error"]) == 500
    run_end = events[-1]
    assert run_end["ev"] == "run_end"
    assert run_end["ok"] is True


def test_a_run_that_fails_reports_a_truncated_error_on_run_end(tmp_path: Path) -> None:
    orch = tmp_path / "failing_run.yml"
    orch.write_text(
        """
effects:
  - type: tool
    name: always_fails
    provider: shell
    params:
      command: bash
      args: ["-c", "head -c 600 /dev/zero | tr '\\\\0' x >&2; exit 1"]
      allowed_commands: ["bash"]
""".lstrip("\n"),
        encoding="utf-8",
    )
    out_path = tmp_path / "out.json"
    events_path = tmp_path / "events.jsonl"

    proc = _run_cof(orch, out_path=out_path, events_path=events_path)
    stdout, stderr = _communicate(proc, timeout=20.0, label="waiting for the run to finish")

    assert proc.returncode == 1, (stdout, stderr)
    events = _events(events_path)
    run_end = events[-1]
    assert run_end["ev"] == "run_end"
    assert run_end["ok"] is False
    assert len(run_end["error"]) == 500
    assert "signal" not in run_end


def test_sigint_once_run_end_has_the_signal_after_the_final_live_state_write(
    tmp_path: Path,
) -> None:
    orch = tmp_path / "slow.yml"
    started = tmp_path / "started"
    orch.write_text(
        f"""
effects:
  - type: tool
    name: slow
    provider: shell
    params:
      command: bash
      args: ["-c", "touch {started}; exec sleep 30"]
      allowed_commands: ["bash"]
""".lstrip("\n"),
        encoding="utf-8",
    )
    out_path = tmp_path / "out.json"
    events_path = tmp_path / "events.jsonl"
    live_state_path = tmp_path / "live.json"

    proc = _run_cof(
        orch, out_path=out_path, events_path=events_path, live_state_path=live_state_path
    )
    try:
        _wait_for_paths([started])
    except TimeoutError:
        _diagnose_and_fail(proc, timeout=20.0, label="waiting for the step to start")

    proc.send_signal(signal.SIGINT)
    stdout, stderr = _communicate(
        proc, timeout=20.0, label="waiting for the run to exit after SIGINT"
    )

    assert proc.returncode == 130, (stdout, stderr)
    events = _events(events_path)
    run_end = events[-1]
    assert run_end["ev"] == "run_end"
    assert run_end["ok"] is False
    assert run_end["signal"] == "SIGINT"

    # The final live-state write landed — `run_end` is only ever written
    # after it (`runtime_shim.run`'s own `finally:` order).
    live_state = json.loads(live_state_path.read_text(encoding="utf-8"))
    assert live_state["runtime"]["last_run"]["completed_at"]


def test_second_sigint_during_cleanup_writes_no_run_end(tmp_path: Path) -> None:
    """A second SIGINT ends the process via `os._exit` before `finally:`
    (and so before `run_end`) ever completes (#419's own "Abort" rule)."""
    orch = tmp_path / "second_signal.yml"
    started = tmp_path / "started"
    cleanup_started = tmp_path / "cleanup_started"
    orch.write_text(
        f"""
effects:
  - type: dynamic
    name: d1
    effects:
      - type: tool
        name: slow
        provider: shell
        params:
          command: bash
          args: ["-c", "touch {started}; exec sleep 30"]
          allowed_commands: ["bash"]
    finally:
      - type: tool
        name: cleanup
        provider: shell
        params:
          command: bash
          args: ["-c", "touch {cleanup_started}; exec sleep 30"]
          allowed_commands: ["bash"]
""".lstrip("\n"),
        encoding="utf-8",
    )
    out_path = tmp_path / "out.json"
    events_path = tmp_path / "events.jsonl"

    proc = _run_cof(orch, out_path=out_path, events_path=events_path)
    try:
        _wait_for_paths([started])
    except TimeoutError:
        _diagnose_and_fail(proc, timeout=20.0, label="waiting for the step to start")

    proc.send_signal(signal.SIGINT)
    try:
        _wait_for_paths([cleanup_started])
    except TimeoutError:
        _diagnose_and_fail(
            proc, timeout=20.0, label="waiting for cleanup to start after the first signal"
        )

    t0 = time.monotonic()
    proc.send_signal(signal.SIGINT)
    stdout, stderr = _communicate(
        proc, timeout=20.0, label="waiting for the second signal to end the run"
    )
    elapsed = time.monotonic() - t0

    assert proc.returncode == 130, (stdout, stderr)
    assert elapsed < 10.0, f"took {elapsed:.1f}s; cleanup sleeps 30s uninterrupted"
    assert "Traceback" not in stderr, stderr

    raw_lines = events_path.read_text(encoding="utf-8").splitlines()
    events = [json.loads(line) for line in raw_lines]
    assert all(e["ev"] != "run_end" for e in events)
    assert any(e["ev"] == "run_start" for e in events)


def test_an_unwritable_events_path_still_succeeds_with_one_warning(tmp_path: Path) -> None:
    orch = tmp_path / "ok.yml"
    orch.write_text(
        """
effects:
  - type: tool
    name: step
    provider: shell
    params:
      command: echo
      args: ["hi"]
""".lstrip("\n"),
        encoding="utf-8",
    )
    out_path = tmp_path / "out.json"
    blocker = tmp_path / "not-a-dir"
    blocker.write_text("", encoding="utf-8")
    events_path = blocker / "events.jsonl"

    proc = _run_cof(orch, out_path=out_path, events_path=events_path)
    stdout, stderr = _communicate(proc, timeout=20.0, label="waiting for the run to finish")

    assert proc.returncode == 0, (stdout, stderr)
    assert out_path.exists()
    assert not events_path.exists()
    warning_lines = [
        line for line in stderr.splitlines() if "sync with the run" in line
    ]
    assert len(warning_lines) == 1, (stdout, stderr)


def _normalize_for_comparison(value: Any) -> Any:
    """Strip wall-clock/run-id fields that legitimately differ between two
    otherwise-identical runs (timestamps, `run_id`, `totals.wall_time_s`),
    so two runs of the same orchestration can be compared for equality."""
    if isinstance(value, dict):
        out: dict[str, Any] = {}
        for key, sub in value.items():
            if key in ("_run_id", "_timestamp", "created_at", "completed_at"):
                continue
            cleaned = sub
            if key == "runtime" and isinstance(sub, dict):
                cleaned = {k: v for k, v in sub.items() if k != "last_run"}
            out[key] = _normalize_for_comparison(cleaned)
        return out
    if isinstance(value, list):
        return [_normalize_for_comparison(v) for v in value]
    return value


def test_events_never_changes_a_failing_runs_outcome(tmp_path: Path) -> None:
    """A failing tool (`on_error: continue`, swallowed) and a failing
    prompt (default `on_error: fail`, propagates and fails the run) must
    produce the same final state and exit code with and without
    `--events` — the stream never affects the run it observes (#419
    review)."""
    replies_path = tmp_path / "replies.yaml"
    replies_path.write_text(
        "prime.fails_fail:\n  - error:\n      kind: server_error\n", encoding="utf-8"
    )
    config_path = tmp_path / "config.json"
    config_path.write_text(
        json.dumps(
            {
                "default_adapter": "scripted",
                "default_model": "test",
                "runtime": {
                    "adapters": {"scripted": {"replies_file": str(replies_path)}}
                },
            }
        ),
        encoding="utf-8",
    )
    orch = tmp_path / "orch.yml"
    orch.write_text(
        """
effects:
  - type: tool
    name: fails_continue
    provider: shell
    on_error: continue
    params:
      command: bash
      args: ["-c", "exit 1"]
      allowed_commands: ["bash"]
  - type: prompt
    name: fails_fail
    template: "ask something"
""".lstrip("\n"),
        encoding="utf-8",
    )

    out_path = tmp_path / "out.json"

    def run_once(*, with_events: bool) -> tuple[int, dict[str, Any]]:
        args = [
            sys.executable, "-m", "circuitry.cli.app", "run", str(orch),
            "--config", str(config_path), "--out", str(out_path), "--quiet",
        ]
        if with_events:
            args += ["--events", str(tmp_path / "events.jsonl")]
        proc = subprocess.run(
            args,
            capture_output=True,
            text=True,
            timeout=20.0,
            env=_sandboxed_env(tmp_path),
            cwd=tmp_path,
            check=False,
        )
        return proc.returncode, json.loads(out_path.read_text(encoding="utf-8"))

    code_without, state_without = run_once(with_events=False)
    code_with, state_with = run_once(with_events=True)

    assert code_without == code_with == 1
    assert _normalize_for_comparison(state_without) == _normalize_for_comparison(state_with)
    assert (tmp_path / "events.jsonl").exists()


def test_run_end_is_emitted_after_the_live_state_mirrors_final_write(
    tmp_path: Path, monkeypatch: Any
) -> None:
    """``run_end`` must go out only after ``--live-state``'s own final
    write (#419's own ordering rule) — checked by instrumenting the actual
    call order in-process, which a subprocess's own exit code/file
    contents can't distinguish from "both happened, in some order"."""
    from circuitry.cli.config import CircuitryConfig
    from circuitry.cli.events import EventLog
    from circuitry.cli.live_state import LiveStateMirror
    from circuitry.cli.runtime_shim import RunRequest, run

    order: list[str] = []
    real_close = LiveStateMirror.close

    def recording_close(self: LiveStateMirror, final_state: dict[str, Any]) -> bool:
        order.append("live_state_close")
        return real_close(self, final_state)

    monkeypatch.setattr(LiveStateMirror, "close", recording_close)

    real_run_end = EventLog.run_end

    def recording_run_end(self: EventLog, **kwargs: Any) -> None:
        order.append("run_end")
        real_run_end(self, **kwargs)

    monkeypatch.setattr(EventLog, "run_end", recording_run_end)

    orch_path = tmp_path / "orch.json"
    orch_path.write_text(
        json.dumps({"effects": [{"type": "tool", "name": "step", "provider": "uuid"}]}),
        encoding="utf-8",
    )
    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=tmp_path / "out.json",
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
            live_state_path=tmp_path / "live.json",
            events_path=tmp_path / "events.jsonl",
            skip_preflight=True,
        )
    )

    assert result.ok, result.error
    assert order == ["live_state_close", "run_end"]


def test_run_library_events_writes_the_stream(tmp_path: Path, monkeypatch: Any) -> None:
    """`cof run-library --events <f>` wires the same stream as `cof run`
    (#419 review finding 7: nothing previously exercised `run-library`)."""
    import pytest

    pytest.importorskip("typer")
    from typing import ClassVar

    from typer.testing import CliRunner

    from circuitry.cli import app as app_module
    from circuitry.cli.app import app

    orch = tmp_path / "noop.yml"
    orch.write_text(
        """
effects:
  - type: tool
    name: step
    provider: shell
    params:
      command: echo
      args: ["hi"]
""".lstrip("\n"),
        encoding="utf-8",
    )

    class _Asset:
        asset_id = "demo"
        version = "1.0.0"
        source = "test"
        file_path = orch
        metadata: ClassVar[dict] = {}

    monkeypatch.setattr(app_module, "fetch_shared_orchestration", lambda **kwargs: _Asset())

    out_path = tmp_path / "out.json"
    events_path = tmp_path / "events.jsonl"
    runner = CliRunner()
    result = runner.invoke(
        app,
        [
            "run-library",
            "demo",
            "--out",
            str(out_path),
            "--events",
            str(events_path),
            "--allow-capabilities",
            "shell",
        ],
    )
    assert result.exit_code == 0, result.stdout

    events = _events(events_path)
    assert events[0]["ev"] == "run_start"
    assert events[-1]["ev"] == "run_end"
    assert events[-1]["ok"] is True
    start = next(e for e in events if e["ev"] == "start" and e["path"] == "prime.step")
    end = next(e for e in events if e["ev"] == "end" and e["path"] == "prime.step")
    assert start["id"] == end["id"]
    assert end["ok"] is True
