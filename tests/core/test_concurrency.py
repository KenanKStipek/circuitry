"""`RunConcurrencyLimiter` (#274): a run-wide cap plus named resource groups."""

from __future__ import annotations

import threading
import time
from concurrent.futures import ThreadPoolExecutor

import pytest

from circuitry.core.concurrency import (
    RunConcurrencyLimiter,
    UnknownConcurrencyGroupError,
    parse_concurrency_groups,
    parse_max_concurrency,
)


class _Tracker:
    """Records how many `with` bodies were inside the limiter at once.

    An optional ``barrier`` passed to ``work``, sized to the concurrency a
    test expects, makes the peak deterministic instead of inferred from
    the wall clock (#456): a body waits on it while counted as in flight,
    so a lower peak than expected times out with ``BrokenBarrierError``
    instead of racing a sleep.
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._in_flight = 0
        self.max_in_flight = 0

    def work(self, delay: float = 0.03, barrier: threading.Barrier | None = None) -> None:
        with self._lock:
            self._in_flight += 1
            self.max_in_flight = max(self.max_in_flight, self._in_flight)
        if barrier is not None:
            barrier.wait()
        time.sleep(delay)
        with self._lock:
            self._in_flight -= 1


def test_max_concurrency_serializes_acquirers() -> None:
    limiter = RunConcurrencyLimiter(max_concurrency=1)
    tracker = _Tracker()

    def run_one() -> None:
        with limiter.acquire(group=None):
            tracker.work()

    with ThreadPoolExecutor(max_workers=5) as pool:
        list(pool.map(lambda _: run_one(), range(5)))

    assert tracker.max_in_flight == 1


def test_unset_max_concurrency_imposes_no_cap() -> None:
    """A barrier sized to all five acquirers (#456), not a sleep, proves
    the peak: on a slow runner the fifth acquirer can start after the
    first has already finished."""
    limiter = RunConcurrencyLimiter()
    tracker = _Tracker()
    barrier = threading.Barrier(5, timeout=5)

    def run_one() -> None:
        with limiter.acquire(group=None):
            tracker.work(delay=0.05, barrier=barrier)

    with ThreadPoolExecutor(max_workers=5) as pool:
        list(pool.map(lambda _: run_one(), range(5)))

    assert tracker.max_in_flight == 5


def test_group_cap_serializes_within_the_named_group() -> None:
    limiter = RunConcurrencyLimiter(groups={"gpu": 1})
    tracker = _Tracker()

    def run_one() -> None:
        with limiter.acquire(group="gpu"):
            tracker.work()

    with ThreadPoolExecutor(max_workers=5) as pool:
        list(pool.map(lambda _: run_one(), range(5)))

    assert tracker.max_in_flight == 1


def test_different_groups_run_independently() -> None:
    """A barrier shared by both groups (#456), not a sleep, proves the two
    ran at once: on a slow runner one call can finish before the other
    even starts, hiding exactly the cross-group blocking this test exists
    to catch.
    """
    limiter = RunConcurrencyLimiter(groups={"gpu": 1, "cpu": 1})
    gpu_tracker = _Tracker()
    cpu_tracker = _Tracker()
    combined_lock = threading.Lock()
    combined_count = 0
    max_combined = 0
    barrier = threading.Barrier(2, timeout=5)

    def bump(delta: int) -> None:
        nonlocal combined_count, max_combined
        with combined_lock:
            combined_count += delta
            max_combined = max(max_combined, combined_count)

    def run_gpu() -> None:
        with limiter.acquire(group="gpu"):
            bump(1)
            gpu_tracker.work(delay=0.05, barrier=barrier)
            bump(-1)

    def run_cpu() -> None:
        with limiter.acquire(group="cpu"):
            bump(1)
            cpu_tracker.work(delay=0.05, barrier=barrier)
            bump(-1)

    with ThreadPoolExecutor(max_workers=2) as pool:
        f1 = pool.submit(run_gpu)
        f2 = pool.submit(run_cpu)
        f1.result()
        f2.result()

    assert gpu_tracker.max_in_flight == 1
    assert cpu_tracker.max_in_flight == 1
    # Both groups' slots were held at once — one group's cap never blocked
    # the other's.
    assert max_combined == 2


def test_global_cap_and_group_cap_compose() -> None:
    """runtime.max_concurrency and a group both apply at once: a grouped
    effect counts against both the group's own cap and the run-wide one.

    A barrier sized to the global cap (#456), not a sleep, proves hitting
    it: four acquirers (an even multiple of the cap, so the barrier's two
    waves each fill exactly) arrive in two waves of two rather than a
    sleep-window race deciding how many overlap.
    """
    limiter = RunConcurrencyLimiter(max_concurrency=2, groups={"gpu": 4})
    tracker = _Tracker()
    barrier = threading.Barrier(2, timeout=5)

    def run_one() -> None:
        with limiter.acquire(group="gpu"):
            tracker.work(barrier=barrier)

    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(lambda _: run_one(), range(4)))

    # The group alone would allow 4 at once; the run-wide cap of 2 still wins.
    assert tracker.max_in_flight == 2


def test_group_waiters_do_not_hold_global_slots() -> None:
    """Group acquisition happens before the global one (#274 review P1):
    a leaf queued behind a full group must not occupy a global slot while
    it waits, or it would starve every ungrouped leaf in the run."""
    limiter = RunConcurrencyLimiter(max_concurrency=2, groups={"gpu": 1})

    gpu_holder_ready = threading.Event()
    release_gpu_holder = threading.Event()

    def hold_gpu() -> None:
        with limiter.acquire(group="gpu"):
            gpu_holder_ready.set()
            release_gpu_holder.wait(timeout=5)

    holder = threading.Thread(target=hold_gpu)
    holder.start()
    gpu_holder_ready.wait(timeout=5)

    # Two more gpu leaves queue behind the group's single slot. If group
    # acquisition happened after the global one, each would first grab one
    # of the two global slots and then block — leaving none for the
    # ungrouped leaf below.
    def queue_gpu() -> None:
        with limiter.acquire(group="gpu"):
            pass

    queued = [threading.Thread(target=queue_gpu) for _ in range(2)]
    for t in queued:
        t.start()

    # Give the queued threads a moment to reach (and block on) the group
    # semaphore before checking the ungrouped leaf.
    time.sleep(0.1)

    ungrouped_ran = threading.Event()
    with limiter.acquire(group=None):
        ungrouped_ran.set()
    assert ungrouped_ran.is_set()

    release_gpu_holder.set()
    holder.join(timeout=5)
    for t in queued:
        t.join(timeout=5)
    assert not holder.is_alive()
    assert all(not t.is_alive() for t in queued)


def test_unknown_group_raises() -> None:
    limiter = RunConcurrencyLimiter(groups={"gpu": 1})
    with pytest.raises(UnknownConcurrencyGroupError, match="comfy"):
        with limiter.acquire(group="comfy"):
            pass


def test_on_wait_and_on_acquired_fire_only_when_actually_blocked() -> None:
    limiter = RunConcurrencyLimiter(max_concurrency=1)
    waits: list[str] = []
    acquisitions = 0

    def on_wait(label: str) -> None:
        waits.append(label)

    def on_acquired() -> None:
        nonlocal acquisitions
        acquisitions += 1

    # First acquirer never has to wait.
    with limiter.acquire(group=None, on_wait=on_wait, on_acquired=on_acquired):
        pass
    assert waits == []
    assert acquisitions == 0

    # A second acquirer blocked behind the first does.
    holder_ready = threading.Event()
    release_holder = threading.Event()

    def hold() -> None:
        with limiter.acquire(group=None):
            holder_ready.set()
            release_holder.wait(timeout=5)

    t = threading.Thread(target=hold)
    t.start()
    holder_ready.wait(timeout=5)
    with limiter.acquire(group=None, on_wait=on_wait, on_acquired=on_acquired):
        pass
    release_holder.set()
    t.join(timeout=5)

    assert waits == ["global"]
    assert acquisitions == 1


def test_parse_max_concurrency_rejects_non_positive_and_non_int() -> None:
    value, errors = parse_max_concurrency(0)
    assert value is None and errors

    value, errors = parse_max_concurrency(-1)
    assert value is None and errors

    value, errors = parse_max_concurrency("4")
    assert value is None and errors

    value, errors = parse_max_concurrency(None)
    assert value is None and errors == []

    value, errors = parse_max_concurrency(4)
    assert value == 4 and errors == []


def test_parse_concurrency_groups_rejects_malformed_entries() -> None:
    groups, errors = parse_concurrency_groups({"gpu": 1, "comfy": 2})
    assert groups == {"gpu": 1, "comfy": 2}
    assert errors == []

    groups, errors = parse_concurrency_groups({"gpu": 0})
    assert groups == {} and errors

    groups, errors = parse_concurrency_groups({"gpu": "one"})
    assert groups == {} and errors

    groups, errors = parse_concurrency_groups("not-a-mapping")
    assert groups == {} and errors

    groups, errors = parse_concurrency_groups(None)
    assert groups == {} and errors == []
