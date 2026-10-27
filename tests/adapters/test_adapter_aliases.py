"""Aliases of any adapter except `host_claude` (#656): a
`runtime.adapters.<alias>` entry that sets `adapter` builds that target's
class, under the alias's own name, from the alias's own settings only. See
`tests/adapters/test_cli_adapter_aliases.py` for the narrower claude_code/pi
CLI-alias coverage that predates this (#579)."""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from circuitry.adapters.factory import (
    ADAPTER_REGISTRY,
    alias_config_errors,
    alias_names,
    build_adapter,
    configured_timeout_seconds,
)
from circuitry.adapters.groq import GroqAdapter
from circuitry.adapters.lmstudio import LMStudioAdapter
from circuitry.adapters.ollama import OllamaAdapter
from circuitry.allowlist_gate import allowed_adapters
from circuitry.api import run_orchestration
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.effective_settings import resolve_effective_settings
from circuitry.cli.runtime_shim import validate


def _runtime(**adapters: dict[str, Any]) -> dict[str, Any]:
    return {"adapters": adapters}


# ---------------------------------------------------------------------------
# Every registry adapter except host_claude can be aliased
# ---------------------------------------------------------------------------

#: Settings that let each target build without error; a target that ignores
#: an unrelated key (base_url on a CLI adapter) is still exercised, matching
#: "keys the target doesn't read are ignored, as on the target's own block".
_ALIAS_SETTINGS: dict[str, Any] = {"base_url": "http://alias-target.example/v1"}


@pytest.mark.parametrize("target", sorted(n for n in ADAPTER_REGISTRY if n != "host_claude"))
def test_every_registry_adapter_except_host_claude_can_be_aliased(target: str) -> None:
    runtime = _runtime(
        **{target: {"base_url": "http://base.example/v1"}},
        my_alias={"adapter": target, **_ALIAS_SETTINGS},
    )
    alias = build_adapter(adapter_name="my_alias", runtime=runtime)
    base = build_adapter(adapter_name=target, runtime=runtime)
    assert type(alias) is type(base)
    assert alias.name == "my_alias"
    assert base.name == target
    # The alias never touched the base's own block: its base_url came from
    # the alias's own settings, the base's from its own.
    if hasattr(alias, "base_url"):
        assert alias.base_url == "http://alias-target.example/v1"
        assert base.base_url == "http://base.example/v1"


def test_host_claude_cannot_be_an_alias_target() -> None:
    runtime = _runtime(my_alias={"adapter": "host_claude"})
    with pytest.raises(ValueError) as excinfo:
        build_adapter(adapter_name="my_alias", runtime=runtime)
    assert str(excinfo.value) == (
        "runtime.adapters.my_alias.adapter cannot be 'host_claude': it is "
        "supplied at run time by circuitry-mcp, never built from config"
    )


# ---------------------------------------------------------------------------
# Target normalisation and the four error texts
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("raw_target", ["groq", " groq", "GROQ", " Groq \t"])
def test_a_target_is_normalised_with_strip_and_lower(raw_target: str) -> None:
    runtime = _runtime(my_alias={"adapter": raw_target})
    adapter = build_adapter(adapter_name="my_alias", runtime=runtime)
    assert isinstance(adapter, GroqAdapter)
    assert adapter.name == "my_alias"


def test_an_unknown_or_non_string_target_is_a_config_error() -> None:
    for bad_target in ("nonesuch", 5, None, True):
        runtime = _runtime(my_alias={"adapter": bad_target})
        with pytest.raises(ValueError) as excinfo:
            build_adapter(adapter_name="my_alias", runtime=runtime)
        assert str(excinfo.value) == (
            f"runtime.adapters.my_alias.adapter must name a built-in adapter, "
            f"got {bad_target!r}"
        )


def test_a_target_that_is_itself_an_alias_is_a_config_error() -> None:
    runtime = _runtime(
        base_alias={"adapter": "groq"}, my_alias={"adapter": "base_alias"}
    )
    with pytest.raises(ValueError) as excinfo:
        build_adapter(adapter_name="my_alias", runtime=runtime)
    assert str(excinfo.value) == (
        "runtime.adapters.my_alias.adapter must name a built-in adapter, got "
        "'base_alias', which is itself an alias"
    )


@pytest.mark.parametrize("name", sorted(ADAPTER_REGISTRY))
def test_an_alias_named_like_any_built_in_adapter_is_a_config_error(name: str) -> None:
    runtime = _runtime(**{name: {"adapter": "groq"}})
    with pytest.raises(ValueError) as excinfo:
        build_adapter(adapter_name=name, runtime=runtime)
    assert str(excinfo.value) == (
        f"runtime.adapters.{name}.adapter is set, but {name!r} is a built-in "
        "adapter; an alias needs a name of its own"
    )


