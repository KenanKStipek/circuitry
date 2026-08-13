from __future__ import annotations

from pathlib import Path

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.effective_settings import resolve_effective_settings
from circuitry.cli.profiles import ProfileSettings
from circuitry.cli.runtime_shim import RunRequest, run


def _profile(**overrides: object) -> ProfileSettings:
    defaults: dict[str, object] = {
        "name": "fast",
        "path": Path("profiles/fast.yml"),
        "adapter": None,
        "model": None,
        "inputs": {},
        "effects": {},
        "persistence": None,
        "raw": {},
    }
    defaults.update(overrides)
    return ProfileSettings(**defaults)  # type: ignore[arg-type]


def _write(tmp_path: Path, name: str, content: str) -> Path:
    path = tmp_path / name
    path.write_text(content, encoding="utf-8")
    return path


def test_effective_settings_precedence_cli_over_orch_over_config() -> None:
    cfg = CircuitryConfig(
        default_model="cfg-model",
        default_adapter="openai",
        plugins=["cfg.plugin"],
        runtime={"adapters": {"openai": {"base_url": "https://cfg.example"}}},
    )
    orch = {
        "model": "orch-model",
        "adapter": "anthropic",
        "plugins": ["orch.plugin"],
        "runtime": {"adapters": {"anthropic": {"max_tokens": 1024}}},
    }

    effective = resolve_effective_settings(
        cfg=cfg,
        orch=orch,
        cli_model="cli-model",
        cli_adapter="litellm",
        cli_plugins=["cli.plugin"],
    )

    assert effective.model == "cli-model"
    assert effective.adapter == "litellm"
    assert effective.plugins == ["cli.plugin"]
    assert effective.sources["model"] == "cli"
    assert effective.sources["adapter"] == "cli"
    assert effective.sources["plugins"] == "cli"
    assert effective.sources["runtime"] == "orchestration"
    assert effective.runtime["adapters"]["anthropic"]["max_tokens"] == 1024


def test_effective_settings_falls_back_to_orchestration_then_config() -> None:
    cfg = CircuitryConfig(default_model="cfg-model", default_adapter="openai")
    orch = {"model": "orch-model", "adapter": "anthropic"}

    effective = resolve_effective_settings(cfg=cfg, orch=orch)
    assert effective.model == "orch-model"
    assert effective.adapter == "anthropic"
    assert effective.sources["model"] == "orchestration"
    assert effective.sources["adapter"] == "orchestration"

    effective_cfg_only = resolve_effective_settings(cfg=cfg, orch={})
    assert effective_cfg_only.model == "cfg-model"
    assert effective_cfg_only.adapter == "openai"
    assert effective_cfg_only.sources["model"] == "config"
    assert effective_cfg_only.sources["adapter"] == "config"


def test_effective_settings_precedence_cli_over_profile_over_orch_over_config() -> None:
    cfg = CircuitryConfig(default_model="cfg-model", default_adapter="openai")
    orch = {"model": "orch-model", "adapter": "anthropic"}
    profile = _profile(model="profile-model", adapter="ollama")

    # profile wins over orchestration and config
    effective = resolve_effective_settings(cfg=cfg, orch=orch, profile=profile)
    assert effective.model == "profile-model"
    assert effective.adapter == "ollama"
    assert effective.sources["model"] == "profile"
    assert effective.sources["adapter"] == "profile"

    # cli still wins over profile
    effective_with_cli = resolve_effective_settings(
        cfg=cfg, orch=orch, profile=profile, cli_model="cli-model", cli_adapter="litellm"
    )
    assert effective_with_cli.model == "cli-model"
    assert effective_with_cli.adapter == "litellm"
    assert effective_with_cli.sources["model"] == "cli"
    assert effective_with_cli.sources["adapter"] == "cli"

    # profile falls back to orchestration when it doesn't set a field
    effective_partial = resolve_effective_settings(
        cfg=cfg, orch=orch, profile=_profile(model=None, adapter=None)
    )
    assert effective_partial.model == "orch-model"
    assert effective_partial.adapter == "anthropic"
    assert effective_partial.sources["model"] == "orchestration"
    assert effective_partial.sources["adapter"] == "orchestration"

    # profile falls back to config when neither profile nor orchestration set a field
    effective_cfg_fallback = resolve_effective_settings(
        cfg=cfg, orch={}, profile=_profile(model=None, adapter=None)
    )
    assert effective_cfg_fallback.model == "cfg-model"
    assert effective_cfg_fallback.adapter == "openai"
    assert effective_cfg_fallback.sources["model"] == "config"
    assert effective_cfg_fallback.sources["adapter"] == "config"


def test_no_profile_resolution_is_unaffected_by_the_profile_parameter() -> None:
    """Regression guard: omitting `profile` must behave identically to before."""
    cfg = CircuitryConfig(default_model="cfg-model", default_adapter="openai")
    orch = {"model": "orch-model", "adapter": "anthropic"}

    without_kwarg = resolve_effective_settings(cfg=cfg, orch=orch)
    with_none = resolve_effective_settings(cfg=cfg, orch=orch, profile=None)
    assert without_kwarg == with_none


def test_run_returns_actionable_error_for_missing_adapter_and_model(
    tmp_path: Path,
) -> None:
    orch = _write(
        tmp_path,
        "missing.yml",
        """
effects:
  - type: prompt
    name: greet
    template: \"hello\"
""".strip()
        + "\n",
    )

    req = RunRequest(
        orchestration_path=orch,
        state_path=None,
        out_path=None,
        dry_run=True,
        validate_only=False,
        config=CircuitryConfig(),
    )
    result = run(req)

    assert result.ok is False
    assert isinstance(result.error, str)
    assert "No adapter resolved for orchestration" in result.error
    assert "default_adapter" in result.error


def test_run_returns_actionable_error_for_unknown_adapter(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "unknown.yml",
        """
adapter: not-real
model: gpt-4o-mini
effects:
  - type: prompt
    name: greet
    template: \"hello\"
""".strip()
        + "\n",
    )

    req = RunRequest(
        orchestration_path=orch,
        state_path=None,
        out_path=None,
        dry_run=True,
        validate_only=False,
        config=CircuitryConfig(),
    )
    result = run(req)

    assert result.ok is False
    assert isinstance(result.error, str)
    assert "Unknown adapter" in result.error
    assert "Supported adapters:" in result.error
    assert "runtime.adapters" in result.error


def test_compile_error_takes_precedence_over_missing_runtime_settings(
    tmp_path: Path,
) -> None:
    orch = _write(
        tmp_path,
        "invalid.yml",
        """
effects:
  - type: prompt
    name: dup
    template: "a"
  - type: prompt
    name: dup
    template: "b"
""".strip()
        + "\n",
    )

    req = RunRequest(
        orchestration_path=orch,
        state_path=None,
        out_path=None,
        dry_run=True,
        validate_only=False,
        config=CircuitryConfig(),
    )
    result = run(req)

    assert result.ok is False
    assert isinstance(result.error, str)
    assert "Duplicate effect name 'dup'" in result.error
    assert "No adapter resolved for orchestration" not in result.error
