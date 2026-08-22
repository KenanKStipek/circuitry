"""The data layer behind the Doctor, Settings and Validate views.

Lives outside ``tests/tui`` on purpose: ``circuitry.tui.diagnostics`` imports no
Textual, so these run everywhere the library runs — including the CI lanes that
install without the ``tui`` extra and skip the pilot suite entirely.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.effective_settings import (
    EffectiveSettings,
    resolve_effective_settings,
)
from circuitry.cli.redaction import REDACTED
from circuitry.preflight import CheckResult
from circuitry.tui.diagnostics import (
    CheckTarget,
    ExtensionCheck,
    check_targets,
    load_diagnostics,
    next_step,
    run_check,
    settings_rows,
    validate_report,
)

FIXTURES = Path(__file__).parent / "tui" / "fixtures"


# -- the missing-item grammar --------------------------------------------------


@pytest.mark.parametrize(
    ("item", "expected"),
    [
        ("env:OPENAI_API_KEY", "Set the OPENAI_API_KEY environment variable"),
        ("binary:ffmpeg", "Install ffmpeg and make sure it is on your PATH"),
        ("library:pymongo", "pip install pymongo"),
        ("host:http://localhost:11434", "Start the service at http://localhost:11434"),
    ],
)
def test_every_missing_kind_becomes_a_next_step(item: str, expected: str) -> None:
    assert expected in next_step(item)


@pytest.mark.parametrize("item", ["something-odd", "env:", ""])
def test_unknown_missing_items_are_echoed_not_dropped(item: str) -> None:
    """An unrecognised prefix is still information; never swallow it."""
    assert item in next_step(item)


def test_a_missing_check_carries_one_next_step_per_item() -> None:
    check = ExtensionCheck(
        CheckTarget("tool", "ffmpeg"),
        "missing",
        ("binary:ffmpeg", "env:FFMPEG_PATH"),
    )
    assert len(check.next_steps) == 2
    assert "PATH" in check.detail
    assert "FFMPEG_PATH" in check.detail
    assert not check.ok


# -- the check walk ------------------------------------------------------------


def test_targets_follow_the_allowlists() -> None:
    config = CircuitryConfig(
        enabled_adapters=["ollama"],
        enabled_tools=["clock"],
        plugins=["circuitry.runtime_plugins.jsonl_file"],
    )
    assert check_targets(config) == (
        CheckTarget("adapter", "ollama"),
        CheckTarget("tool", "clock"),
        CheckTarget("runtime_plugin", "circuitry.runtime_plugins.jsonl_file"),
    )


def test_default_open_config_checks_every_compiled_in_extension() -> None:
    targets = check_targets(CircuitryConfig())
    categories = {target.category for target in targets}
    assert categories == {"adapter", "tool"}  # no runtime plugins configured
    assert CheckTarget("adapter", "ollama") in targets


def test_an_unknown_adapter_is_an_error_not_a_crash() -> None:
    check = run_check(CheckTarget("adapter", "nope"), CircuitryConfig())
    assert check.state == "error"
    assert "Unknown adapter" in (check.message or "")


def test_a_runtime_injected_adapter_is_deferred() -> None:
    """host_claude cannot be built outside MCP — that is not a broken machine."""
    check = run_check(CheckTarget("adapter", "host_claude"), CircuitryConfig())
    assert check.state == "deferred"
    assert check.ok


def test_a_healthy_extension_reports_ok() -> None:
    check = run_check(CheckTarget("tool", "clock"), CircuitryConfig())
    assert check.state == "ok"


def test_a_missing_env_var_is_reported_with_its_next_step(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    check = run_check(CheckTarget("adapter", "openai"), CircuitryConfig())
    assert check.state == "missing"
    assert "env:OPENAI_API_KEY" in check.missing
    assert "Set the OPENAI_API_KEY environment variable" in check.detail


def test_a_check_that_raises_is_reported_not_propagated() -> None:
    class Exploding:
        def check(self) -> CheckResult:
            raise RuntimeError("boom")

    from circuitry.preflight import call_check

    result = call_check(Exploding())
    assert not result.ok
    assert "boom" in (result.message or "")


# -- effective settings --------------------------------------------------------


def _settings(**runtime: object) -> EffectiveSettings:
    config = CircuitryConfig(
        default_model="llama3.1:8b",
        default_adapter="ollama",
        runtime=dict(runtime),
    )
    return resolve_effective_settings(cfg=config, orch={})


def test_every_setting_carries_the_layer_it_came_from() -> None:
    effective = resolve_effective_settings(
        cfg=CircuitryConfig(default_model="from-config"),
        orch={"adapter": "from-orch"},
    )
    sources = {row.key: row.source for row in settings_rows(effective)}
    assert sources["model"] == "config"
    assert sources["adapter"] == "orchestration"


def test_a_seeded_token_never_reaches_the_screen() -> None:
    effective = _settings(
        adapters={"openai": {"api_key": "sk-livetoken0000000000000000000000"}}
    )
    rows = {row.key: row.value for row in settings_rows(effective)}
    assert rows["runtime.adapters.openai.api_key"] == REDACTED
    assert all("sk-livetoken" not in row.value for row in settings_rows(effective))


def test_nested_runtime_config_is_flattened_to_one_row_per_value() -> None:
    effective = _settings(adapters={"ollama": {"base_url": "http://localhost:11434"}})
    rows = {row.key: row.value for row in settings_rows(effective)}
    assert rows["runtime.adapters.ollama.base_url"] == "http://localhost:11434"


def test_unset_values_render_as_a_dash_rather_than_none() -> None:
    rows = {row.key: row.value for row in settings_rows(resolve_effective_settings(cfg=CircuitryConfig(), orch={}))}
    assert rows["model"] == "—"
    assert rows["plugins"] == "—"


def test_load_diagnostics_resolves_the_machine(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.chdir(tmp_path)
    diagnostics = load_diagnostics()
    assert diagnostics.targets()
    assert any(row.key == "adapter" for row in diagnostics.rows())


# -- validation ----------------------------------------------------------------


def test_a_broken_file_reports_every_error_class_at_once() -> None:
    """The CLI stops at the first gate; the view needs the whole picture."""
    report = validate_report(FIXTURES / "broken.yml", config=CircuitryConfig())
    assert report.kinds() == ("schema", "compile", "cycle", "preflight")
    assert not report.ok


def test_each_error_class_says_something_specific() -> None:
    report = validate_report(FIXTURES / "broken.yml", config=CircuitryConfig())
    assert "bad name" in report.of_kind("schema")[0].message
    assert report.of_kind("schema")[0].location == "/effects/0/name"
    assert "whitespace is not allowed" in report.of_kind("compile")[0].message
    assert "broken.yml" in report.of_kind("cycle")[0].message
    assert "definitely_not_an_adapter" in report.of_kind("preflight")[0].line()


def test_a_clean_file_reports_nothing() -> None:
    report = validate_report(FIXTURES / "valid.yml", skip_preflight=True)
    assert report.ok
    assert report.kinds() == ()


def test_gates_that_could_not_run_are_named_rather_than_assumed_green() -> None:
    report = validate_report(FIXTURES / "valid.yml", skip_preflight=True)
    assert "preflight" in report.skipped
    assert "allowlist" in report.skipped


def test_an_empty_file_is_a_load_error(tmp_path: Path) -> None:
    path = tmp_path / "empty.yml"
    path.write_text("", encoding="utf-8")
    report = validate_report(path)
    assert report.kinds() == ("load",)
    assert "empty" in report.of_kind("load")[0].message


def test_a_missing_file_is_a_load_error(tmp_path: Path) -> None:
    report = validate_report(tmp_path / "nope.yml")
    assert report.kinds() == ("load",)


def test_unparseable_yaml_is_a_load_error(tmp_path: Path) -> None:
    path = tmp_path / "bad.yml"
    path.write_text("effects: [\n  - type: prompt\n", encoding="utf-8")
    report = validate_report(path)
    assert report.kinds() == ("load",)


def test_an_allowlist_violation_is_its_own_class() -> None:
    locked_down = CircuitryConfig(enabled_adapters=[])
    report = validate_report(FIXTURES / "valid.yml", config=locked_down)
    assert "allowlist" in report.kinds()


def test_preflight_issues_carry_their_next_steps(tmp_path: Path) -> None:
    path = tmp_path / "unreachable.yml"
    path.write_text(
        "adapter: ollama\neffects:\n  - type: prompt\n    name: greet\n"
        '    template: "hi"\n',
        encoding="utf-8",
    )
    config = CircuitryConfig(
        runtime={"adapters": {"ollama": {"base_url": "http://127.0.0.1:1"}}}
    )
    report = validate_report(path, config=config)
    preflight = report.of_kind("preflight")
    assert preflight
    assert any("Start the service at" in hint for hint in preflight[0].hints)