# ---------------------------------------------------------------------------
# alias_config_errors: every alias in the merged runtime, name order
# ---------------------------------------------------------------------------


def test_alias_config_errors_is_empty_when_every_alias_builds() -> None:
    runtime = _runtime(
        local_a={"adapter": "lmstudio", "base_url": "http://a/v1"},
        local_b={"adapter": "lmstudio", "base_url": "http://b/v1"},
    )
    assert alias_config_errors(runtime) == []


def test_alias_config_errors_lists_every_broken_alias_in_name_order() -> None:
    runtime = _runtime(
        zz_bad={"adapter": "nonesuch"},
        aa_bad={"adapter": "also-nonesuch"},
        ok={"adapter": "groq"},
    )
    errors = alias_config_errors(runtime)
    assert len(errors) == 2
    assert errors[0].startswith("runtime.adapters.aa_bad.adapter")
    assert errors[1].startswith("runtime.adapters.zz_bad.adapter")


def test_alias_names_lists_every_adapters_entry_with_an_adapter_key() -> None:
    runtime = _runtime(
        a={"adapter": "groq"}, b={"base_url": "http://x"}, groq={"base_url": "http://y"}
    )
    assert alias_names(runtime) == ("a",)


# ---------------------------------------------------------------------------
# cof check / cof run agree: the issue's own reproduction
# ---------------------------------------------------------------------------


def test_cof_check_and_run_agree_on_the_issues_reproduction(tmp_path: Path) -> None:
    doc = tmp_path / "alias.yml"
    doc.write_text(
        "effects:\n  - type: prompt\n    name: line\n    template: hi\n",
        encoding="utf-8",
    )
    config = CircuitryConfig(
        default_adapter="apple_fm",
        default_model="apple-on-device",
        runtime=_runtime(
            apple_fm={
                "adapter": "lmstudio",
                "base_url": "http://127.0.0.1:1/v1",
                "default_model": "apple-on-device",
            }
        ),
    )
    result = validate(doc, config=config, skip_preflight=True)
    assert result["ok"], result

    broken_config = CircuitryConfig(
        default_adapter="apple_fm",
        default_model="apple-on-device",
        runtime=_runtime(apple_fm={"adapter": "nonesuch"}),
    )
    broken_result = validate(doc, config=broken_config, skip_preflight=True)
    assert not broken_result["ok"]
    assert any(
        "runtime.adapters.apple_fm.adapter must name a built-in adapter" in e
        for e in broken_result["errors"]
    )

    run_result = run_orchestration(
        orchestration_path=doc, config=broken_config, raise_on_error=False
    )
    assert not run_result.ok
    assert run_result.error is not None
    assert str(run_result.error).startswith("Invalid adapter aliases:\n")
    assert (
        "runtime.adapters.apple_fm.adapter must name a built-in adapter"
        in str(run_result.error)
    )


def test_cof_check_reports_a_broken_alias_used_by_nothing(tmp_path: Path) -> None:
    doc = tmp_path / "alias.yml"
    doc.write_text(
        "effects:\n  - type: prompt\n    name: line\n    template: hi\n",
        encoding="utf-8",
    )
    config = CircuitryConfig(
        default_model="m", runtime=_runtime(unused_alias={"adapter": "nonesuch"})
    )
    result = validate(doc, config=config, skip_preflight=True)
    assert not result["ok"]
    assert any(
        "runtime.adapters.unused_alias.adapter must name a built-in adapter" in e
        for e in result["errors"]
    )


def test_cof_check_skip_preflight_still_reports_a_broken_alias(tmp_path: Path) -> None:
    doc = tmp_path / "alias.yml"
    doc.write_text(
        "effects:\n  - type: prompt\n    name: line\n    template: hi\n",
        encoding="utf-8",
    )
    config = CircuitryConfig(
        default_model="m", runtime=_runtime(bad={"adapter": "nonesuch"})
    )
    with_preflight = validate(doc, config=config, skip_preflight=False)
    without_preflight = validate(doc, config=config, skip_preflight=True)
    assert not with_preflight["ok"] and not without_preflight["ok"]
    assert with_preflight["errors"] == without_preflight["errors"]


# ---------------------------------------------------------------------------
# A keyless groq alias: soft with on_error: skip, hard otherwise
# ---------------------------------------------------------------------------


