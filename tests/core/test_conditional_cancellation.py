"""A named `if` whose branch is cancelled (#796) must end `ok: false`,
the same way an interrupted `dynamic`/`use`/named loop already does,
never `ok: true`.
"""

from __future__ import annotations

import signal
from unittest.mock import MagicMock

import pytest

from circuitry.core.cancellation import RunCancelledBySignal
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.resume import effect_completed_ok
from circuitry.core.store import Store


def test_named_if_branch_cancelled_records_error_and_completed_at(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def killed_plugin(**kw: object) -> object:
        m = MagicMock()
        m.execute.side_effect = RunCancelledBySignal()
        return m

    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", killed_plugin)

    orch = {
        "effects": [
            {
                "type": "conditional",
                "name": "gate",
                "if": {"mode": "cel", "expr": "true"},
                "then": [
                    {
                        "type": "tool",
                        "name": "killed",
                        "provider": "shell",
                        "params": {},
                    }
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch)
    state: dict = {}

    with pytest.raises(RunCancelledBySignal):
        DynamicRuntime(root, adapter=None, model="unit-test").execute(store=Store(state))

    node = state["prime"]["gate"]
    meta = node["meta"]
    # `ok: error is None` (`cli/events.py::EventLog.on_complete`) must
    # read `False` for this node, never `True`, once its own branch was
    # interrupted.
    assert meta.get("error") is not None
    # `completed_at` is set too, the same pair an interrupted `dynamic`/
    # `use`/named loop already records -- `effect_completed_ok` requires
    # *both* `completed_at` truthy *and* no `error`, so the truthy
    # `error` alone already keeps `cof run --resume` rerunning this
    # branch regardless.
    assert meta.get("completed_at") is not None
    assert not effect_completed_ok(node)


@pytest.mark.parametrize("message", ["run cancelled", "run cancelled during backoff", ""])
@pytest.mark.parametrize(
    ("signum", "expected"),
    [(signal.SIGTERM, "Interrupted (SIGTERM)"), (signal.SIGINT, "Interrupted (Ctrl-C/SIGINT)")],
)
def test_a_worker_threads_cancellation_message_never_becomes_a_containers_error(
    message: str, signum: int, expected: str
) -> None:
    """A `RunCancelledBySignal` a worker thread's own poll raised carries its own
    message; every container's `meta.error` uses the signal-name wording instead,
    as the root and electricity do (#796)."""
    from circuitry.core.cancellation import get_token
    from circuitry.core.dynamic import _error_text

    token = get_token()
    try:
        token.request(signum)
        assert _error_text(RunCancelledBySignal(message)) == expected
    finally:
        token.reset()
