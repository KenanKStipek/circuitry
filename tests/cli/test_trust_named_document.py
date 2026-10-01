"""A document named by path is trusted; one that arrived indirectly is limited.

`cof run ./my.yml` (and `cof check` / `cof score` on a path, the TUI's local
files, the SDK's `run_orchestration` / `validate_orchestration`, scheduler
jobs) applies the document's whole `runtime:` block and `plugins:` list, with
one notice naming the host settings among them. A library name, `run-library`,
`run_shared_orchestration`, MCP and the REST trigger keep the limit to
`runtime.complexity` / `runtime.state`. No network: adapters are built but
never called.
"""

from __future__ import annotations

import json
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest
import yaml
from typer.testing import CliRunner

from circuitry import api
from circuitry.adapters.base import GenerateResult
from circuitry.cli import app as app_module
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.effective_settings import (
    orchestration_host_setting_warnings,
    resolve_effective_settings,
)
from circuitry.cli.runtime_shim import RunRequest, RunResult, validate
from circuitry.cli.runtime_shim import run as shim_run
from circuitry.mcp import server as mcp_server
from circuitry.service import RestTriggerService
from circuitry.service import rest as rest_module
from circuitry.service import scheduler as scheduler_module
from circuitry.service.scheduler import RecurringScheduler, ScheduledJob

CONFIG_BASE_URL = "https://api.config.example/v1"
DOCUMENT_BASE_URL = "http://document.example/v1"
DOCUMENT_API_KEY = "sk-document-secret-value"

NOTICE = "Applied host settings from orch.yml: runtime.adapters.openai.base_url"
IGNORED = "Ignored runtime.adapters from the orchestration"

runner = CliRunner()


def _config_dict(**extra: Any) -> dict[str, Any]:
    runtime: dict[str, Any] = {"adapters": {"openai": {"base_url": CONFIG_BASE_URL}}}
    runtime.update(extra)
    return {"default_adapter": "openai", "default_model": "gpt-test", "runtime": runtime}


def _cfg(**extra: Any) -> CircuitryConfig:
    return CircuitryConfig(**_config_dict(**extra))


def _config_file(tmp_path: Path, **extra: Any) -> Path:
    path = tmp_path / "config.json"
    path.write_text(json.dumps(_config_dict(**extra)), encoding="utf-8")
    return path


def _document_runtime() -> dict[str, Any]:
    return {"adapters": {"openai": {"base_url": DOCUMENT_BASE_URL}}}


def _write_yaml(path: Path, orch: dict[str, Any]) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(yaml.safe_dump(orch, sort_keys=False), encoding="utf-8")
    return path


def _tool_orch(directory: Path, name: str = "orch.yml", **top_level: Any) -> Path:
    """A document that runs without an adapter (one local tool effect)."""
    orch = {**top_level, "effects": [{"type": "tool", "name": "t", "provider": "uuid"}]}
    return _write_yaml(directory / name, orch)


def _applied_base_url(state: dict[str, Any]) -> str:
    """The openai base_url the run resolved (from its recorded settings)."""
    runtime = state["runtime"]["effective_settings"]["runtime"]
    return str(runtime["adapters"]["openai"]["base_url"])


@dataclass
class _RecordingAdapter:
    name: str
    calls: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append(model)
        return GenerateResult(text="ok", raw={})


def _capture_adapter_builds(monkeypatch: pytest.MonkeyPatch) -> list[str]:
    """Record the base_url each run-level adapter is built with."""
    built: list[str] = []

    def _build(*, adapter_name: str, runtime: dict[str, Any]) -> _RecordingAdapter:
        built.append(runtime["adapters"][adapter_name]["base_url"])
        return _RecordingAdapter(name=adapter_name)

    monkeypatch.setattr("circuitry.cli.runtime_shim.build_adapter", _build)
    return built


def _prompt_orch(directory: Path) -> Path:
    return _write_yaml(
        directory / "orch.yml",
        {
            "adapter": "openai",
            "model": "gpt-test",
            "runtime": _document_runtime(),
            "effects": [{"type": "prompt", "name": "ask", "template": "hi"}],
        },
    )


def _library_config(tmp_path: Path, folder: Path) -> Path:
    """A config whose `local` folder source serves *folder* by name."""
    return _config_file(
        tmp_path,
        library={"sources": [{"type": "folder", "name": "local", "path": str(folder)}]},
    )


# ── the notice ───────────────────────────────────────────────────────────────