def test_a_keyless_groq_alias_is_hard_by_default(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("GROQ_API_KEY", raising=False)
    doc = tmp_path / "doc.yml"
    doc.write_text(
        "effects:\n  - type: prompt\n    name: a\n    provider: groq_alt\n    template: hi\n",
        encoding="utf-8",
    )
    config = CircuitryConfig(default_model="m", runtime=_runtime(groq_alt={"adapter": "groq"}))
    result = validate(doc, config=config)
    assert not result["ok"]
    assert any("adapter:groq_alt" in e for e in result["errors"])


def test_a_keyless_groq_alias_is_soft_with_on_error_skip(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("GROQ_API_KEY", raising=False)
    doc = tmp_path / "doc.yml"
    doc.write_text(
        "effects:\n  - type: prompt\n    name: a\n    provider: groq_alt\n"
        "    template: hi\n    on_error: skip\n",
        encoding="utf-8",
    )
    config = CircuitryConfig(default_model="m", runtime=_runtime(groq_alt={"adapter": "groq"}))
    result = validate(doc, config=config)
    assert result["ok"], result
    assert any("groq_alt" in w and "will skip" in w for w in result["warnings"])


# ---------------------------------------------------------------------------
# Timeout and allowlist by the alias's own name
# ---------------------------------------------------------------------------


def test_configured_timeout_seconds_is_the_aliass_own() -> None:
    runtime = _runtime(
        groq={"timeout_seconds": 10},
        groq_alt={"adapter": "groq", "timeout_seconds": 77},
    )
    assert configured_timeout_seconds("groq_alt", runtime) == 77
    assert configured_timeout_seconds("groq", runtime) == 10


def test_allowed_adapters_lists_the_alias_by_its_own_name() -> None:
    runtime_config: dict[str, Any] = {}
    from circuitry.allowlist_gate import install_allowlists

    install_allowlists(runtime_config, enabled_adapters=["groq_alt"], enabled_tools=None)
    assert allowed_adapters(runtime_config) == ["groq_alt"]


# ---------------------------------------------------------------------------
# The strict-allowlist rule: enabled_adapters set, a document's own
# runtime.adapters.<name>.adapter key is ignored
# ---------------------------------------------------------------------------


def test_a_documents_new_alias_is_ignored_when_enabled_adapters_is_set() -> None:
    cfg = CircuitryConfig(enabled_adapters=["ollama"], default_model="m")
    orch = {"runtime": {"adapters": {"new_alias": {"adapter": "groq"}}}}
    effective = resolve_effective_settings(
        cfg=cfg, orch=orch, trust_document=True, document_name="doc.yml"
    )
    assert "new_alias" not in (effective.runtime.get("adapters") or {})
    assert any(
        w == "runtime.adapters.new_alias.adapter from doc.yml ignored: with "
        "enabled_adapters set, only config defines adapter aliases"
        for w in effective.warnings
    )


def test_a_documents_retarget_of_a_config_alias_is_ignored_when_enabled_adapters_is_set() -> None:
    cfg = CircuitryConfig(
        enabled_adapters=["local_a"],
        default_model="m",
        runtime=_runtime(local_a={"adapter": "lmstudio", "base_url": "http://config/v1"}),
    )
    orch = {"runtime": {"adapters": {"local_a": {"adapter": "openai"}}}}
    effective = resolve_effective_settings(
        cfg=cfg, orch=orch, trust_document=True, document_name="doc.yml"
    )
    assert effective.runtime["adapters"]["local_a"] == {
        "adapter": "lmstudio",
        "base_url": "http://config/v1",
    }
    assert any(
        "runtime.adapters.local_a.adapter from doc.yml ignored" in w
        for w in effective.warnings
    )
    built = build_adapter(adapter_name="local_a", runtime=effective.runtime)
    assert isinstance(built, LMStudioAdapter)
    assert built.base_url == "http://config/v1"


def test_a_fully_stripped_retarget_is_not_recorded_as_applied() -> None:
    # The document's only key for `local_a` is `adapter`, so once it is
    # dropped nothing from the document actually applies: `runtime`'s own
    # source must say so ("config", not "orchestration"), and no "Applied
    # host settings" notice should name `runtime.adapters` for it.
    cfg = CircuitryConfig(
        enabled_adapters=["local_a"],
        default_model="m",
        runtime=_runtime(local_a={"adapter": "lmstudio", "base_url": "http://config/v1"}),
    )
    orch = {"runtime": {"adapters": {"local_a": {"adapter": "openai"}}}}
    effective = resolve_effective_settings(
        cfg=cfg, orch=orch, trust_document=True, document_name="doc.yml"
    )
    assert effective.sources["runtime"] == "config"
    assert not any(w.startswith("Applied host settings") for w in effective.warnings)


def test_the_applied_settings_notice_precedes_the_alias_retarget_warning() -> None:
    # `cof check`'s own `orchestration_host_setting_warnings` and `cof run`'s
    # `resolve_effective_settings` must agree on the order: the notice,
    # then the alias warning.
    cfg = CircuitryConfig(
        enabled_adapters=["local_a"],
        default_model="m",
        runtime=_runtime(local_a={"adapter": "lmstudio", "base_url": "http://config/v1"}),
    )
    orch = {
        "runtime": {
            "adapters": {"local_a": {"adapter": "openai", "base_url": "http://doc/v1"}}
        }
    }
    effective = resolve_effective_settings(
        cfg=cfg, orch=orch, trust_document=True, document_name="doc.yml"
    )
    from circuitry.cli.effective_settings import orchestration_host_setting_warnings

    check_warnings = orchestration_host_setting_warnings(orch, cfg, trust_document=True, document_name="doc.yml")
    for warnings in (effective.warnings, check_warnings):
        notice_index = next(
            i for i, w in enumerate(warnings) if w.startswith("Applied host settings")
        )
        alias_index = next(
            i for i, w in enumerate(warnings) if "adapter from doc.yml ignored" in w
        )
        assert notice_index < alias_index, warnings


def test_an_untrusted_documents_plugin_warning_precedes_its_runtime_warning() -> None:
    # `cof check`'s own untrusted-document order (`_split_orchestration_
    # plugins` then `_split_orchestration_runtime`) must match `cof run`'s.
    cfg = CircuitryConfig(default_model="m")
    orch = {
        "plugins": ["not_listed"],
        "runtime": {"adapters": {"ollama": {"base_url": "http://x"}}},
    }
    effective = resolve_effective_settings(cfg=cfg, orch=orch, document_name="doc.yml")
    plugin_index = next(
        i for i, w in enumerate(effective.warnings) if w.startswith("Skipped plugin")
    )
    runtime_index = next(
        i for i, w in enumerate(effective.warnings) if w.startswith("Ignored runtime.")
    )
    assert plugin_index < runtime_index, effective.warnings


def test_a_documents_base_url_only_override_still_applies_when_enabled_adapters_is_set() -> None:
    cfg = CircuitryConfig(
        enabled_adapters=["local_a"],
        default_model="m",
        runtime=_runtime(local_a={"adapter": "lmstudio", "base_url": "http://config/v1"}),
    )
    orch = {"runtime": {"adapters": {"local_a": {"base_url": "http://override/v1"}}}}
    effective = resolve_effective_settings(
        cfg=cfg, orch=orch, trust_document=True, document_name="doc.yml"
    )
    assert effective.runtime["adapters"]["local_a"] == {
        "adapter": "lmstudio",
        "base_url": "http://override/v1",
    }
    assert not any("ignored" in w for w in effective.warnings)


def test_without_enabled_adapters_a_document_keeps_its_current_power() -> None:
    cfg = CircuitryConfig(default_model="m")
    orch = {"runtime": {"adapters": {"new_alias": {"adapter": "groq"}}}}
    effective = resolve_effective_settings(
        cfg=cfg, orch=orch, trust_document=True, document_name="doc.yml"
    )
    assert effective.runtime["adapters"]["new_alias"] == {"adapter": "groq"}
    assert not any("ignored" in w for w in effective.warnings)


# ---------------------------------------------------------------------------
# ollama's hint names the alias's own block
# ---------------------------------------------------------------------------


def test_ollamas_unreachable_hint_names_the_aliass_own_block(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from dataclasses import dataclass

    @dataclass(frozen=True)
    class FakeProc:
        returncode: int
        stdout: str = ""
        stderr: str = ""

        def communicate(self, input: object = None, timeout: object = None) -> tuple[str, str]:
            return (self.stdout, self.stderr)

        def __enter__(self) -> FakeProc:
            return self

        def __exit__(self, *exc: object) -> bool:
            return False

    def fake_run(*args: Any, **kwargs: Any) -> FakeProc:
        del args, kwargs
        return FakeProc(returncode=7, stderr="curl: (7) Failed to connect")

    monkeypatch.setattr("subprocess.Popen", fake_run)
    runtime = _runtime(local_ollama={"adapter": "ollama", "base_url": "http://x/v1"})
    adapter = build_adapter(adapter_name="local_ollama", runtime=runtime)
    assert isinstance(adapter, OllamaAdapter)
    with pytest.raises(RuntimeError) as excinfo:
        adapter.generate(model="m", prompt="x")
    assert "runtime.adapters.local_ollama.base_url" in str(excinfo.value)
    assert "runtime.adapters.ollama.base_url" not in str(excinfo.value)
