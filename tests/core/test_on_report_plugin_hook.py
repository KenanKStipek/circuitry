"""The ``on_report`` runtime plugin hook (#615).

A ``report`` effect never fires ``on_effect_start``/``on_effect_complete``
(see ``core/test_report_effect.py``); this is the hook it fires instead,
optional the same way ``on_effect_start`` is.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run
from circuitry.core.report import (
    REPORT_DATA_MAX_BYTES,
    build_report_payload,
    encode_report_data,
)
from circuitry.core.runtime_plugins import PluginContext, invoke_plugins


def _write(tmp_path: Path, name: str, content: str) -> Path:
    p = tmp_path / name
    p.write_text(content, encoding="utf-8")
    return p


def _reports(state: dict[str, Any]) -> list[dict[str, Any]]:
    return list(state["runtime"]["report_marker"]["reports"])


REPORT_ORCH = """
adapter: openai
model: gpt-4o-mini
effects:
  - type: report
    level: warning
    message: "note on widgets"
    data:
      literal: 7
""".strip() + "\n"


def test_a_plugin_without_on_report_is_untouched(
    tmp_path: Path, caplog: pytest.LogCaptureFixture,
) -> None:
    """A plugin implementing every other hook but not ``on_report`` keeps
    working unchanged -- the hasattr guard skips it silently, the same as
    a plugin missing ``on_effect_start``. Without the guard, dispatching
    the hook would raise ``AttributeError`` and log a hook-failure warning
    (see the raising-plugin test below), which the guard must prevent."""
    orch = _write(tmp_path, "orch.yml", REPORT_ORCH)
    cfg = CircuitryConfig(plugins=["plugin_fixtures:make_recording_plugin"])
    with caplog.at_level("WARNING"):
        result = run(
            RunRequest(
                orchestration_path=orch,
                state_path=None,
                out_path=None,
                dry_run=True,
                validate_only=False,
                initial_state={},
                config=cfg,
            )
        )
    assert result.ok is True, result.error
    events = result.state["runtime"]["plugins"]["events"]
    assert not any(e["hook"] == "on_report" for e in events)
    assert not any("on_report" in r.message for r in caplog.records)
    # The plugin's own on_run_start still ran -- it is genuinely untouched,
    # not silently broken.
    assert result.state["runtime"]["plugin_marker"]["started"] is True


def test_a_raising_report_plugin_is_logged_but_the_run_succeeds(
    tmp_path: Path, caplog: pytest.LogCaptureFixture,
) -> None:
    """Per-effect hooks' failures (on_effect_start/on_effect_complete/
    on_report) are logged, not recorded in ``runtime.plugins.events`` --
    only the three run-level hooks are (see ``cli.test_plugins_runtime``).
    A report must never fail the run, so a raising ``on_report`` is no
    different from any other hook failure: logged, swallowed, run succeeds.
    """
    orch = _write(tmp_path, "orch.yml", REPORT_ORCH)
    cfg = CircuitryConfig(plugins=["plugin_fixtures:make_failing_plugin"])
    with caplog.at_level("WARNING"):
        result = run(
            RunRequest(
                orchestration_path=orch,
                state_path=None,
                out_path=None,
                dry_run=True,
                validate_only=False,
                initial_state={},
                config=cfg,
            )
        )
    assert result.ok is True, result.error
    assert any(
        "on_report" in record.message and "plugin report exploded" in record.message
        for record in caplog.records
    )


def test_the_report_dict_carries_path_level_message_and_data(
    tmp_path: Path,
) -> None:
    orch = _write(tmp_path, "orch.yml", REPORT_ORCH)
    cfg = CircuitryConfig(plugins=["plugin_fixtures:make_report_plugin"])
    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=False,
            initial_state={},
            config=cfg,
        )
    )
    assert result.ok is True, result.error
    reports = _reports(result.state)
    assert len(reports) == 1
    report = reports[0]
    assert report["path"] == "prime"
    assert report["level"] == "warning"
    assert report["message"] == "note on widgets"
    assert report["data"] == {"literal": 7}
    assert "data_truncated" not in report
    assert isinstance(report["ts"], str) and report["ts"]


# -- the shared truncation helper --------------------------------------------


def test_encode_report_data_boundary_is_not_truncated() -> None:
    """The limit is "exceeds REPORT_DATA_MAX_BYTES", not "at least" (#605,
    #615): a payload whose compact JSON is exactly the limit keeps its
    value."""
    import json

    blob_len = REPORT_DATA_MAX_BYTES - len('{"blob":""}')
    payload = {"blob": "x" * blob_len}
    assert len(json.dumps(payload, separators=(",", ":")).encode("utf-8")) == (
        REPORT_DATA_MAX_BYTES
    )
    value, truncated = encode_report_data(payload)
    assert value == payload
    assert truncated is None


def test_encode_report_data_one_byte_over_is_truncated() -> None:
    import json

    blob_len = REPORT_DATA_MAX_BYTES - len('{"blob":""}') + 1
    payload = {"blob": "x" * blob_len}
    expected_bytes = len(json.dumps(payload, separators=(",", ":")).encode("utf-8"))
    assert expected_bytes == REPORT_DATA_MAX_BYTES + 1
    value, truncated = encode_report_data(payload)
    assert value is None
    assert truncated == expected_bytes


def test_build_report_payload_uses_data_truncated_past_the_limit() -> None:
    big = {"blob": "x" * 20_000}
    payload = build_report_payload(
        path="prime.status", level="info", message="big", data=big
    )
    assert list(payload.keys()) == ["ts", "path", "level", "message", "data_truncated"]
    assert payload["path"] == "prime.status"


def test_build_report_payload_omits_data_when_none() -> None:
    payload = build_report_payload(
        path="prime", level="info", message="hi", data=None
    )
    assert "data" not in payload
    assert "data_truncated" not in payload


# -- invoke_plugins directly --------------------------------------------------


class _RaisingPlugin:
    name = "raising"

    def on_report(
        self, *, state: dict[str, Any], context: PluginContext, report: dict[str, Any]
    ) -> None:
        del state, context, report
        raise RuntimeError("boom")


class _NoHookPlugin:
    name = "no-hook"


def test_invoke_plugins_on_report_skips_a_plugin_without_the_hook() -> None:
    ctx = PluginContext(
        run_id="r", orchestration_path=Path("x.yml"),
        dry_run=False, validate_only=False, runtime_config={},
    )
    events = invoke_plugins(
        plugins=[_NoHookPlugin()],  # type: ignore[list-item]
        hook_name="on_report",
        state={},
        context=ctx,
        report={"path": "prime", "level": "info", "message": "hi"},
    )
    assert events == []


def test_invoke_plugins_on_report_logs_a_raising_plugin() -> None:
    ctx = PluginContext(
        run_id="r", orchestration_path=Path("x.yml"),
        dry_run=False, validate_only=False, runtime_config={},
    )
    events = invoke_plugins(
        plugins=[_RaisingPlugin()],  # type: ignore[list-item]
        hook_name="on_report",
        state={},
        context=ctx,
        report={"path": "prime", "level": "info", "message": "hi"},
    )
    assert len(events) == 1
    assert events[0]["hook"] == "on_report"
    assert events[0]["ok"] is False
    assert "boom" in events[0]["error"]