def test_trusted_document_applies_its_whole_runtime_with_one_notice() -> None:
    effective = resolve_effective_settings(
        cfg=_cfg(),
        orch={"runtime": _document_runtime()},
        trust_document=True,
        document_name="orch.yml",
    )

    assert effective.runtime["adapters"]["openai"]["base_url"] == DOCUMENT_BASE_URL
    assert effective.sources["runtime"] == "orchestration"
    assert effective.warnings == (NOTICE,)


def test_untrusted_document_keeps_the_limit_by_default() -> None:
    effective = resolve_effective_settings(
        cfg=_cfg(), orch={"runtime": _document_runtime()}
    )

    assert effective.runtime["adapters"]["openai"]["base_url"] == CONFIG_BASE_URL
    assert len(effective.warnings) == 1
    assert effective.warnings[0].startswith(IGNORED)


def test_notice_names_key_paths_and_unlisted_plugins_never_values() -> None:
    orch = {
        "plugins": ["acme.telemetry", "listed.plugin"],
        "runtime": {
            "adapters": {"openai": {"base_url": DOCUMENT_BASE_URL, "api_key": DOCUMENT_API_KEY}},
            # Deeper than runtime.<key>.<a>.<b>: cut off, so env names stay out.
            "plugins": {"shell": {"env": {"SECRET_NAME": "x"}}},
            "persistence": {},
            "made_up_key": 1,
            "state": {"record_children": True},
        },
    }

    effective = resolve_effective_settings(
        cfg=_cfg(),
        orch=orch,
        cli_plugins=None,
        trust_document=True,
        document_name="my.yml",
    )
    # Config lists one of the two plugins; only the other is a host setting.
    listed = CircuitryConfig(**_config_dict(), enabled_plugins=["listed.plugin"])
    with_listed = orchestration_host_setting_warnings(
        orch, listed, trust_document=True, document_name="my.yml"
    )

    assert with_listed == [
        (
            "Applied host settings from my.yml: runtime.adapters.openai.base_url, "
            "runtime.adapters.openai.api_key, runtime.plugins.shell.env, "
            "runtime.persistence, runtime.made_up_key, plugins: acme.telemetry"
        )
    ]
    assert len(effective.warnings) == 1
    assert effective.warnings[0].endswith("plugins: acme.telemetry, listed.plugin")
    assert DOCUMENT_API_KEY not in effective.warnings[0]
    assert "SECRET_NAME" not in effective.warnings[0]


def test_notice_keeps_an_odd_key_on_one_line() -> None:
    warnings = orchestration_host_setting_warnings(
        {"runtime": {"adapters\nforged line": 1}},
        _cfg(),
        trust_document=True,
        document_name="orch.yml",
    )

    assert warnings == [
        "Applied host settings from orch.yml: runtime.'adapters\\nforged line'"
    ]


def test_trusted_document_setting_only_complexity_and_state_prints_nothing() -> None:
    effective = resolve_effective_settings(
        cfg=_cfg(),
        orch={
            "runtime": {
                "state": {"record_children": True},
                "complexity": {"scoring": {"enabled": True}},
            }
        },
        trust_document=True,
        document_name="orch.yml",
    )

    assert effective.warnings == ()
    assert effective.complexity.scoring.enabled is True


# ── cof run: a path vs a library name ────────────────────────────────────────


