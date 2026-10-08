"""Standalone driver for the #385 follow-up "a step the kill cannot reach"
tests (`test_run_cancel_signal_delivery.py`).

Runs one orchestration through `circuitry.cli.runtime_shim.run`, wrapped in
`circuitry.cli.interrupts.sigterm_as_interrupt` exactly like `cof run`
itself, and exits with the same 130/143/129/1/0 convention `cli.app` uses —
without going through the full CLI argument parser, so this can register a
test-only tool plugin (``test_block``: writes a "started" marker, sleeps
for a fixed duration ignoring cancellation entirely, then writes a "done"
marker — a stand-in for a step a real cancellation genuinely cannot reach,
such as an in-flight MCP call or the `service` tool's own subprocess) by
adding it straight to `plugins.factory.PLUGIN_REGISTRY` before calling
`run()`, which a real `cof run` subprocess has no way to do from outside.

Usage: ``run_with_uncancellable_step.py <orchestration.yml>``
"""

from __future__ import annotations

import signal
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.interrupts import sigterm_as_interrupt
from circuitry.cli.runtime_shim import RunRequest, run
from circuitry.plugins.base import ToolResult
from circuitry.plugins.factory import PLUGIN_REGISTRY
from circuitry.preflight import CheckResult


@dataclass(frozen=True)
class _UncancellableBlockPlugin:
    """A step cancellation genuinely cannot reach: no `run_tracked`, no
    `get_token().check()` of its own -- a plain Python-level sleep, the
    same shape the orchestration reference names as one of the few things
    a cancelled run's signal handler cannot kill."""

    name: str = "test_block"

    def execute(
        self, *, params: dict[str, Any], timeout_seconds: int = 300
    ) -> ToolResult:
        del timeout_seconds
        Path(params["started"]).touch()
        # Required test 2 (#385 review P1-3): after a configurable delay,
        # this worker thread sends itself a SECOND SIGINT via
        # `signal.pthread_kill` -- standing in for the OS routing a user's
        # second Ctrl-C to this thread rather than the main thread (POSIX
        # may deliver a process-directed signal to any thread that
        # doesn't block it). CPython only ever runs the registered
        # handler on the main thread regardless of which thread the raw
        # signal reached, so this proves the fix notices it there anyway.
        second_signal_delay = params.get("second_signal_delay")
        elapsed = 0.0
        if second_signal_delay is not None:
            time.sleep(float(second_signal_delay))
            elapsed = float(second_signal_delay)
            signal.pthread_kill(threading.get_ident(), signal.SIGINT)
        time.sleep(max(0.0, float(params["seconds"]) - elapsed))
        Path(params["done"]).touch()
        return ToolResult(value=True, raw={})

    def check(self) -> CheckResult:
        return CheckResult(ok=True)


def main() -> int:
    orch_path = Path(sys.argv[1])
    PLUGIN_REGISTRY["test_block"] = lambda cfg: _UncancellableBlockPlugin()

    req = RunRequest(
        orchestration_path=orch_path,
        state_path=None,
        out_path=None,
        dry_run=False,
        validate_only=False,
        initial_state={},
        config=CircuitryConfig(),
        skip_preflight=True,
    )
    with sigterm_as_interrupt():
        result = run(req)

    if not result.ok:
        return (
            143
            if result.sigterm
            else 129
            if result.sighup
            else 130
            if result.interrupted
            else 1
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
