"""CEL fails loud — issue #184.

Two halves, matching the two ways a bad expression used to disappear:

* **Compile time.** ``cof check`` parses every ``mode: cel`` expression
  with the real CEL parser, so an expression that is not CEL is rejected
  before a run dispatches instead of quietly meaning "false" forever.
* **Run time.** An expression that cannot be evaluated errors the effect
  (``meta.error``) and ends the run ``ok=False``, the same shape a failing
  tool effect has — never a silent ``else`` branch.

The line is drawn at the *expression*: an unset ``state.`` path is data,
not a defect, and still reads false (``tests/core/test_disabled_effects``
pins that contract). See ``core.cel_eval``'s module docstring.

Since #185 the evaluator is ``cel-python``, so the constructs #184 had to
reject by name — ``!``, ``has()``, the comprehension macros, ``?:`` — are
supported and must now *pass* ``cof check``. That reversal is asserted
here on purpose.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.adapters.base import GenerateResult
from circuitry.api import run_orchestration
from circuitry.cli.app import app
from circuitry.core.cel_eval import CelEvaluationError
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store

runner = CliRunner()

# An expression that passes compile-time validation (it parses) but cannot be
# evaluated: ``size()`` of an integer. Stands in for every typo that only
# shows up against real state.
BROKEN_AT_RUNTIME = "size(state.input.n) > 0"


@dataclass(frozen=True)
class EchoAdapter:
    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=prompt, raw={"model": model, "prompt": prompt})


def _flat(text: str) -> str:
    """Rich wraps console output at the terminal width; assert on the words."""
    return " ".join(text.split())


def _write(tmp_path: Path, name: str, content: str) -> Path:
    path = tmp_path / name
    path.write_text(content.strip() + "\n", encoding="utf-8")
    return path


def _conditional_orch(expr: str, *, on_error: str | None = None) -> dict:
    gate: dict = {
        "type": "conditional",
        "name": "gate",
        "if": {"mode": "cel", "expr": expr},
        "then": [{"type": "prompt", "name": "taken", "template": "then"}],
        "else": [{"type": "prompt", "name": "skipped", "template": "else"}],
    }
    if on_error is not None:
        gate["on_error"] = on_error
    return {"effects": [gate]}


# ── compile time: cof check ──────────────────────────────────────────────────


#: Real CEL that #184 had to reject by name and #185 makes work.
ONCE_UNSUPPORTED_CEL = [
    "!state.prime.x.value",
    "has(state.input.name)",
    "size(state.input.items.filter(i, i > 1)) > 0",
    "state.input.ok ? true : false",
    "state.input.items.all(i, i > 0)",
    "state.input.items.exists_one(i, i == 1)",
]


@pytest.mark.parametrize("expr", ONCE_UNSUPPORTED_CEL)
def test_check_accepts_the_cel_the_translator_could_not_run(
    tmp_path: Path, expr: str
) -> None:
    orch = _write(
        tmp_path,
        "orch.yml",
        f"""
adapter: ollama
model: phi3:mini
effects:
  - type: conditional
    name: gate
    if:
      mode: cel
      expr: "{expr}"
    then:
      - type: prompt
        name: taken
        template: "t"
""",
    )

    result = runner.invoke(app, ["check", str(orch), "--skip-preflight"])

    assert result.exit_code == 0, result.stdout


#: Not CEL at all — a Python comprehension and a bare Python `not`.
NOT_CEL = [
    "[x for x in state.input.items]",
    "not state.input.ok",
]


@pytest.mark.parametrize("expr", NOT_CEL)
def test_check_rejects_non_cel_naming_effect_and_expression(
    tmp_path: Path, expr: str
) -> None:
    orch = _write(
        tmp_path,
        "orch.yml",
        f"""
adapter: ollama
model: phi3:mini
effects:
  - type: conditional
    name: gate
    if:
      mode: cel
      expr: "{expr}"
    then:
      - type: prompt
        name: taken
        template: "t"
""",
    )

    result = runner.invoke(app, ["check", str(orch), "--skip-preflight"])

    assert result.exit_code == 1
    output = _flat(result.stdout)
    assert "does not parse" in output
    assert "(effect 'gate')" in output


def test_check_rejects_unparseable_cel(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "orch.yml",
        """
adapter: ollama
model: phi3:mini
effects:
  - type: conditional
    name: gate
    if:
      mode: cel
      expr: "state.input.a =="
    then:
      - type: prompt
        name: taken
        template: "t"
""",
    )

    result = runner.invoke(app, ["check", str(orch), "--skip-preflight"])

    assert result.exit_code == 1
    assert "does not parse" in _flat(result.stdout)


def test_check_rejects_non_cel_in_a_loop_while(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "orch.yml",
        """
adapter: ollama
model: phi3:mini
effects:
  - type: loop
    name: spin
    max_iterations: 2
    while:
      mode: cel
      expr: "not state.prime.step.value"
    body:
      - type: prompt
        name: step
        template: "s"
""",
    )

    result = runner.invoke(app, ["check", str(orch), "--skip-preflight"])

    assert result.exit_code == 1
    output = _flat(result.stdout)
    assert "Loop while CEL expression" in output
    assert "does not parse" in output
    assert "(effect 'spin')" in output


def test_check_accepts_negation_in_a_loop_while(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "orch.yml",
        """
adapter: ollama
model: phi3:mini
effects:
  - type: loop
    name: spin
    max_iterations: 2
    while:
      mode: cel
      expr: "!state.prime.step.value"
    body:
      - type: prompt
        name: step
        template: "s"
