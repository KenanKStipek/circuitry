"""An orchestration may only set author-level runtime settings.

A document's own `runtime:` block contributes `runtime.complexity` and
`runtime.state` (ORCHESTRATION_RUNTIME_KEYS); every other key is host
configuration that only config.json supplies, and is dropped with a warning.
A document's top-level `plugins:` list only adds modules config already lists.
`trust_orchestration_runtime` (or its env var) restores the old behaviour.
This is the limit for documents that reach `cof` indirectly; a file named by
path is trusted (see test_trust_named_document.py).
No network: adapters are built but never called.
"""

from __future__ import annotations

import json
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest
import yaml
from typer.testing import CliRunner

from circuitry.adapters import build_adapter
from circuitry.adapters.base import GenerateResult
from circuitry.cli import app as app_module
from circuitry.cli.config import CircuitryConfig, resolve_config
from circuitry.cli.effective_settings import (
    ORCHESTRATION_RUNTIME_KEYS,
    resolve_effective_settings,
)
from circuitry.cli.runtime_shim import RunRequest, run, validate

CONFIG_BASE_URL = "https://api.config.example/v1"
DOCUMENT_BASE_URL = "http://document.example/v1"


def _cfg(**overrides: Any) -> CircuitryConfig:
    fields: dict[str, Any] = {
        "default_adapter": "openai",
        "default_model": "gpt-test",
        "runtime": {"adapters": {"openai": {"base_url": CONFIG_BASE_URL}}},
    }
    fields.update(overrides)
    return CircuitryConfig(**fields)


def _document_runtime() -> dict[str, Any]:
    return {"adapters": {"openai": {"base_url": DOCUMENT_BASE_URL}}}


def _write_yaml(path: Path, orch: dict[str, Any]) -> Path:
    path.write_text(yaml.safe_dump(orch, sort_keys=False), encoding="utf-8")
    return path


def _plugin_module(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, name: str) -> str:
    """A runtime-plugin module on sys.path; importing it is what we watch for."""
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
    return name


def _tool_orch(tmp_path: Path, **top_level: Any) -> Path:
    """A document that runs without an adapter (one local tool effect)."""
    orch = {**top_level, "effects": [{"type": "tool", "name": "t", "provider": "uuid"}]}
    return _write_yaml(tmp_path / "orch.yml", orch)


def _run(path: Path, cfg: CircuitryConfig) -> Any:
    result = run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=cfg,
            skip_preflight=True,
        )
    )
    assert result.ok, result.error
    return result


def test_author_level_keys_are_complexity_and_state() -> None:
    assert frozenset({"complexity", "state"}) == ORCHESTRATION_RUNTIME_KEYS


def test_document_cannot_change_adapter_base_url() -> None:
    effective = resolve_effective_settings(
        cfg=_cfg(), orch={"runtime": _document_runtime()}
    )

    adapter = build_adapter(adapter_name="openai", runtime=effective.runtime)
    assert adapter.base_url == CONFIG_BASE_URL  # type: ignore[attr-defined]
    assert effective.sources["runtime"] == "config"
    assert effective.warnings == (
        (
            "Ignored runtime.adapters from the orchestration: it is a host "
            "setting and must go in config.json (an orchestration may only set "
            "runtime.complexity, runtime.state)."
        ),
    )


def test_every_host_key_is_dropped_with_one_warning_each() -> None:
    host_keys = {
        "adapters": _document_runtime()["adapters"],
        "plugins": {"shell": {"binary": "/tmp/not-a-shell"}},
        "persistence": {"enabled": True, "backend": "sqlite", "path": "/tmp/x.db"},
        "library": {"local_root": "/tmp"},
        "mcp": {"servers": {}},
        "made_up_key": 1,
    }
    effective = resolve_effective_settings(
        cfg=_cfg(), orch={"runtime": {**host_keys, "state": {"record_children": True}}}
    )

    for key in ("plugins", "persistence", "library", "mcp", "made_up_key"):
        assert key not in effective.runtime
    assert effective.runtime["adapters"] == {"openai": {"base_url": CONFIG_BASE_URL}}
    assert "persistence" not in effective.sources
    assert len(effective.warnings) == len(host_keys)
    for key, warning in zip(host_keys, effective.warnings, strict=True):
        assert warning.startswith(f"Ignored runtime.{key} from the orchestration")
        assert "must go in config.json" in warning


