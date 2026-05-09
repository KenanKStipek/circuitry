"""Tests for Story 0 — adapter / tool / runtime-plugin allowlist subsystem."""

from __future__ import annotations

from pathlib import Path

import pytest
import typer.testing

from circuitry.cli.allowlist import (
    _provider_token_to_adapter,
    check_allowlist,
    walk_orchestration_refs,
)
from circuitry.cli.app import app
from circuitry.cli.config import CircuitryConfig, resolve_config
from circuitry.cli.runtime_shim import validate
from circuitry.core.runtime_plugins import load_plugins


def _write(tmp_path: Path, name: str, content: str) -> Path:
    p = tmp_path / name
    p.write_text(content, encoding="utf-8")
    return p


# ---------- walk_orchestration_refs ----------


def test_walk_collects_top_level_adapter() -> None:
    orch = {
        "adapter": "ollama",
        "effects": [{"type": "prompt", "name": "g", "template": "x"}],
    }
    adapters, tools = walk_orchestration_refs(orch)
    assert adapters == {"ollama"}
    assert tools == set()


def test_walk_collects_prompt_provider_and_fallbacks() -> None:
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "g",
                "template": "x",
                "provider": "openai:gpt-4o",
                "provider_fallbacks": ["anthropic:claude", "ollama"],
            }
        ]
    }
    adapters, tools = walk_orchestration_refs(orch)
    assert adapters == {"openai", "anthropic", "ollama"}
    assert tools == set()


def test_walk_collects_tool_provider() -> None:
    orch = {
        "effects": [
            {"type": "tool", "name": "t", "provider": "ffmpeg"},
            {"type": "tool", "name": "u", "provider": "comfyui"},
        ]
    }
    adapters, tools = walk_orchestration_refs(orch)
    assert tools == {"ffmpeg", "comfyui"}
    assert adapters == set()


def test_walk_recurses_through_dynamic_loop_conditional_reflector() -> None:
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "d",
                "effects": [
                    {
                        "type": "loop",
                        "name": "l",
                        "body": [{"type": "tool", "name": "ff", "provider": "ffmpeg"}],
                    },
                    {
                        "type": "if",
                        "if": {"mode": "literal", "value": True},
                        "then": [{"type": "tool", "name": "c", "provider": "comfyui"}],
                        "else": [
                            {
                                "type": "reflector",
                                "name": "r",
                                "effects": [
                                    {
                                        "type": "prompt",
                                        "name": "p",
                                        "template": "x",
                                        "provider": "groq",
                                    }
                                ],
                            }
                        ],
                    },
                ],
            }
        ]
    }
    adapters, tools = walk_orchestration_refs(orch)
    assert adapters == {"groq"}
    assert tools == {"ffmpeg", "comfyui"}


def test_provider_token_handles_edge_cases() -> None:
    assert _provider_token_to_adapter("") is None
    assert _provider_token_to_adapter("   ") is None
    assert _provider_token_to_adapter("openai") == "openai"
    assert _provider_token_to_adapter("openai:gpt-4o") == "openai"
    assert _provider_token_to_adapter(":model") is None  # head empty


# ---------- check_allowlist ----------


def test_check_allowlist_default_open_passes() -> None:
    orch = {"adapter": "openai", "effects": []}
    cfg = CircuitryConfig()  # all enabled_* are None
    assert check_allowlist(orch=orch, config=cfg) == []


def test_check_allowlist_strict_rejects_missing_adapter() -> None:
    orch = {"adapter": "openai", "effects": []}
    cfg = CircuitryConfig(enabled_adapters=["ollama"])
    errors = check_allowlist(orch=orch, config=cfg)
    assert len(errors) == 1
    assert (
        errors[0]
        == "adapter 'openai' not in enabled_adapters allowlist (enabled: ['ollama'])"
    )


def test_check_allowlist_lockdown_rejects_all_tools() -> None:
    orch = {
        "effects": [
            {"type": "tool", "name": "t", "provider": "ffmpeg"},
        ]
    }
    cfg = CircuitryConfig(enabled_tools=[])  # AC 0.3
    errors = check_allowlist(orch=orch, config=cfg)
    assert len(errors) == 1
    assert "tool 'ffmpeg' not in enabled_tools allowlist (enabled: [])" in errors[0]


def test_check_allowlist_passes_when_listed() -> None:
    orch = {
        "adapter": "ollama",
        "effects": [{"type": "tool", "name": "t", "provider": "ffmpeg"}],
    }
    cfg = CircuitryConfig(enabled_adapters=["ollama"], enabled_tools=["ffmpeg"])
    assert check_allowlist(orch=orch, config=cfg) == []


