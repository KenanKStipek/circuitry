from __future__ import annotations

import json
import os
from pathlib import Path
from unittest.mock import patch

import pytest

from circuitry.cli.config import (
    SANE_DEFAULTS,
    CircuitryConfig,
    _apply_env_vars,
    _deep_merge,
    resolve_config,
)


# ---------------------------------------------------------------------------
# _deep_merge
# ---------------------------------------------------------------------------


def test_deep_merge_flat_overlay():
    base = {"a": 1, "b": 2}
    overlay = {"b": 99, "c": 3}
    result = _deep_merge(base, overlay)
    assert result == {"a": 1, "b": 99, "c": 3}


def test_deep_merge_nested_dict():
    base = {"runtime": {"adapters": {"ollama": {"base_url": "http://a"}}}}
    overlay = {"runtime": {"adapters": {"ollama": {"timeout": 30}}}}
    result = _deep_merge(base, overlay)
    assert result["runtime"]["adapters"]["ollama"] == {
        "base_url": "http://a",
        "timeout": 30,
    }


def test_deep_merge_overlay_replaces_non_dict():
    base = {"x": {"nested": True}}
    overlay = {"x": "flat_now"}
    result = _deep_merge(base, overlay)
    assert result["x"] == "flat_now"


def test_deep_merge_does_not_mutate_base():
    base = {"a": {"b": 1}}
    overlay = {"a": {"c": 2}}
    _deep_merge(base, overlay)
    assert base == {"a": {"b": 1}}


# ---------------------------------------------------------------------------
# _apply_env_vars
# ---------------------------------------------------------------------------


