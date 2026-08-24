from __future__ import annotations

from pathlib import Path

import pytest

from circuitry.cli.complexity_config import ComplexityConfigError
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


def test_effective_settings_out_precedence_cli_over_profile_over_default() -> None:
    """`--out` has no orchestration/config layer — only cli > profile > default."""
    cfg = CircuitryConfig()
    orch: dict[str, object] = {}

    # neither cli nor profile: no file written
    assert resolve_effective_settings(cfg=cfg, orch=orch).out is None
    assert resolve_effective_settings(cfg=cfg, orch=orch).sources["out"] == "default"

    # profile alone supplies it
    profile = _profile(out="runs/profile-out.json")
    effective = resolve_effective_settings(cfg=cfg, orch=orch, profile=profile)
    assert effective.out == Path("runs/profile-out.json")
    assert effective.sources["out"] == "profile"

    # cli beats profile
    effective_with_cli = resolve_effective_settings(
        cfg=cfg, orch=orch, profile=profile, cli_out=Path("runs/cli-out.json")
    )
    assert effective_with_cli.out == Path("runs/cli-out.json")
    assert effective_with_cli.sources["out"] == "cli"

    # profile absent, no cli: still no file written
    effective_no_profile_out = resolve_effective_settings(
        cfg=cfg, orch=orch, profile=_profile(out=None)
    )
    assert effective_no_profile_out.out is None
    assert effective_no_profile_out.sources["out"] == "default"


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


# -- cli_scoring/cli_routing/cli_decompose (issue #110) ----------------------

_BANDS = [{"name": "cheap", "max": 40, "model": "small"}, {"model": "large"}]


def test_cli_complexity_flags_override_config_in_both_directions() -> None:
    cfg = CircuitryConfig(
        runtime={"complexity": {"scoring": {"enabled": True}}}
    )

    # Config says scoring on; --no-scoring forces it off for this run.
    off = resolve_effective_settings(cfg=cfg, orch={}, cli_scoring=False)
    assert off.complexity.scoring.enabled is False
    assert off.sources["complexity.scoring"] == "cli"

    # Config says nothing about routing (off by default); --routing forces it
    # on, reusing the bands the config already declared.
    cfg_with_bands = CircuitryConfig(
        runtime={
            "complexity": {
                "scoring": {"enabled": True},
                "routing": {"enabled": False, "bands": _BANDS},
            }
        }
    )
    on = resolve_effective_settings(cfg=cfg_with_bands, orch={}, cli_routing=True)
    assert on.complexity.routing.enabled is True
    assert on.complexity.routing.bands[0].model == "small"
    assert on.sources["complexity.routing"] == "cli"


def test_cli_complexity_flags_beat_orchestration_and_profile() -> None:
    cfg = CircuitryConfig()
    orch = {"runtime": {"complexity": {"scoring": {"enabled": True}}}}
    profile = _profile()

    effective = resolve_effective_settings(
        cfg=cfg, orch=orch, profile=profile, cli_scoring=False
    )
    assert effective.complexity.scoring.enabled is False
    assert effective.sources["complexity.scoring"] == "cli"


def test_cli_complexity_flag_untouched_switches_keep_their_own_layer() -> None:
    """A flag on one switch must not disturb the sources of the other two."""
    cfg = CircuitryConfig(
        runtime={"complexity": {"scoring": {"enabled": True}}}
    )
    orch = {
        "runtime": {
            "complexity": {
                "scoring": {"enabled": True},
                "routing": {"enabled": True, "bands": _BANDS},
            }
        }
    }

    # Turning routing off by flag leaves scoring's provenance untouched — it
    # is still satisfied by the orchestration layer, which the flag never
    # reached.
    effective = resolve_effective_settings(cfg=cfg, orch=orch, cli_routing=False)
    assert effective.complexity.routing.enabled is False
    assert effective.sources["complexity.routing"] == "cli"
    assert effective.complexity.scoring.enabled is True
    assert effective.sources["complexity.scoring"] == "orchestration"


def test_cli_routing_flag_without_scoring_raises_same_prerequisite_error() -> None:
    cfg = CircuitryConfig(
        runtime={"complexity": {"routing": {"enabled": False, "bands": _BANDS}}}
    )

    with pytest.raises(ComplexityConfigError) as excinfo:
        resolve_effective_settings(cfg=cfg, orch={}, cli_routing=True)

    message = str(excinfo.value)
    assert "runtime.complexity.routing.enabled is true" in message
    assert "runtime.complexity.scoring.enabled" in message


def test_cli_decompose_flag_without_scoring_raises_same_prerequisite_error() -> None:
    cfg = CircuitryConfig()

    with pytest.raises(ComplexityConfigError) as excinfo:
        resolve_effective_settings(cfg=cfg, orch={}, cli_decompose=True)

    message = str(excinfo.value)
    assert "runtime.complexity.decomposition.enabled is true" in message
    assert "runtime.complexity.scoring.enabled" in message


def test_no_complexity_flags_preserves_existing_precedence() -> None:
    """Regression guard: omitting the three flags must behave identically."""
    cfg = CircuitryConfig(runtime={"complexity": {"scoring": {"enabled": True}}})
    orch: dict[str, object] = {}

    without_kwargs = resolve_effective_settings(cfg=cfg, orch=orch)
    with_none = resolve_effective_settings(
        cfg=cfg, orch=orch, cli_scoring=None, cli_routing=None, cli_decompose=None
    )
    assert without_kwargs == with_none
