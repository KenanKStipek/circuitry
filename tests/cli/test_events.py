"""Unit tests for :class:`circuitry.cli.events.EventLog` (#419)."""

from __future__ import annotations

import json
import threading
from pathlib import Path
from typing import Any

import pytest

from circuitry.cli.events import EventLog


def _lines(path: Path) -> list[dict[str, Any]]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


def test_run_start_is_first_line(tmp_path: Path) -> None:
    log = EventLog(tmp_path / "events.jsonl")
    log.run_start(run_id="r1", orchestration="do-thing.yml")
    log.close()

    lines = _lines(tmp_path / "events.jsonl")
    assert lines[0]["ev"] == "run_start"
    assert lines[0]["seq"] == 0
    assert lines[0]["v"] == 1
    assert lines[0]["run_id"] == "r1"
    assert lines[0]["orchestration"] == "do-thing.yml"
    assert lines[0]["engine"].startswith("cof ")
    assert isinstance(lines[0]["pid"], int)
    assert lines[0]["ts"].endswith("Z")


def test_dispatch_payload_carries_branches_and_concurrency(tmp_path: Path) -> None:
    """`branches` is the true branch total; `concurrency` is the ceiling
    the pool actually enforces — the two differ whenever `max_concurrency`
    caps a loop or dynamic below its item/effect count (#423)."""
    log = EventLog(tmp_path / "events.jsonl")
    log.on_dispatch("prime.each", 2, 3)
    log.close()

    dispatch = _lines(tmp_path / "events.jsonl")[0]
    assert dispatch["ev"] == "dispatch"
    assert dispatch["path"] == "prime.each"
    assert dispatch["branches"] == 3
    assert dispatch["concurrency"] == 2


def test_seq_strictly_increasing_across_event_kinds(tmp_path: Path) -> None:
    log = EventLog(tmp_path / "events.jsonl")
    log.run_start(run_id="r1", orchestration="o.yml")
    log.on_dispatch("prime.each", 2, 3)
    log.on_start("prime.each.iter_0.t", {})
    log.on_complete("prime.each.iter_0.t", {"meta": {"error": None}})
    log.run_end(ok=True, error=None, signal=None)
    log.close()

    seqs = [line["seq"] for line in _lines(tmp_path / "events.jsonl")]
    assert seqs == sorted(seqs)
    assert seqs == list(range(len(seqs)))


def test_container_start_before_child_start_child_end_before_container_end(
    tmp_path: Path,
) -> None:
    log = EventLog(tmp_path / "events.jsonl")
    log.on_start("prime.d1", {})
    log.on_start("prime.d1.child", {})
    log.on_complete("prime.d1.child", {"meta": {"error": None}})
    log.on_complete("prime.d1", {"meta": {"error": None}})
    log.close()

    events = _lines(tmp_path / "events.jsonl")
    kinds = [(e["ev"], e["path"]) for e in events]
    assert kinds == [
        ("start", "prime.d1"),
        ("start", "prime.d1.child"),
        ("end", "prime.d1.child"),
        ("end", "prime.d1"),
    ]


def test_start_end_pairing_gives_distinct_ids_for_the_same_path(tmp_path: Path) -> None:
    """An unnamed loop's body writes one path per pass -- each pass still
    gets its own instance id, even run sequentially on the same thread."""
    log = EventLog(tmp_path / "events.jsonl")
    log.on_start("prime.body", {})
    log.on_complete("prime.body", {"meta": {"error": None}})
    log.on_start("prime.body", {})
    log.on_complete("prime.body", {"meta": {"error": None}})
    log.close()

    events = _lines(tmp_path / "events.jsonl")
    starts = [e["id"] for e in events if e["ev"] == "start"]
    ends = [e["id"] for e in events if e["ev"] == "end"]
    assert len(set(starts)) == 2
    assert starts == ends


