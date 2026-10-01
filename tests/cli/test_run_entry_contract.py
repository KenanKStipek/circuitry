"""Run-entry contract: fresh run identity, enforced `interface.inputs`, visible warnings.

Covers #253 (`_run_id`/`_timestamp` must be fresh every run, never carried
over from `--state` or a persisted snapshot), #255 (a missing `--state` file
is an error; the top-level document's own `interface.inputs` are enforced —
required/type/`default:` — the same way `use:` children already are), and
#256 (library `logger.warning()` calls reach the CLI's stderr).
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli import config as config_module
from circuitry.cli.app import app
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run

runner = CliRunner()


def _write(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content.strip() + "\n", encoding="utf-8")
    return path


def _stamp_orch(path: Path) -> Path:
    """No adapter, no interface — the #253 repro from stamp.yml."""
    return _write(
        path,
        """
effects:
  - type: tool
    name: stamp
    provider: json
    params: {mode: stringify, input: "{{_run_id}} {{_timestamp}}"}
""",
    )


def _run(orch_path: Path, **kwargs):
    defaults = {
        "orchestration_path": orch_path,
        "state_path": None,
        "out_path": None,
        "dry_run": False,
        "validate_only": False,
        "config": CircuitryConfig(),
    }
    defaults.update(kwargs)
    return run(RunRequest(**defaults))


# ---------------------------------------------------------------------------
# #253 — _run_id / _timestamp are fresh every run
# ---------------------------------------------------------------------------


def test_run_id_and_timestamp_fresh_across_state_carryover(tmp_path: Path) -> None:
    orch_path = _stamp_orch(tmp_path / "stamp.yml")

    first = _run(orch_path)
    assert first.ok is True, first.error

    second = _run(orch_path, initial_state={**first.state})

    assert second.state["_run_id"] != first.state["_run_id"]
    assert second.state["runtime"]["last_run"]["run_id"] != (
        first.state["runtime"]["last_run"]["run_id"]
    )
    # Both differ from the carried-over run's id/timestamp — not just from
    # each other's `runtime.last_run.run_id` (the one key that already got a
    # fresh value before this fix).
    assert second.state["prime"]["stamp"]["value"] != first.state["prime"]["stamp"]["value"]


def test_run_id_and_timestamp_fresh_on_persistence_resume(tmp_path: Path) -> None:
    orch_path = _stamp_orch(tmp_path / "stamp.yml")
    log_path = tmp_path / "runs.jsonl"
    cfg = CircuitryConfig(
        runtime={"persistence": {"enabled": True, "backend": "jsonl-file", "path": str(log_path)}}
    )

    first = _run(orch_path, config=cfg)
    assert first.ok is True, first.error

    second = _run(orch_path, config=cfg)
    assert second.ok is True, second.error
    assert second.state["runtime"]["persistence"]["loaded_from_persistence"] is True

    assert second.state["_run_id"] != first.state["_run_id"]
    assert second.state["_timestamp"] != first.state["_timestamp"] or (
        second.state["_run_id"] != first.state["_run_id"]
    )
    assert second.state["prime"]["stamp"]["value"] != first.state["prime"]["stamp"]["value"]


# ---------------------------------------------------------------------------
# #255(a) — a missing --state file is an error, not empty input
# ---------------------------------------------------------------------------


def test_missing_state_file_is_an_error_not_empty_input(tmp_path: Path) -> None:
    orch_path = _stamp_orch(tmp_path / "stamp.yml")
    missing = tmp_path / "does_not_exist.json"

    result = _run(orch_path, state_path=missing)

    assert result.ok is False
    assert "state file not found" in (result.error or "")
    assert str(missing) in (result.error or "")


def test_cli_missing_state_file_is_a_clean_error(tmp_path: Path) -> None:
    orch_path = _stamp_orch(tmp_path / "stamp.yml")
    missing = tmp_path / "does_not_exist.json"

    result = runner.invoke(app, ["run", str(orch_path), "--state", str(missing), "--json"])

    assert result.exit_code == 1
    assert "Traceback" not in result.output
    payload = json.loads(result.stdout)
    assert payload["ok"] is False
    assert "state file not found" in payload["error"]


