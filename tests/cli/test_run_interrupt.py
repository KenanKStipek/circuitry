"""`cof run` and a Ctrl-C/SIGINT mid-run (#270 F6).

The engine-level behavior (`runtime_shim.run` catching `KeyboardInterrupt`
the same way it catches any other failure, writing the usual bookkeeping)
is exercised directly. The CLI-level exit-code mapping (130, not 1) is
exercised against a faked `run` the same way `test_pipe_detection.py` does,
since actually delivering SIGINT to a CliRunner invocation isn't practical.
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import patch

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.cli.runtime_shim import RunRequest, RunResult, run

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()


class InterruptingAdapter:
    """Raises KeyboardInterrupt for any prompt named in ``interrupt_on``."""

    name = "interrupting"

    def __init__(self) -> None:
        self.calls: list[str] = []
        self.interrupt_on: set[str] = set()

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        if prompt in self.interrupt_on:
            raise KeyboardInterrupt
        self.calls.append(prompt)
        return GenerateResult(text=f"ok:{prompt}", raw={})


def _write(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def _two_step_orch(tmp_path: Path) -> Path:
    return _write(
        tmp_path / "chain.yml",
        """
effects:
  - type: prompt
    name: step1
    template: "one"
  - type: prompt
    name: step2
    template: "two"
""".lstrip(),
    )


def test_run_catches_keyboard_interrupt_like_any_other_failure(tmp_path: Path) -> None:
    orch = _two_step_orch(tmp_path)
    adapter = InterruptingAdapter()
    adapter.interrupt_on = {"two"}

    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
        )
    )

    assert result.ok is False
    assert result.interrupted is True
    assert result.error is not None and "Interrupt" in result.error
    # step1 already finished before the interrupt — its state survives in
    # the returned (and, for a real --out, written) state, same as a crash.
    assert result.state["prime"]["step1"]["value"] == "ok:one"
    assert result.state["runtime"]["last_run"]["completed_at"]


def test_run_resumes_after_an_interrupted_run(tmp_path: Path) -> None:
    """The interrupted run's own state is a valid `--resume` source: step1
    is skipped, step2 (never reached) runs fresh."""
    orch = _two_step_orch(tmp_path)
    adapter = InterruptingAdapter()
    adapter.interrupt_on = {"two"}

    first = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
        )
    )
    assert first.interrupted is True
    assert adapter.calls == ["one"]

    adapter.interrupt_on = set()
    second = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=first.state,
            resume=True,
        )
    )
    assert second.ok is True, second.error
    assert adapter.calls == ["one", "two"]


def _fake_interrupted_run(state: dict):
    def _fake(req):
        return RunResult(ok=False, state=state, warnings=[], error="Interrupted (Ctrl-C/SIGINT)", interrupted=True)

    return _fake


def test_cli_exits_130_on_an_interrupted_run(tmp_path: Path) -> None:
    orch = _write(tmp_path / "orch.yml", "effects:\n  - type: tool\n    name: s\n    provider: uuid\n")
    state = {"prime": {}, "runtime": {"last_run": {}}}

    with patch("circuitry.cli.app.run", _fake_interrupted_run(state)):
        result = runner.invoke(app, ["run", str(orch)])

    assert result.exit_code == 130
    assert "interrupted" in result.stdout.lower()
