from __future__ import annotations

import json
import threading
import time
from dataclasses import dataclass
from itertools import pairwise
from pathlib import Path
from typing import Any

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.cli import live_state
from circuitry.cli.app import _write_state_json
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.live_state import (
    LIVE_STATE_INTERVAL_SECONDS,
    LiveStateMirror,
    write_live_state,
)
from circuitry.cli.runtime_shim import RunRequest, run
from circuitry.core.store import Store


def test_write_live_state_creates_valid_json(tmp_path: Path):
    target = tmp_path / "state.json"
    state = {"prime": {"greet": {"value": "hello"}}}
    write_live_state(target, state)

    assert target.exists()
    parsed = json.loads(target.read_text(encoding="utf-8"))
    assert parsed == state


def test_write_live_state_creates_parent_dirs(tmp_path: Path):
    target = tmp_path / "deep" / "nested" / "state.json"
    write_live_state(target, {"ok": True})

    assert target.exists()
    assert json.loads(target.read_text(encoding="utf-8")) == {"ok": True}


def test_write_live_state_no_tmp_file_remains(tmp_path: Path):
    target = tmp_path / "state.json"
    write_live_state(target, {"a": 1})

    tmp_file = target.with_suffix(".tmp")
    assert not tmp_file.exists(), ".tmp file should not remain after atomic rename"


def test_write_live_state_overwrites_existing(tmp_path: Path):
    target = tmp_path / "state.json"
    write_live_state(target, {"version": 1})
    write_live_state(target, {"version": 2})

    parsed = json.loads(target.read_text(encoding="utf-8"))
    assert parsed["version"] == 2


def _wait_for(predicate: Any, timeout: float = 5.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.01)
    return predicate()