def test_concurrent_tree_branches_pair_ids_per_thread(tmp_path: Path) -> None:
    """Several threads starting/completing the *same* path concurrently
    still pair each start with its own end -- no thread sees another
    thread's id, since start/end for one instance always share a thread."""
    log = EventLog(tmp_path / "events.jsonl")
    errors: list[BaseException] = []

    def branch() -> None:
        try:
            for _ in range(20):
                log.on_start("prime.each.iter_N.t", {})
                log.on_complete("prime.each.iter_N.t", {"meta": {"error": None}})
        except BaseException as exc:
            errors.append(exc)

    threads = [threading.Thread(target=branch) for _ in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    log.close()

    assert not errors
    events = _lines(tmp_path / "events.jsonl")
    starts = sorted(e["id"] for e in events if e["ev"] == "start")
    ends = sorted(e["id"] for e in events if e["ev"] == "end")
    assert len(starts) == 160
    assert starts == ends == sorted(set(starts))  # every id appears exactly once each way


def test_end_ok_true_when_meta_error_is_none(tmp_path: Path) -> None:
    log = EventLog(tmp_path / "events.jsonl")
    log.on_start("prime.step", {})
    log.on_complete("prime.step", {"value": "x", "meta": {"error": None}})
    log.close()

    end = next(e for e in _lines(tmp_path / "events.jsonl") if e["ev"] == "end")
    assert end["ok"] is True
    assert "error" not in end
    assert end["ms"] >= 0


def test_end_ok_false_carries_truncated_error(tmp_path: Path) -> None:
    long_error = "x" * 600
    log = EventLog(tmp_path / "events.jsonl")
    log.on_start("prime.step", {})
    log.on_complete("prime.step", {"value": None, "meta": {"error": long_error}})
    log.close()

    end = next(e for e in _lines(tmp_path / "events.jsonl") if e["ev"] == "end")
    assert end["ok"] is False
    assert end["error"] == "x" * 500
    assert len(end["error"]) == 500


def test_complete_without_a_matching_start_gets_a_null_id_and_no_crash(
    tmp_path: Path,
) -> None:
    """A completion this instance never saw the start of -- e.g. a
    composed observer upstream raised and the runtime's own `finally:`
    fired `on_complete` again for the same instance (#419 review) -- is
    not an error: it writes `end` with `id: null` and no `ms`, and the
    stream keeps going."""
    log = EventLog(tmp_path / "events.jsonl")
    log.on_complete("prime.step", {"meta": {"error": None}})
    log.run_start(run_id="r1", orchestration="o.yml")  # still writing
    log.close()

    events = _lines(tmp_path / "events.jsonl")
    end = events[0]
    assert end["ev"] == "end"
    assert end["id"] is None
    assert "ms" not in end
    assert end["ok"] is True
    assert events[1]["ev"] == "run_start"


def test_a_double_complete_is_not_an_error(tmp_path: Path) -> None:
    log = EventLog(tmp_path / "events.jsonl")
    log.on_start("prime.step", {})
    log.on_complete("prime.step", {"meta": {"error": None}})
    log.on_complete("prime.step", {"meta": {"error": None}})  # the double-fire
    log.close()

    ends = [e for e in _lines(tmp_path / "events.jsonl") if e["ev"] == "end"]
    assert len(ends) == 2
    assert ends[0]["id"] is not None and "ms" in ends[0]
    assert ends[1]["id"] is None and "ms" not in ends[1]


def test_run_end_ok_omits_error_and_signal(tmp_path: Path) -> None:
    log = EventLog(tmp_path / "events.jsonl")
    log.run_end(ok=True, error=None, signal=None)
    log.close()

    run_end = _lines(tmp_path / "events.jsonl")[0]
    assert run_end == {
        "v": 1,
        "seq": 0,
        "ts": run_end["ts"],
        "ev": "run_end",
        "ok": True,
    }


def test_run_end_failure_carries_truncated_error_and_signal(tmp_path: Path) -> None:
    long_error = "boom " * 200
    log = EventLog(tmp_path / "events.jsonl")
    log.run_end(ok=False, error=long_error, signal="SIGINT")
    log.close()

    run_end = _lines(tmp_path / "events.jsonl")[0]
    assert run_end["ok"] is False
    assert run_end["error"] == long_error[:500]
    assert run_end["signal"] == "SIGINT"


def test_file_is_created_or_truncated_at_construction(tmp_path: Path) -> None:
    path = tmp_path / "events.jsonl"
    path.write_text("stale leftover line\n", encoding="utf-8")
    log = EventLog(path)
    log.run_start(run_id="r1", orchestration="o.yml")
    log.close()

    lines = path.read_text(encoding="utf-8").splitlines()
    assert len(lines) == 1
    assert "stale" not in lines[0]


def test_creates_missing_parent_directories(tmp_path: Path) -> None:
    path = tmp_path / "deep" / "nested" / "events.jsonl"
    log = EventLog(path)
    log.run_start(run_id="r1", orchestration="o.yml")
    assert log.close() is False
    assert path.exists()


def test_unwritable_path_disables_writes_without_raising(tmp_path: Path) -> None:
    blocker = tmp_path / "not-a-dir"
    blocker.write_text("", encoding="utf-8")
    log = EventLog(blocker / "events.jsonl")

    # Every call is a no-op, never an exception -- writing this stream must
    # never fail the run.
    log.run_start(run_id="r1", orchestration="o.yml")
    log.on_start("prime.step", {})
    log.on_complete("prime.step", {"meta": {"error": None}})
    log.run_end(ok=True, error=None, signal=None)
    assert log.close() is True


def test_a_write_failure_mid_run_disables_further_writes(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    log = EventLog(tmp_path / "events.jsonl")
    log.run_start(run_id="r1", orchestration="o.yml")

    real_write = log._file.write
    calls = {"n": 0}

    def flaky_write(data: str) -> int:
        calls["n"] += 1
        raise OSError("disk full")

    monkeypatch.setattr(log._file, "write", flaky_write)
    log.on_start("prime.step", {})  # fails to write, disables further writes
    monkeypatch.setattr(log._file, "write", real_write)
    log.on_complete("prime.step", {"meta": {"error": None}})  # still disabled
    assert log.close() is True

    assert calls["n"] == 1
    lines = _lines(tmp_path / "events.jsonl")
    assert len(lines) == 1
    assert lines[0]["ev"] == "run_start"


def test_close_returns_false_when_nothing_ever_failed(tmp_path: Path) -> None:
    log = EventLog(tmp_path / "events.jsonl")
    log.run_start(run_id="r1", orchestration="o.yml")
    assert log.close() is False


def test_an_unexpected_non_oserror_also_disables_further_writes(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Not just an `OSError` writing the file -- any unexpected exception
    anywhere in a method's body must not escape and must not re-fail on
    every subsequent call (#419 review)."""
    log = EventLog(tmp_path / "events.jsonl")
    log.run_start(run_id="r1", orchestration="o.yml")

    def boom(data: str) -> int:
        raise RuntimeError("not an OSError")

    monkeypatch.setattr(log._file, "write", boom)
    log.on_start("prime.step", {})  # must not raise
    log.on_complete("prime.step", {"meta": {"error": None}})  # already disabled
    assert log.close() is True

    lines = _lines(tmp_path / "events.jsonl")
    assert len(lines) == 1
    assert lines[0]["ev"] == "run_start"


def test_no_torn_lines_under_concurrent_writers(tmp_path: Path) -> None:
    log = EventLog(tmp_path / "events.jsonl")

    def writer(n: int) -> None:
        for i in range(50):
            log.on_dispatch(f"prime.w{n}", i, i)

    threads = [threading.Thread(target=writer, args=(n,)) for n in range(6)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    log.close()

    # Every line parses as exactly one complete JSON object -- a torn write
    # would either fail json.loads or merge two objects onto one line.
    text = (tmp_path / "events.jsonl").read_text(encoding="utf-8")
    raw_lines = text.splitlines()
    assert len(raw_lines) == 300
    for line in raw_lines:
        json.loads(line)