def test_document_state_and_complexity_still_apply() -> None:
    effective = resolve_effective_settings(
        cfg=_cfg(),
        orch={
            "runtime": {
                "state": {"record_children": True},
                "complexity": {"scoring": {"enabled": True}},
            }
        },
    )

    assert effective.warnings == ()
    assert effective.runtime["state"] == {"record_children": True}
    assert effective.complexity.scoring.enabled is True
    assert effective.sources["runtime"] == "orchestration"
    assert effective.sources["complexity"] == "orchestration"
    assert effective.runtime["adapters"]["openai"]["base_url"] == CONFIG_BASE_URL


def test_document_record_children_still_keeps_the_child_record(tmp_path: Path) -> None:
    _write_yaml(
        tmp_path / "child.yml",
        {
            "outputs": {"id": "prime.t.value"},
            "effects": [{"type": "tool", "name": "t", "provider": "uuid"}],
        },
    )
    parent = _write_yaml(
        tmp_path / "parent.yml",
        {
            "runtime": {"state": {"record_children": True}},
            "effects": [{"type": "use", "name": "sub", "path": "child.yml"}],
        },
    )

    result = _run(parent, _cfg())

    assert result.warnings == []
    assert "t" in result.state["prime"]["sub"]


def test_trusted_config_keeps_the_whole_document_runtime() -> None:
    effective = resolve_effective_settings(
        cfg=_cfg(trust_orchestration_runtime=True),
        orch={"runtime": _document_runtime()},
    )

    adapter = build_adapter(adapter_name="openai", runtime=effective.runtime)
    assert adapter.base_url == DOCUMENT_BASE_URL  # type: ignore[attr-defined]
    assert effective.warnings == (
        (
            "Applied host settings from the orchestration: "
            "runtime.adapters.openai.base_url"
        ),
    )


