"""Standalone driver for the streaming-SIGINT test
(`test_run_cancel_signal_delivery.py`): runs one `prompt` effect through the
`ollama` adapter against a caller-given `base_url`, wrapped in
`circuitry.cli.interrupts.sigterm_as_interrupt` exactly like `cof run`
itself, and exits with the same 130/143/129/1/0 convention `cli.app` uses.

Usage: ``run_streaming_prompt.py <orchestration.yml> <base_url>``
"""

from __future__ import annotations

import sys
from pathlib import Path

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.interrupts import sigterm_as_interrupt
from circuitry.cli.runtime_shim import RunRequest, run


def main() -> int:
    orch_path = Path(sys.argv[1])
    base_url = sys.argv[2]

    req = RunRequest(
        orchestration_path=orch_path,
        state_path=None,
        out_path=None,
        dry_run=False,
        validate_only=False,
        initial_state={},
        config=CircuitryConfig(
            default_adapter="ollama",
            default_model="m",
            runtime={"adapters": {"ollama": {"base_url": base_url}}},
        ),
        skip_preflight=True,
    )
    with sigterm_as_interrupt():
        result = run(req)

    if not result.ok:
        if not (result.interrupted or result.sigterm or result.sighup):
            print(f"run failed: {result.error}", file=sys.stderr)
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
