"""Profile per-effect routing control (issue #109): opt-out and band pins.

Compiler-level overlay mechanics and the dispatch precedence chain
(explicit > pin > router) live in ``tests/core/test_prompt_routing.py`` and
``tests/core/test_profile_effect_overlay.py``; this file covers the two
things that only exist above the runtime: the profile schema accepting the
new ``routing`` key (and still rejecting nonsense), and
``circuitry.cli.runtime_shim.run`` validating a pinned band name against the
run's actual routing table before anything dispatches.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.cli.complexity_config import RoutingSettings, parse_complexity_settings
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.profiles import (
    ProfileValidationError,
    load_profile,
    validate_profile_routing_pins,
)
from circuitry.cli.runtime_shim import RunRequest, run


def _write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def _write_orch(tmp_path: Path, *, routing: dict[str, Any] | None = None) -> Path:
    orch: dict[str, Any] = {
        "model": "orch-model",
        "effects": [
            {"type": "prompt", "name": "task", "template": "go"},
        ],
    }
    if routing is not None:
        orch["runtime"] = {"complexity": {"scoring": {"enabled": True}, "routing": routing}}
    path = tmp_path / "recipe.yml"
    import yaml

    _write(path, yaml.safe_dump(orch, sort_keys=False))
    return path


@dataclass
class RecordingAdapter:
    name: str = "recording"
    calls: list[str] = field(default_factory=list)

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        self.calls.append(model)
        return GenerateResult(text=f"ok:{model}", raw={})


BANDS_BLOCK = {
    "enabled": True,
    "bands": [
        {"name": "cheap", "max": 1.0, "model": "cheap-model"},
        {"name": "premium", "model": "premium-model"},
    ],
}


# --------------------------------------------------------------------------
# profile schema: the new `routing` key
# --------------------------------------------------------------------------


def test_profile_schema_accepts_routing_opt_out_and_pin(tmp_path: Path) -> None:
    orch_path = _write_orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "mixed.yml",
        "effects:\n"
        "  task:\n"
        "    routing: premium\n",
    )
    profile = load_profile(
        name="mixed",
        orchestration_path=orch_path,
        orch={"effects": [{"type": "prompt", "name": "task", "template": "go"}]},
    )
    assert profile.effects["task"] == {"routing": "premium"}


def test_profile_schema_accepts_routing_false(tmp_path: Path) -> None:
    orch_path = _write_orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "off.yml",
        "effects:\n  task:\n    routing: false\n",
    )
    profile = load_profile(
        name="off",
        orchestration_path=orch_path,
        orch={"effects": [{"type": "prompt", "name": "task", "template": "go"}]},
    )
    assert profile.effects["task"] == {"routing": False}


@pytest.mark.parametrize(
    "routing_yaml",
    [
        pytest.param("routing: true\n", id="true-not-allowed"),
        pytest.param("routing: ''\n", id="empty-string-not-allowed"),
        pytest.param("routing: 3\n", id="number-not-allowed"),
    ],
)
def test_profile_schema_rejects_invalid_routing_values(
    tmp_path: Path, routing_yaml: str
) -> None:
    orch_path = _write_orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "bad.yml",
        f"effects:\n  task:\n    {routing_yaml}",
    )
    with pytest.raises(ProfileValidationError) as exc_info:
        load_profile(
            name="bad",
            orchestration_path=orch_path,
            orch={"effects": [{"type": "prompt", "name": "task", "template": "go"}]},
        )
    assert "schema validation" in str(exc_info.value)


def test_profile_schema_rejects_unknown_effect_key_alongside_routing(
    tmp_path: Path,
) -> None:
    """A near-miss of the new key still fails schema validation — same rule
    as any other unknown per-effect key."""
    orch_path = _write_orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "typo.yml",
        "effects:\n  task:\n    routng: premium\n",
    )
    with pytest.raises(ProfileValidationError) as exc_info:
        load_profile(
            name="typo",
            orchestration_path=orch_path,
            orch={"effects": [{"type": "prompt", "name": "task", "template": "go"}]},
        )
    assert "schema validation" in str(exc_info.value)


def test_profile_rejects_routing_pin_on_unknown_effect_path(tmp_path: Path) -> None:
    orch_path = _write_orch(tmp_path)
    _write(
        orch_path.parent / "profiles" / "bad-path.yml",
        "effects:\n  does_not_exist:\n    routing: premium\n",
    )
    with pytest.raises(ProfileValidationError) as exc_info:
        load_profile(
            name="bad-path",
            orchestration_path=orch_path,
            orch={"effects": [{"type": "prompt", "name": "task", "template": "go"}]},
        )
    assert "does_not_exist" in str(exc_info.value)


# --------------------------------------------------------------------------
# eager band-name validation (`validate_profile_routing_pins`)
# --------------------------------------------------------------------------


def _routing(bands: list[dict[str, Any]]) -> RoutingSettings:
    return parse_complexity_settings(
        {"scoring": {"enabled": True}, "routing": {"enabled": True, "bands": bands}}
    ).routing


def test_validate_profile_routing_pins_accepts_a_known_band() -> None:
    validate_profile_routing_pins(
        {"task": {"routing": "premium"}},
        routing=_routing(BANDS_BLOCK["bands"]),
        profile_name="p",
    )  # does not raise


def test_validate_profile_routing_pins_ignores_boolean_opt_outs() -> None:
    """`routing: false` has no name to check — never a validation target."""
    validate_profile_routing_pins(
        {"task": {"routing": False}},
        routing=_routing(BANDS_BLOCK["bands"]),
        profile_name="p",
    )  # does not raise


def test_validate_profile_routing_pins_rejects_unknown_band_name() -> None:
    with pytest.raises(ProfileValidationError) as exc_info:
        validate_profile_routing_pins(
            {"task": {"routing": "no-such-band"}},
            routing=_routing(BANDS_BLOCK["bands"]),
            profile_name="p",
        )
    message = str(exc_info.value)
    assert "no-such-band" in message
    assert "premium" in message
    assert "cheap" in message


# --------------------------------------------------------------------------
# end-to-end via runtime_shim.run
# --------------------------------------------------------------------------


def test_opt_out_honored_end_to_end(tmp_path: Path) -> None:
    orch_path = _write_orch(tmp_path, routing={"enabled": True, "bands": [{"name": "everything", "model": "routed-model"}]})
    _write(orch_path.parent / "profiles" / "trivial.yml", "effects:\n  task:\n    routing: false\n")

    adapter = RecordingAdapter()
    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            adapter=adapter,
            skip_preflight=True,
            config=CircuitryConfig(),
            profile_name="trivial",
        )
    )
    assert result.ok, result.error
    assert adapter.calls == ["orch-model"]
    assert result.state["prime"]["task"]["meta"]["model_reason"] == "default"


def test_pinned_band_honored_end_to_end(tmp_path: Path) -> None:
    orch_path = _write_orch(tmp_path, routing=BANDS_BLOCK)
    _write(orch_path.parent / "profiles" / "expensive.yml", "effects:\n  task:\n    routing: premium\n")

    adapter = RecordingAdapter()
    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            adapter=adapter,
            skip_preflight=True,
            config=CircuitryConfig(),
            profile_name="expensive",
        )
    )
    assert result.ok, result.error
    assert adapter.calls == ["premium-model"]
    meta = result.state["prime"]["task"]["meta"]
    assert meta["model_reason"] == "router"
    assert meta["complexity"]["band"] == {"name": "premium", "model": "premium-model"}


def test_bad_band_pin_fails_the_run_before_any_dispatch(tmp_path: Path) -> None:
    orch_path = _write_orch(tmp_path, routing=BANDS_BLOCK)
    _write(
        orch_path.parent / "profiles" / "typo-band.yml",
        "effects:\n  task:\n    routing: premiumm\n",
    )

    adapter = RecordingAdapter()
    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            adapter=adapter,
            skip_preflight=True,
            config=CircuitryConfig(),
            profile_name="typo-band",
        )
    )
    assert not result.ok
    assert "premiumm" in (result.error or "")
    assert adapter.calls == []


def test_profile_level_model_override_beats_an_effect_routing_pin(
    tmp_path: Path,
) -> None:
    """Interaction with a profile-level `model:` override: the run-level
    default it pins is `model_locked`, which is 'explicit' the same way a
    per-effect `model:` is — it beats a `routing` pin on that effect too."""
    orch_path = _write_orch(tmp_path, routing=BANDS_BLOCK)
    _write(
        orch_path.parent / "profiles" / "locked.yml",
        "model: profile-level-model\neffects:\n  task:\n    routing: premium\n",
    )

    adapter = RecordingAdapter()
    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            adapter=adapter,
            skip_preflight=True,
            config=CircuitryConfig(),
            profile_name="locked",
        )
    )
    assert result.ok, result.error
    assert adapter.calls == ["profile-level-model"]
    assert result.state["prime"]["task"]["meta"]["model_reason"] == "default"