def test_cli_missing_state_file_with_inline_override_is_also_a_clean_error(
    tmp_path: Path,
) -> None:
    """`--state missing.json -e k=v` reads the file eagerly to merge `-e` in —
    a separate code path from the plain `--state` case above."""
    orch_path = _stamp_orch(tmp_path / "stamp.yml")
    missing = tmp_path / "does_not_exist.json"

    result = runner.invoke(
        app, ["run", str(orch_path), "--state", str(missing), "-e", "k=v", "--dry-run"]
    )

    assert result.exit_code == 1
    assert "Traceback" not in result.output
    assert "state file not found" in result.output


# ---------------------------------------------------------------------------
# #255(b) — interface.inputs required/type enforced at every run entry
# ---------------------------------------------------------------------------


def _echo_orch(path: Path, iface_inputs: dict) -> Path:
    import yaml

    doc = {
        "interface": {"inputs": iface_inputs},
        "effects": [
            {
                "type": "tool",
                "name": "echo",
                "provider": "json",
                "params": {"mode": "stringify", "input": "{{input.value}}"},
            }
        ],
    }
    path.write_text(yaml.dump(doc), encoding="utf-8")
    return path


def test_required_input_enforced_at_top_level_run(tmp_path: Path) -> None:
    orch_path = _echo_orch(
        tmp_path / "echo.yml", {"value": {"type": "string", "required": True}}
    )

    result = _run(orch_path)

    assert result.ok is False
    assert "missing required input 'value'" in (result.error or "")


def test_required_input_satisfied_runs_clean(tmp_path: Path) -> None:
    orch_path = _echo_orch(
        tmp_path / "echo.yml", {"value": {"type": "string", "required": True}}
    )

    result = _run(orch_path, initial_state={"input": {"value": "hi"}})

    assert result.ok is True, result.error


def test_type_mismatch_rejected_at_top_level_run(tmp_path: Path) -> None:
    orch_path = _echo_orch(tmp_path / "echo.yml", {"value": {"type": "number"}})

    result = _run(orch_path, initial_state={"input": {"value": {"not": "a number"}}})

    assert result.ok is False
    assert "input 'value'" in (result.error or "")
    assert "number" in (result.error or "")


def test_cli_dash_e_string_coerced_to_declared_number(tmp_path: Path) -> None:
    """A CLI `-e` value JSON can't parse (`007`) still coerces via the
    declared type, instead of failing or passing through as text."""
    orch_path = _echo_orch(tmp_path / "echo.yml", {"value": {"type": "number"}})

    result = runner.invoke(
        app, ["run", str(orch_path), "-e", "value=007", "--json"]
    )

    assert result.exit_code == 0, result.output
    payload = json.loads(result.stdout)
    # Rendered via Mustache then JSON-stringified by the tool: a value truly
    # coerced to int 7 renders "7"; a value left as the string "007" would
    # keep its leading zero through both steps.
    assert payload["prime"]["echo"]["value"] == '"7"'


def test_cli_dash_e_string_coerced_to_declared_boolean_via_cel(tmp_path: Path) -> None:
    """`-e flag=True` isn't valid JSON (capitalized) so `-e` parsing leaves it
    a string; the declared `type: boolean` still coerces it — checked with a
    CEL comparison against the boolean literal, which only matches a real
    Python `bool`, not the string "True"."""
    import yaml

    orch_path = tmp_path / "flag.yml"
    orch_path.write_text(
        yaml.dump(
            {
                "interface": {"inputs": {"flag": {"type": "boolean"}}},
                "effects": [
                    {
                        "type": "if",
                        "name": "check",
                        "if": {"mode": "cel", "expr": "state.input.flag == true"},
                        "then": [
                            {
                                "type": "tool",
                                "name": "branch",
                                "provider": "json",
                                "params": {"mode": "stringify", "input": "then"},
                            }
                        ],
                        "else": [
                            {
                                "type": "tool",
                                "name": "branch",
                                "provider": "json",
                                "params": {"mode": "stringify", "input": "else"},
                            }
                        ],
                    }
                ],
            }
        ),
        encoding="utf-8",
    )

    result = runner.invoke(app, ["run", str(orch_path), "-e", "flag=True", "--json"])

    assert result.exit_code == 0, result.output
    payload = json.loads(result.stdout)
    assert payload["prime"]["check"]["value"]["branch"] == "then"