def test_cof_run_path_builds_the_adapter_from_the_document_and_prints_the_notice(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    built = _capture_adapter_builds(monkeypatch)
    orch = _prompt_orch(tmp_path)

    result = runner.invoke(
        app_module.app,
        ["run", str(orch), "-c", str(_config_file(tmp_path)), "--skip-preflight"],
    )

    assert result.exit_code == 0, result.output
    assert built == [DOCUMENT_BASE_URL]
    assert result.stderr.splitlines() == [f"Warning: {NOTICE}"]


def test_cof_run_library_name_stays_limited(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    built = _capture_adapter_builds(monkeypatch)
    folder = tmp_path / "lib"
    folder.mkdir()
    _prompt_orch(folder).rename(folder / "scoped_doc.yml")

    result = runner.invoke(
        app_module.app,
        ["run", "scoped_doc", "-c", str(_library_config(tmp_path, folder)), "--skip-preflight"],
    )

    assert result.exit_code == 0, result.output
    assert built == [CONFIG_BASE_URL]
    assert f"Warning: {IGNORED}" in result.stderr
    assert "Applied host settings" not in result.stderr


def test_cof_run_path_setting_only_state_prints_nothing(tmp_path: Path) -> None:
    orch = _tool_orch(tmp_path, runtime={"state": {"record_children": True}})

    result = runner.invoke(
        app_module.app,
        ["run", str(orch), "-c", str(_config_file(tmp_path)), "--skip-preflight"],
    )

    assert result.exit_code == 0, result.output
    assert result.stderr == ""


def test_cof_run_last_replays_the_original_trust(tmp_path: Path) -> None:
    """The stash holds the resolved file, so `--last` must not re-trust a name."""
    folder = tmp_path / "lib"
    _tool_orch(folder, name="scoped_doc.yml", runtime=_document_runtime())
    config = str(_library_config(tmp_path, folder))

    by_name = runner.invoke(
        app_module.app, ["run", "scoped_doc", "-c", config, "--skip-preflight"]
    )
    replay_name = runner.invoke(app_module.app, ["run", "--last"])
    by_path = runner.invoke(
        app_module.app,
        ["run", str(folder / "scoped_doc.yml"), "-c", config, "--skip-preflight"],
    )
    replay_path = runner.invoke(app_module.app, ["run", "--last"])

    for result in (by_name, replay_name, by_path, replay_path):
        assert result.exit_code == 0, result.output
    assert _applied_base_url(json.loads(replay_name.stdout)) == CONFIG_BASE_URL
    assert IGNORED in replay_name.stderr
    assert _applied_base_url(json.loads(replay_path.stdout)) == DOCUMENT_BASE_URL
    assert "Applied host settings from scoped_doc.yml" in replay_path.stderr


def test_cof_check_path_prints_the_notice(tmp_path: Path) -> None:
    orch = _tool_orch(tmp_path, runtime=_document_runtime())

    result = runner.invoke(
        app_module.app,
        ["check", str(orch), "-c", str(_config_file(tmp_path)), "--skip-preflight"],
    )

    assert result.exit_code == 0, result.output
    assert f"Warning: {NOTICE}" in " ".join(result.output.split())
    assert IGNORED not in result.output


def test_cof_score_path_resolves_the_document_as_trusted(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    seen: list[bool] = []
    real = app_module.resolve_effective_settings

    def _spy(**kwargs: Any) -> Any:
        seen.append(kwargs.get("trust_document", False))
        return real(**kwargs)

    monkeypatch.setattr("circuitry.cli.score.resolve_effective_settings", _spy)
    orch = _tool_orch(tmp_path, runtime=_document_runtime())

    runner.invoke(
        app_module.app, ["score", str(orch), "-c", str(_config_file(tmp_path))]
    )

    assert seen == [True]


def test_run_library_stays_limited(tmp_path: Path) -> None:
    lib_root = tmp_path / "library"
    _tool_orch(lib_root / "welcome", name="1.0.0.yml", runtime=_document_runtime())
    (lib_root / "welcome" / "1.0.0.json").write_text("{}\n", encoding="utf-8")
    config = _config_file(
        tmp_path, library={"backend": "filesystem", "local_root": str(lib_root)}
    )

    result = runner.invoke(app_module.app, ["run-library", "welcome", "-c", str(config)])

    assert result.exit_code == 0, result.output
    assert _applied_base_url(json.loads(result.stdout)) == CONFIG_BASE_URL
    assert f"Warning: {IGNORED}" in result.stderr


# ── SDK ──────────────────────────────────────────────────────────────────────


def test_sdk_run_orchestration_trusts_the_path_by_default(tmp_path: Path) -> None:
    orch = _tool_orch(tmp_path, runtime=_document_runtime())

    trusted = api.run_orchestration(orchestration_path=orch, config=_cfg())
    limited = api.run_orchestration(
        orchestration_path=orch, config=_cfg(), trust_document=False
    )

    assert _applied_base_url(trusted.state) == DOCUMENT_BASE_URL
    assert trusted.warnings == [NOTICE]
    assert _applied_base_url(limited.state) == CONFIG_BASE_URL
    assert limited.warnings[0].startswith(IGNORED)


def test_sdk_validate_orchestration_matches_run(tmp_path: Path) -> None:
    orch = _tool_orch(tmp_path, runtime=_document_runtime())

    trusted = api.validate_orchestration(orchestration_path=orch)
    limited = api.validate_orchestration(orchestration_path=orch, trust_document=False)

    assert trusted["warnings"] == [NOTICE]
    assert limited["warnings"][0].startswith(IGNORED)


def test_sdk_run_shared_orchestration_stays_limited(tmp_path: Path) -> None:
    lib_root = tmp_path / "library"
    _tool_orch(lib_root / "welcome", name="1.0.0.yml", runtime=_document_runtime())
    (lib_root / "welcome" / "1.0.0.json").write_text("{}\n", encoding="utf-8")

    result = api.run_shared_orchestration(
        asset_id="welcome",
        config=_cfg(library={"backend": "filesystem", "local_root": str(lib_root)}),
    )

    assert _applied_base_url(result.state) == CONFIG_BASE_URL
    assert result.warnings[0].startswith(IGNORED)


def test_trusted_document_loads_an_unlisted_plugin(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    name = "trust_named_document_plugin"
    (tmp_path / f"{name}.py").write_text(
        "class _Plugin:\n"
        f"    name = {name!r}\n"
        "    def on_run_start(self, *, state, context): pass\n"
        "    def on_run_success(self, *, state, context): pass\n"
        "    def on_run_failure(self, *, state, context, error): pass\n"
        "plugin = _Plugin()\n",
        encoding="utf-8",
    )
    monkeypatch.syspath_prepend(str(tmp_path))
    monkeypatch.delitem(sys.modules, name, raising=False)

    result = api.run_orchestration(
        orchestration_path=_tool_orch(tmp_path, plugins=[name]), config=_cfg()
    )

    assert result.state["runtime"]["plugins"]["loaded"] == [name]
    assert result.warnings == [f"Applied host settings from orch.yml: plugins: {name}"]


# ── scheduler, REST, MCP ─────────────────────────────────────────────────────


def _capture_runs(monkeypatch: pytest.MonkeyPatch, module: Any) -> list[RunResult]:
    results: list[RunResult] = []

    def _run(req: RunRequest) -> RunResult:
        result = shim_run(req)
        results.append(result)
        return result

    monkeypatch.setattr(module, "run", _run)
    return results


@pytest.mark.parametrize(
    ("job_trust", "expected_url"),
    [({}, DOCUMENT_BASE_URL), ({"trust_document": False}, CONFIG_BASE_URL)],
)
def test_scheduler_job_trusts_its_configured_path_unless_opted_out(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    job_trust: dict[str, Any],
    expected_url: str,
) -> None:
    results = _capture_runs(monkeypatch, scheduler_module)
    job = ScheduledJob(
        name="job",
        orchestration_path=_tool_orch(tmp_path, runtime=_document_runtime()),
        interval_seconds=60,
        **job_trust,
    )

    RecurringScheduler(jobs=[job], config=_cfg()).tick()

    assert [_applied_base_url(r.state) for r in results] == [expected_url]


def test_rest_trigger_stays_limited(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    results = _capture_runs(monkeypatch, rest_module)
    orch = _tool_orch(tmp_path, runtime=_document_runtime())

    response = RestTriggerService(
        allow_unauthenticated=True, config=_cfg(), orchestration_root=tmp_path
    ).handle_http_request(
        method="POST",
        path="/v1/triggers/run",
        headers={},
        body=json.dumps({"orchestration_path": str(orch)}),
    )

    assert response.status_code == 200, response.body
    assert _applied_base_url(results[0].state) == CONFIG_BASE_URL
    assert results[0].warnings[0].startswith(IGNORED)


def test_mcp_validate_stays_limited(tmp_path: Path) -> None:
    orch = _tool_orch(tmp_path, runtime=_document_runtime())

    report = mcp_server._validate_orchestration_impl(str(orch))

    assert any(w.startswith(IGNORED) for w in report["warnings"])
    assert not any("Applied host settings" in w for w in report["warnings"])


def test_mcp_run_stays_limited(tmp_path: Path) -> None:
    orch = _tool_orch(tmp_path, runtime=_document_runtime())

    started = mcp_server._run_orchestration_impl(orchestration=str(orch))
    run_id = started["run_id"]
    deadline = time.monotonic() + 5.0
    final = started
    while final["status"] == "running" and time.monotonic() < deadline:
        time.sleep(0.02)
        final = mcp_server._get_run_state_impl(run_id=run_id)

    assert final["status"] == "completed", final
    assert any(w.startswith(IGNORED) for w in final["warnings"])
    assert not any("Applied host settings" in w for w in final["warnings"])


def test_validate_defaults_to_limited(tmp_path: Path) -> None:
    orch = _tool_orch(tmp_path, runtime=_document_runtime())

    assert any(w.startswith(IGNORED) for w in validate(orch)["warnings"])
    assert validate(orch, trust_document=True)["warnings"] == [NOTICE]
