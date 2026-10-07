"""Issue #263 part 2: retries are classified, back off exponentially with
jitter, and honour a provider's ``Retry-After``.

``AlwaysFailAdapter``-style doubles raise a bare ``RuntimeError`` elsewhere
in this test suite to mean "every attempt fails"; here the point is the
classification itself, so each double raises :class:`AdapterCallError` with
an explicit :class:`RetryInfo` to stand in for a provider's actual response.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import pytest

from circuitry.adapters._retry import AdapterCallError, RetryInfo
from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


def _patch_sleep(monkeypatch: pytest.MonkeyPatch, fn) -> None:
    """Patch the cancellation-aware retry wait (#356), not a bare time.sleep."""
    from circuitry.core.cancellation import get_token

    monkeypatch.setattr(get_token(), "sleep_or_raise", fn)


@dataclass
class ScriptedRetryAdapter:
    """Raises ``AdapterCallError`` per ``failures``, then succeeds."""

    name: str = "scripted"
    failures: list[RetryInfo] = field(default_factory=list)
    calls: int = 0

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls += 1
        if self.calls <= len(self.failures):
            raise AdapterCallError(
                f"attempt {self.calls} failed", retry_info=self.failures[self.calls - 1]
            )
        return GenerateResult(text="ok", raw={})


def _retries_orch(max_attempts: int, backoff_ms: int = 0) -> dict:
    return {
        "effects": [
            {
                "type": "prompt",
                "name": "task",
                "template": "hi",
                "retries": {"max_attempts": max_attempts, "backoff_ms": backoff_ms},
            }
        ]
    }


def test_a_429_is_retried_and_eventually_succeeds() -> None:
    root = compile_orchestration(orch=_retries_orch(max_attempts=2), root_name="prime")
    adapter = ScriptedRetryAdapter(failures=[RetryInfo(retryable=True, status=429)])
    store = Store({})

    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    assert adapter.calls == 2
    assert store.get("prime.task.value") == "ok"
    assert store.get("prime.task.meta.retries_used") == 1


def test_a_400_fails_immediately_without_spending_the_retry_budget() -> None:
    """Non-retryable errors must not retry even when attempts remain \u2014
    retrying a 400 can never succeed."""
    root = compile_orchestration(orch=_retries_orch(max_attempts=3), root_name="prime")
    adapter = ScriptedRetryAdapter(
        failures=[
            RetryInfo(retryable=False, status=400),
            RetryInfo(retryable=False, status=400),
            RetryInfo(retryable=False, status=400),
        ]
    )
    store = Store({})

    with pytest.raises(RuntimeError):
        DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    assert adapter.calls == 1
    error = store.get("prime.task.meta.error")
    assert "attempt 1 failed" in error


def test_backoff_honours_retry_after_seconds(monkeypatch: pytest.MonkeyPatch) -> None:
    root = compile_orchestration(
        orch=_retries_orch(max_attempts=2, backoff_ms=1000), root_name="prime"
    )
    adapter = ScriptedRetryAdapter(
        failures=[RetryInfo(retryable=True, status=429, retry_after="7")]
    )
    store = Store({})

    sleeps: list[float] = []
    _patch_sleep(monkeypatch, sleeps.append)

    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    assert sleeps == [7.0]


def test_backoff_without_retry_after_grows_and_is_capped(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from circuitry.adapters import _retry as retry_mod

    monkeypatch.setattr(retry_mod.random, "uniform", lambda lo, hi: hi)

    root = compile_orchestration(
        orch=_retries_orch(max_attempts=3, backoff_ms=1000), root_name="prime"
    )
    adapter = ScriptedRetryAdapter(
        failures=[
            RetryInfo(retryable=True, status=503),
            RetryInfo(retryable=True, status=503),
        ]
    )
    store = Store({})

    sleeps: list[float] = []
    _patch_sleep(monkeypatch, sleeps.append)

    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    # With jitter forced to its ceiling: first retry waits ~backoff_ms (1s),
    # second roughly doubles (~2s) — capped at _RETRY_BACKOFF_CAP_MS overall.
    assert sleeps == [1.0, 2.0]


def test_backoff_at_a_large_attempt_index_stays_at_the_cap(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The exponential curve would blow past any sane wait by attempt 10
    (``backoff_ms * 2**9`` from a 1s base is over 8 minutes); the cap must
    hold regardless of how many attempts preceded it."""
    from circuitry.adapters import _retry as retry_mod

    monkeypatch.setattr(retry_mod.random, "uniform", lambda lo, hi: hi)

    root = compile_orchestration(
        orch=_retries_orch(max_attempts=10, backoff_ms=1000), root_name="prime"
    )
    adapter = ScriptedRetryAdapter(
        failures=[RetryInfo(retryable=True, status=503) for _ in range(9)]
    )
    store = Store({})

    sleeps: list[float] = []
    _patch_sleep(monkeypatch, sleeps.append)

    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    # _RETRY_BACKOFF_CAP_MS is 60s; by the 7th retry the uncapped exponential
    # curve (1000 * 2**6 = 64000ms) would already exceed it.
    assert max(sleeps) == 60.0
    assert sleeps[-1] == 60.0


def test_an_unclassified_error_is_not_retried() -> None:
    """An adapter that doesn't classify its own failure (a bare
    ``RuntimeError``) is treated as not retryable \u2014 the conservative
    default matches pre-#263 behaviour of never retrying anything by
    default, since the default ``retries`` is still 0."""

    @dataclass
    class PlainFailAdapter:
        name: str = "plain"
        calls: int = 0

        def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120):
            self.calls += 1
            raise RuntimeError("unclassified failure")

    root = compile_orchestration(orch=_retries_orch(max_attempts=3), root_name="prime")
    adapter = PlainFailAdapter()
    store = Store({})

    with pytest.raises(RuntimeError):
        DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)

    assert adapter.calls == 1