""",
    )

    result = runner.invoke(app, ["check", str(orch), "--skip-preflight"])

    assert result.exit_code == 0, result.stdout


def test_check_accepts_supported_cel(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "orch.yml",
        """
adapter: ollama
model: phi3:mini
effects:
  - type: conditional
    name: gate
    if:
      mode: cel
      expr: "size(state.input.items) > 0 && state.input.ok == true"
    then:
      - type: prompt
        name: taken
        template: "t"
""",
    )

    result = runner.invoke(app, ["check", str(orch), "--skip-preflight"])

    assert result.exit_code == 0, result.stdout


# ── run time: the effect errors, the run fails ───────────────────────────────


def test_runtime_cel_error_records_meta_error_and_raises() -> None:
    root = compile_orchestration(
        orch=_conditional_orch(BROKEN_AT_RUNTIME), root_name="prime"
    )
    store = Store({"input": {"n": 3}})

    with pytest.raises(RuntimeError) as excinfo:
        DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
            store=store
        )

    # The container re-raises with the effect path; the typed error is the cause.
    assert "prime.gate" in str(excinfo.value)
    cause = excinfo.value.__cause__
    assert isinstance(cause, CelEvaluationError)
    assert cause.expression == BROKEN_AT_RUNTIME
    meta = store.get("prime.gate.meta")
    assert BROKEN_AT_RUNTIME in meta["error"]
    assert meta["completed_at"]


def test_runtime_cel_error_never_silently_takes_the_else_branch() -> None:
    """Regression: the silent-false hazard.

    A condition that cannot be evaluated used to return ``False``, which is
    indistinguishable from a legitimately false condition — every run
    quietly took ``else``. The branch must not be recorded at all now.
    """
    root = compile_orchestration(
        orch=_conditional_orch(BROKEN_AT_RUNTIME), root_name="prime"
    )
    store = Store({"input": {"n": 3}})

    with pytest.raises(RuntimeError):
        DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
            store=store
        )

    meta = store.get("prime.gate.meta")
    assert "branch" not in meta
    assert "condition_result" not in meta
    assert store.get("prime.gate.skipped") is None
    assert store.get("prime.gate.taken") is None


def test_run_finishes_not_ok_with_the_error_reported(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "orch.yml",
        f"""
adapter: ollama
model: phi3:mini
effects:
  - type: conditional
    name: gate
    if:
      mode: cel
      expr: "{BROKEN_AT_RUNTIME}"
    then:
      - type: prompt
        name: taken
        template: "t"
    else:
      - type: prompt
        name: skipped
        template: "e"
""",
    )

    result = run_orchestration(
        orchestration_path=orch,
        state={"n": 3},
        dry_run=True,
        raise_on_error=False,
    )

    assert result.ok is False
    assert BROKEN_AT_RUNTIME in (result.error or "")


def test_on_error_continue_takes_else_but_still_records_the_error() -> None:
    """Falling to ``else`` is now an explicit opt-in, and still evidenced."""
    root = compile_orchestration(
        orch=_conditional_orch(BROKEN_AT_RUNTIME, on_error="continue"),
        root_name="prime",
    )
    store = Store({"input": {"n": 3}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    meta = store.get("prime.gate.meta")
    assert meta["branch"] == "else"
    assert BROKEN_AT_RUNTIME in meta["error"]


def test_on_error_skip_records_the_error_and_runs_no_branch() -> None:
    root = compile_orchestration(
        orch=_conditional_orch(BROKEN_AT_RUNTIME, on_error="skip"),
        root_name="prime",
    )
    store = Store({"input": {"n": 3}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    assert store.get("prime.gate.value") == {
        "result": None,
        "branch": None,
        "effects": {},
    }
    assert BROKEN_AT_RUNTIME in store.get("prime.gate.meta")["error"]


# ── run time: loop while conditions ──────────────────────────────────────────


def _loop_orch(expr: str, *, on_error: str | None = None) -> dict:
    loop: dict = {
        "type": "loop",
        "name": "spin",
        "max_iterations": 3,
        "while": {"mode": "cel", "expr": expr},
        "body": [{"type": "prompt", "name": "step", "template": "s"}],
    }
    if on_error is not None:
        loop["on_error"] = on_error
    return {"effects": [loop]}


def test_loop_while_cel_error_fails_the_loop() -> None:
    root = compile_orchestration(orch=_loop_orch(BROKEN_AT_RUNTIME), root_name="prime")
    store = Store({"input": {"n": 3}})

    with pytest.raises(RuntimeError):
        DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(
            store=store
        )

    assert BROKEN_AT_RUNTIME in store.get("prime.spin.meta")["error"]
    assert store.get("prime.spin.value")["termination"]["reason"] == "error"


def test_loop_while_cel_error_under_on_error_break_stops_loudly() -> None:
    """``break``/``continue`` stop the loop rather than spinning to
    ``max_iterations`` against a condition that can never become false."""
    root = compile_orchestration(
        orch=_loop_orch(BROKEN_AT_RUNTIME, on_error="break"), root_name="prime"
    )
    store = Store({"input": {"n": 3}})

    DynamicRuntime(root, adapter=EchoAdapter(), model="unit-test").execute(store=store)

    value = store.get("prime.spin.value")
    assert value["iterations"] == 0
    assert value["termination"]["reason"] == "condition_error"
    assert BROKEN_AT_RUNTIME in store.get("prime.spin.meta")["error"]