# ---------- validate() integration ----------


def test_validate_default_open_with_no_config(tmp_path: Path) -> None:
    """AC 0.1: no config / unset env → validation passes."""
    p = _write(
        tmp_path,
        "ok.yml",
        "adapter: openai\neffects:\n  - {type: prompt, name: g, template: x}\n",
    )
    result = validate(p)
    assert result["ok"] is True


def test_validate_strict_rejects_disallowed_adapter(tmp_path: Path) -> None:
    """AC 0.2: strict allowlist denies non-listed adapter."""
    p = _write(
        tmp_path,
        "bad.yml",
        "adapter: openai\neffects:\n  - {type: prompt, name: g, template: x}\n",
    )
    cfg = CircuitryConfig(enabled_adapters=["ollama"])
    result = validate(p, config=cfg)
    assert result["ok"] is False
    assert any("openai" in e and "enabled_adapters" in e for e in result["errors"])


def test_validate_lockdown_rejects_tool_effect(tmp_path: Path) -> None:
    """AC 0.3: empty-string lockdown rejects every tool reference."""
    p = _write(
        tmp_path,
        "tool.yml",
        "adapter: ollama\neffects:\n  - {type: tool, name: t, provider: ffmpeg}\n",
    )
    cfg = CircuitryConfig(enabled_adapters=["ollama"], enabled_tools=[])
    result = validate(p, config=cfg)
    assert result["ok"] is False
    assert any("ffmpeg" in e and "enabled_tools" in e for e in result["errors"])


# ---------- env vs config precedence ----------


def test_env_var_wins_over_config_json(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC 0.4: CIRCUITRY_ENABLED_PLUGINS overrides config.json enabled_plugins."""
    cfg_path = _write(
        tmp_path,
        "config.json",
        '{"enabled_plugins": ["postgres"]}\n',
    )
    monkeypatch.setenv("CIRCUITRY_ENABLED_PLUGINS", "mongodb")
    cfg = resolve_config(explicit_path=cfg_path)
    assert cfg.enabled_plugins == ["mongodb"]


def test_env_var_empty_string_is_lockdown(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("CIRCUITRY_ENABLED_TOOLS", "")
    cfg = resolve_config()
    assert cfg.enabled_tools == []


# ---------- load_plugins() ----------


def test_load_plugins_default_open_when_allowed_none() -> None:
    """When `allowed` is None, no allowlist check is applied."""
    results = load_plugins(["nonexistent.module:plugin"], allowed=None)
    # The import will fail, but that's a load error — not an allowlist error.
    assert results[0].plugin is None
    assert "not in enabled_plugins allowlist" not in (results[0].error or "")


def test_load_plugins_filters_disallowed() -> None:
    results = load_plugins(["foo.bar:plugin"], allowed=["other.plugin"])
    assert results[0].plugin is None
    assert "not in enabled_plugins allowlist" in (results[0].error or "")
    assert "['other.plugin']" in (results[0].error or "")


def test_load_plugins_lockdown_rejects_all() -> None:
    results = load_plugins(["a.b:p"], allowed=[])
    assert results[0].plugin is None
    assert "not in enabled_plugins allowlist" in (results[0].error or "")


# ---------- circuitry list --extensions ----------


def test_list_extensions_json_default_open() -> None:
    """AC 0.5: `circuitry list --extensions` shows compiled-in items."""
    runner = typer.testing.CliRunner()
    result = runner.invoke(app, ["list", "--extensions", "--json"])
    assert result.exit_code == 0
    import json as _json

    payload = _json.loads(result.stdout)
    assert "adapters" in payload
    assert "tool_plugins" in payload
    assert "runtime_plugins" in payload
    adapter_names = {a["name"] for a in payload["adapters"]}
    assert {"ollama", "openai", "anthropic", "litellm", "host_claude"} <= adapter_names
    assert all(
        a["status"] == "compiled-in (default-open)" for a in payload["adapters"]
    )


def test_list_extensions_json_strict_status(monkeypatch: pytest.MonkeyPatch) -> None:
    """AC 0.5: status reflects per-item allowlist membership."""
    monkeypatch.setenv("CIRCUITRY_ENABLED_ADAPTERS", "ollama")
    runner = typer.testing.CliRunner()
    result = runner.invoke(app, ["list", "--extensions", "--json"])
    assert result.exit_code == 0
    import json as _json

    payload = _json.loads(result.stdout)
    by_name = {a["name"]: a["status"] for a in payload["adapters"]}
    assert by_name["ollama"] == "enabled"
    assert by_name["openai"] == "disabled (not in allowlist)"