def test_undeclared_extra_inputs_stay_allowed(tmp_path: Path) -> None:
    orch_path = _echo_orch(tmp_path / "echo.yml", {"value": {"type": "string"}})

    result = _run(
        orch_path, initial_state={"input": {"value": "hi", "extra": "untouched"}}
    )

    assert result.ok is True, result.error
    assert result.state["input"]["extra"] == "untouched"


def test_default_applied_when_top_level_input_omitted(tmp_path: Path) -> None:
    orch_path = _echo_orch(
        tmp_path / "echo.yml", {"value": {"type": "string", "default": "fallback"}}
    )

    result = _run(orch_path)

    assert result.ok is True, result.error
    assert result.state["input"]["value"] == "fallback"
    assert result.state["prime"]["echo"]["value"] == '"fallback"'


def test_default_not_applied_when_caller_passes_value(tmp_path: Path) -> None:
    orch_path = _echo_orch(
        tmp_path / "echo.yml", {"value": {"type": "string", "default": "fallback"}}
    )

    result = _run(orch_path, initial_state={"input": {"value": "caller"}})

    assert result.ok is True, result.error
    assert result.state["input"]["value"] == "caller"


# ---------------------------------------------------------------------------
# #255(b) — the same enforcement reaches every surface, not just cof run
# ---------------------------------------------------------------------------


def test_sdk_run_orchestration_enforces_required_input(tmp_path: Path) -> None:
    from circuitry.api import CircuitryExecutionError, run_orchestration

    orch_path = _echo_orch(
        tmp_path / "echo.yml", {"value": {"type": "string", "required": True}}
    )

    with pytest.raises(CircuitryExecutionError, match="missing required input 'value'"):
        run_orchestration(orchestration_path=orch_path)


def test_rest_trigger_enforces_required_input(tmp_path: Path) -> None:
    from circuitry.service import RestTriggerService

    orch_path = _echo_orch(
        tmp_path / "echo.yml", {"value": {"type": "string", "required": True}}
    )

    svc = RestTriggerService(auth_token=None)
    response = svc.handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps({"orchestration_path": str(orch_path), "dry_run": True}),
    )

    assert response.body["ok"] is False
    assert "missing required input 'value'" in response.body["error"]


def test_scheduler_enforces_required_input(tmp_path: Path) -> None:
    from datetime import datetime, timezone

    from circuitry.service import RecurringScheduler, ScheduledJob

    orch_path = _echo_orch(
        tmp_path / "echo.yml", {"value": {"type": "string", "required": True}}
    )

    start_at = datetime(2026, 2, 19, 12, 0, tzinfo=timezone.utc)
    job = ScheduledJob(
        name="echo-job",
        orchestration_path=orch_path,
        interval_seconds=60,
        dry_run=True,
        start_at=start_at,
    )
    scheduler = RecurringScheduler(jobs=[job])

    records = scheduler.tick(now=start_at)
    assert len(records) == 1
    assert records[0].status == "failed"
    assert "missing required input 'value'" in (records[0].error or "")


# ---------------------------------------------------------------------------
# #256 — library warnings reach the CLI's stderr
# ---------------------------------------------------------------------------


def test_malformed_global_config_warns_on_stderr(tmp_path: Path) -> None:
    config_module.GLOBAL_CONFIG_PATH.parent.mkdir(parents=True, exist_ok=True)
    config_module.GLOBAL_CONFIG_PATH.write_text(
        '{"default_model": "x",}', encoding="utf-8"
    )

    result = runner.invoke(app, ["list"])

    assert "Skipping malformed global config" in result.stderr


def test_warning_on_stderr_does_not_dirty_json_stdout(tmp_path: Path) -> None:
    config_module.GLOBAL_CONFIG_PATH.parent.mkdir(parents=True, exist_ok=True)
    config_module.GLOBAL_CONFIG_PATH.write_text(
        '{"default_model": "x",}', encoding="utf-8"
    )
    orch_path = _stamp_orch(tmp_path / "stamp.yml")

    result = runner.invoke(app, ["run", str(orch_path), "--json"])

    assert "Skipping malformed global config" in result.stderr
    payload = json.loads(result.stdout)  # raises if stdout isn't clean JSON
    assert payload["prime"]["stamp"]["value"]