def _read(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


@pytest.fixture
def serialisations(monkeypatch: pytest.MonkeyPatch) -> list[float]:
    """When each full serialisation of the mirror happened (monotonic time)."""
    stamps: list[float] = []
    real = live_state.dumps_saved_state

    def counting(state: Any, **kwargs: Any) -> str:
        stamps.append(time.monotonic())
        return real(state, **kwargs)

    monkeypatch.setattr(live_state, "dumps_saved_state", counting)
    return stamps


def test_mirror_writes_a_snapshot_without_waiting_for_more(tmp_path: Path) -> None:
    target = tmp_path / "live.json"
    mirror = LiveStateMirror(target, store_lock=threading.RLock())
    mirror({"step": 1})
    assert _wait_for(lambda: target.exists() and _read(target) == {"step": 1})
    mirror.close({"step": 1})


def test_mirror_coalesces_writes_to_one_per_interval(
    tmp_path: Path, serialisations: list[float]
) -> None:
    target = tmp_path / "live.json"
    interval = 0.2
    mirror = LiveStateMirror(target, store_lock=threading.RLock(), interval=interval)
    started = time.monotonic()
    for step in range(300):
        mirror({"step": step})
        time.sleep(0.002)
    elapsed = time.monotonic() - started
    # The trailing write lands within the interval, with the newest snapshot.
    assert _wait_for(lambda: _read(target) == {"step": 299})
    mirror.close({"step": "final"})

    # Bounded by run time / interval, plus the leading write and the final one.
    assert len(serialisations) <= elapsed / interval + 3
    gaps = [b - a for a, b in pairwise(serialisations[:-1])]
    assert all(gap >= interval * 0.9 for gap in gaps)
    assert _read(target) == {"step": "final"}


def test_close_writes_the_final_state_and_stops_the_writer(tmp_path: Path) -> None:
    target = tmp_path / "live.json"
    mirror = LiveStateMirror(target, store_lock=threading.RLock(), interval=60)
    mirror({"step": 1})
    mirror({"step": 2})  # coalesced: the interval has not passed
    mirror.close({"step": "final"})
    assert _read(target) == {"step": "final"}
    assert not mirror._thread.is_alive()
    mirror.close({"step": "again"})  # idempotent
    assert _read(target) == {"step": "final"}


def test_store_writes_never_wait_on_the_mirrors_disk_io(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A slow disk holds up the writer thread, not ``Store.set``."""
    slow_write = 0.3
    writes: list[bool] = []
    lock = threading.RLock()
    real_replace = live_state._replace_file

    def slow_replace(path: Path, payload: str) -> None:
        # Probe the store lock from another thread: free means no I/O under it.
        def probe() -> None:
            acquired = lock.acquire(timeout=1)
            writes.append(acquired)
            if acquired:
                lock.release()

        prober = threading.Thread(target=probe)
        prober.start()
        prober.join()
        time.sleep(slow_write)
        real_replace(path, payload)

    monkeypatch.setattr(live_state, "_replace_file", slow_replace)
    mirror = LiveStateMirror(tmp_path / "live.json", store_lock=lock, interval=0)
    store = Store({}, on_write=mirror, _lock=lock)

    slowest = 0.0
    for step in range(20):
        started = time.monotonic()
        store.set(f"prime.step_{step}", {"value": step})
        slowest = max(slowest, time.monotonic() - started)
        time.sleep(0.02)
    mirror.close(store.root_state)

    assert writes and all(writes)
    assert slowest < slow_write / 2


@dataclass
class _EchoAdapter:
    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        if "BOOM" in prompt:
            raise RuntimeError("scripted failure")
        return GenerateResult(text=prompt, raw={})


def _nested_loops(tail: str) -> dict[str, Any]:
    return {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "each": {"in": "input.outer_items", "as": "o"},
                "body": [
                    {
                        "type": "loop",
                        "name": "inner",
                        "each": {"in": "input.inner_items", "as": "i"},
                        "body": [
                            {"type": "prompt", "name": "step", "template": "PASS {{o}}-{{i}}"}
                        ],
                    }
                ],
            },
            {"type": "prompt", "name": "tail", "template": tail},
        ]
    }


def _run_with_mirror(tmp_path: Path, orch: dict[str, Any]) -> tuple[Any, Path, Path]:
    orch_path = tmp_path / "orch.json"
    orch_path.write_text(json.dumps(orch), encoding="utf-8")
    live = tmp_path / "live.json"
    out = tmp_path / "out.json"
    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=out,
            dry_run=False,
            validate_only=False,
            initial_state={"outer_items": ["a", "b", "c"], "inner_items": ["x", "y", "z"]},
            config=CircuitryConfig(),
            adapter=_EchoAdapter(),
            live_state_path=live,
            skip_preflight=True,
        )
    )
    # What `cof run --out` does with the result.
    _write_state_json(out=out, state=result.state, pretty=False)
    return result, live, out


def test_final_mirror_equals_the_out_state(tmp_path: Path) -> None:
    result, live, out = _run_with_mirror(
        tmp_path, _nested_loops("{{prime.outer.last.inner.last.step.value}}")
    )
    assert result.ok, result.error
    mirrored = _read(live)
    assert mirrored == _read(out)
    # Written after the last effect: the run's completion stamp is in it.
    assert mirrored["runtime"]["last_run"]["completed_at"]
    assert mirrored["prime"]["outer"]["last"] == {"$ref": "iter_2"}


def test_final_mirror_equals_the_out_state_when_the_run_fails(tmp_path: Path) -> None:
    result, live, out = _run_with_mirror(tmp_path, _nested_loops("BOOM"))
    assert not result.ok
    assert _read(live) == _read(out)


def test_a_run_serialises_the_mirror_at_most_once_per_interval(
    tmp_path: Path, serialisations: list[float]
) -> None:
    started = time.monotonic()
    result, _, _ = _run_with_mirror(tmp_path, _nested_loops("done"))
    elapsed = time.monotonic() - started
    assert result.ok, result.error
    # Nine loop passes and a tail make dozens of Store writes; the mirror
    # serialises the leading snapshot, one per elapsed interval, and the final.
    assert 1 <= len(serialisations) <= elapsed / LIVE_STATE_INTERVAL_SECONDS + 2