def test_trust_opt_in_from_config_file_and_env(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("CIRCUITRY_TRUST_ORCHESTRATION_RUNTIME", raising=False)
    plain = tmp_path / "plain.json"
    plain.write_text("{}", encoding="utf-8")
    trusting = tmp_path / "trusting.json"
    trusting.write_text(json.dumps({"trust_orchestration_runtime": True}), encoding="utf-8")

    assert resolve_config(explicit_path=plain).trust_orchestration_runtime is False
    assert resolve_config(explicit_path=trusting).trust_orchestration_runtime is True

    monkeypatch.setenv("CIRCUITRY_TRUST_ORCHESTRATION_RUNTIME", "1")
    assert resolve_config(explicit_path=plain).trust_orchestration_runtime is True

    monkeypatch.setenv("CIRCUITRY_TRUST_ORCHESTRATION_RUNTIME", "0")
    assert resolve_config(explicit_path=trusting).trust_orchestration_runtime is False


def test_document_plugin_not_listed_in_config_is_not_imported(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    name = _plugin_module(tmp_path, monkeypatch, "scope_unlisted_plugin")

    result = _run(_tool_orch(tmp_path, plugins=[name]), _cfg())

    assert name not in sys.modules
    assert result.state["runtime"]["plugins"]["configured"] == []
    assert result.warnings == [
        (
            f"Skipped plugin '{name}' from the orchestration: it is not listed "
            "in config.json 'plugins' or 'enabled_plugins'."
        )
    ]


def test_document_plugin_listed_in_enabled_plugins_loads(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    name = _plugin_module(tmp_path, monkeypatch, "scope_enabled_plugin")

    result = _run(_tool_orch(tmp_path, plugins=[name]), _cfg(enabled_plugins=[name]))

    assert result.state["runtime"]["plugins"]["loaded"] == [name]
    assert result.warnings == []


def test_trusted_config_loads_unlisted_document_plugin(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    name = _plugin_module(tmp_path, monkeypatch, "scope_trusted_plugin")

    result = _run(
        _tool_orch(tmp_path, plugins=[name]), _cfg(trust_orchestration_runtime=True)
    )

    assert result.state["runtime"]["plugins"]["loaded"] == [name]
    assert result.warnings == [f"Applied host settings from orch.yml: plugins: {name}"]


@dataclass
class _RecordingAdapter:
    name: str
    calls: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append(model)
        return GenerateResult(text="ok", raw={})


def test_use_child_cannot_set_host_settings_either(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A composed child runs on the parent's resolved runtime, never its own."""
    name = _plugin_module(tmp_path, monkeypatch, "scope_child_plugin")
    _write_yaml(
        tmp_path / "child.yml",
        {
            "plugins": [name],
            "runtime": _document_runtime(),
            "effects": [
                {
                    "type": "prompt",
                    "name": "ask",
                    "provider": "openai:gpt-test",
                    "template": "hi",
                }
            ],
        },
    )
    parent = _write_yaml(
        tmp_path / "parent.yml",
        {"effects": [{"type": "use", "name": "sub", "path": "child.yml"}]},
    )
    built: list[dict[str, Any]] = []

    def _capture(*, adapter_name: str, runtime: dict[str, Any]) -> _RecordingAdapter:
        built.append(runtime["adapters"][adapter_name])
        return _RecordingAdapter(name=adapter_name)

    monkeypatch.setattr("circuitry.core.prompt.build_adapter", _capture)

    result = run(
        RunRequest(
            orchestration_path=parent,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=_cfg(),
            adapter=_RecordingAdapter(name="ollama"),
            skip_preflight=True,
        )
    )

    assert result.ok, result.error
    assert built == [{"base_url": CONFIG_BASE_URL}]
    assert name not in sys.modules


def test_validate_reports_dropped_document_settings(tmp_path: Path) -> None:
    report = validate(
        _tool_orch(tmp_path, runtime=_document_runtime()),
        config=_cfg(),
        skip_preflight=True,
    )

    assert report["ok"] is True
    assert any(w.startswith("Ignored runtime.adapters") for w in report["warnings"])


runner = CliRunner()


def _config_file(tmp_path: Path) -> Path:
    path = tmp_path / "config.json"
    path.write_text(json.dumps({"runtime": {"adapters": {}}}), encoding="utf-8")
    return path


def test_cof_run_prints_dropped_settings_on_stderr(tmp_path: Path) -> None:
    # Run by library name: a file named by path would be trusted.
    folder = tmp_path / "lib"
    folder.mkdir()
    _tool_orch(folder, runtime=_document_runtime())
    config = tmp_path / "config.json"
    config.write_text(
        json.dumps(
            {
                "runtime": {
                    "adapters": {},
                    "library": {
                        "sources": [{"type": "folder", "name": "local", "path": str(folder)}]
                    },
                }
            }
        ),
        encoding="utf-8",
    )

    result = runner.invoke(
        app_module.app, ["run", "local:orch", "-c", str(config), "--skip-preflight"]
    )

    assert result.exit_code == 0, result.output
    assert "Warning: Ignored runtime.adapters from the orchestration" in result.stderr
    assert "must go in config.json" in result.stderr
    # Piped stdout stays pure JSON.
    assert "Warning" not in result.stdout
    json.loads(result.stdout)


def test_cof_check_escapes_document_keys_in_its_report(tmp_path: Path) -> None:
    # `cof check` names a file by path, so it reports the applied-settings
    # notice; that quotes document keys, which must not be read as markup.
    orch = _tool_orch(tmp_path, runtime={"[/x]": 1, "[conceal]hidden": 2})

    result = runner.invoke(
        app_module.app,
        ["check", str(orch), "-c", str(_config_file(tmp_path)), "--skip-preflight"],
    )

    assert result.exit_code == 0, result.output
    output = " ".join(result.output.split())
    assert (
        "Warning: Applied host settings from orch.yml: "
        "runtime.[/x], runtime.[conceal]hidden"
    ) in output