def test_apply_env_vars_model(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("CIRCUITRY_MODEL", "gpt-4o")
    result = _apply_env_vars({"default_model": "llama3.1:8b"})
    assert result["default_model"] == "gpt-4o"


def test_apply_env_vars_adapter(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("CIRCUITRY_ADAPTER", "openai")
    result = _apply_env_vars({"default_adapter": "ollama"})
    assert result["default_adapter"] == "openai"


def test_apply_env_vars_url_uses_current_adapter(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("CIRCUITRY_ADAPTER_URL", "http://remote:8080")
    base = {
        "default_adapter": "ollama",
        "runtime": {"adapters": {"ollama": {"base_url": "http://localhost:11434"}}},
    }
    result = _apply_env_vars(base)
    assert result["runtime"]["adapters"]["ollama"]["base_url"] == "http://remote:8080"


def test_apply_env_vars_noop_when_unset(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.delenv("CIRCUITRY_MODEL", raising=False)
    monkeypatch.delenv("CIRCUITRY_ADAPTER", raising=False)
    monkeypatch.delenv("CIRCUITRY_ADAPTER_URL", raising=False)
    base = dict(SANE_DEFAULTS)
    result = _apply_env_vars(base)
    assert result["default_model"] == SANE_DEFAULTS["default_model"]


# ---------------------------------------------------------------------------
# resolve_config — sane defaults
# ---------------------------------------------------------------------------


def test_resolve_config_returns_sane_defaults_when_no_files(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.delenv("CIRCUITRY_CONFIG", raising=False)
    monkeypatch.delenv("CIRCUITRY_MODEL", raising=False)
    monkeypatch.delenv("CIRCUITRY_ADAPTER", raising=False)
    monkeypatch.delenv("CIRCUITRY_ADAPTER_URL", raising=False)

    # Patch GLOBAL_CONFIG_PATH to a non-existent path
    fake_global = tmp_path / "no-global" / "config.json"
    with patch("circuitry.cli.config.GLOBAL_CONFIG_PATH", fake_global):
        cfg = resolve_config(cwd=tmp_path)

    assert cfg.default_model == "llama3.1:8b"
    assert cfg.default_adapter == "ollama"
    assert cfg.runtime["adapters"]["ollama"]["base_url"] == "http://localhost:11434"


# ---------------------------------------------------------------------------
# resolve_config — project-local file
# ---------------------------------------------------------------------------


def test_resolve_config_project_local_overrides_defaults(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.delenv("CIRCUITRY_CONFIG", raising=False)
    monkeypatch.delenv("CIRCUITRY_MODEL", raising=False)
    monkeypatch.delenv("CIRCUITRY_ADAPTER", raising=False)
    monkeypatch.delenv("CIRCUITRY_ADAPTER_URL", raising=False)

    local = tmp_path / "circuitry.config.json"
    local.write_text(
        json.dumps({"default_model": "custom-model", "plugins": ["my.plugin"]}),
        encoding="utf-8",
    )
    fake_global = tmp_path / "no-global" / "config.json"
    with patch("circuitry.cli.config.GLOBAL_CONFIG_PATH", fake_global):
        cfg = resolve_config(cwd=tmp_path)

    assert cfg.default_model == "custom-model"
    assert cfg.plugins == ["my.plugin"]
    # Sane defaults for adapter should still be present
    assert cfg.default_adapter == "ollama"


# ---------------------------------------------------------------------------
# resolve_config — global config
# ---------------------------------------------------------------------------


def test_resolve_config_global_config_layers(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.delenv("CIRCUITRY_CONFIG", raising=False)
    monkeypatch.delenv("CIRCUITRY_MODEL", raising=False)
    monkeypatch.delenv("CIRCUITRY_ADAPTER", raising=False)
    monkeypatch.delenv("CIRCUITRY_ADAPTER_URL", raising=False)

    global_dir = tmp_path / "global"
    global_dir.mkdir()
    global_cfg = global_dir / "config.json"
    global_cfg.write_text(
        json.dumps({"default_adapter": "openai"}), encoding="utf-8"
    )

    project_dir = tmp_path / "project"
    project_dir.mkdir()

    with patch("circuitry.cli.config.GLOBAL_CONFIG_PATH", global_cfg):
        cfg = resolve_config(cwd=project_dir)

    assert cfg.default_adapter == "openai"
    # Model still comes from sane defaults
    assert cfg.default_model == "llama3.1:8b"


# ---------------------------------------------------------------------------
# resolve_config — explicit path
# ---------------------------------------------------------------------------


def test_resolve_config_explicit_path_skips_discovery(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.delenv("CIRCUITRY_CONFIG", raising=False)
    monkeypatch.delenv("CIRCUITRY_MODEL", raising=False)
    monkeypatch.delenv("CIRCUITRY_ADAPTER", raising=False)
    monkeypatch.delenv("CIRCUITRY_ADAPTER_URL", raising=False)

    explicit = tmp_path / "explicit.json"
    explicit.write_text(
        json.dumps({"default_model": "explicit-model"}), encoding="utf-8"
    )
    # Also create a project-local that should be ignored
    local = tmp_path / "circuitry.config.json"
    local.write_text(
        json.dumps({"default_model": "local-model"}), encoding="utf-8"
    )

    cfg = resolve_config(explicit_path=explicit, cwd=tmp_path)
    assert cfg.default_model == "explicit-model"


# ---------------------------------------------------------------------------
# resolve_config — env vars override everything
# ---------------------------------------------------------------------------


def test_resolve_config_env_vars_override_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.delenv("CIRCUITRY_CONFIG", raising=False)
    monkeypatch.setenv("CIRCUITRY_MODEL", "env-model")
    monkeypatch.delenv("CIRCUITRY_ADAPTER", raising=False)
    monkeypatch.delenv("CIRCUITRY_ADAPTER_URL", raising=False)

    local = tmp_path / "circuitry.config.json"
    local.write_text(
        json.dumps({"default_model": "file-model"}), encoding="utf-8"
    )
    fake_global = tmp_path / "no-global" / "config.json"
    with patch("circuitry.cli.config.GLOBAL_CONFIG_PATH", fake_global):
        cfg = resolve_config(cwd=tmp_path)

    assert cfg.default_model == "env-model"


# ---------------------------------------------------------------------------
# resolve_config — CIRCUITRY_CONFIG env var
# ---------------------------------------------------------------------------


def test_resolve_config_circuitry_config_env(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.delenv("CIRCUITRY_MODEL", raising=False)
    monkeypatch.delenv("CIRCUITRY_ADAPTER", raising=False)
    monkeypatch.delenv("CIRCUITRY_ADAPTER_URL", raising=False)

    env_cfg = tmp_path / "env-config.json"
    env_cfg.write_text(
        json.dumps({"default_model": "from-env-path"}), encoding="utf-8"
    )
    monkeypatch.setenv("CIRCUITRY_CONFIG", str(env_cfg))

    fake_global = tmp_path / "no-global" / "config.json"
    with patch("circuitry.cli.config.GLOBAL_CONFIG_PATH", fake_global):
        cfg = resolve_config(cwd=tmp_path)

    assert cfg.default_model == "from-env-path"
