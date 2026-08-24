"""`--no-routing` composing with a profile's per-effect `routing` pin.

#148 documented a profile `effects.<path>.routing` pin as resolving
independent of the config-level `routing.enabled` switch — it names a band
directly, so it applies whether or not `routing` is switched on elsewhere
(see `docs/profiles.md#per-effect-routing-control`). A run-level
`--no-routing`, by contrast, is a deliberate "routing is off for this run,
full stop": issue #110 asks for it to beat a profile pin rather than let one
pinned effect quietly keep routing on. `--routing` (forcing it *on*) has no
special interaction — pins and opt-outs already apply the same regardless.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run


def _write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


@dataclass(frozen=True)
class RecordingAdapter:
    name: str = "primary"
    calls: list = field(default_factory=list)

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        self.calls.append((model, prompt))
        return GenerateResult(text=f"{model}:{prompt}", raw={})


def _orch_and_profile(tmp_path: Path) -> tuple[Path, str]:
    orch_path = tmp_path / "pinned.yml"
    _write(
        orch_path,
        """
model: default-model
effects:
  - type: prompt
    name: greet
    template: "hello"
""".strip()
        + "\n",
    )
    _write(
        orch_path.parent / "profiles" / "pin-top.yml",
        """
effects:
  greet:
    routing: top
""".strip()
        + "\n",
    )
    return orch_path, "pin-top"


def _cfg_with_bands() -> CircuitryConfig:
    return CircuitryConfig(
        runtime={
            "complexity": {
                "scoring": {"enabled": True},
                "routing": {
                    "enabled": True,
                    "bands": [
                        {"name": "cheap", "max": 40, "model": "cheap-model"},
                        {"name": "top", "model": "top-model"},
                    ],
                },
            }
        }
    )


def test_profile_routing_pin_applies_when_routing_is_not_overridden(
    tmp_path: Path,
) -> None:
    orch_path, profile_name = _orch_and_profile(tmp_path)

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=_cfg_with_bands(),
            adapter=RecordingAdapter(),
            profile_name=profile_name,
        )
    )

    assert result.ok is True, result.error
    meta = result.state["prime"]["greet"]["meta"]
    assert meta["model"] == "top-model"
    assert meta["model_reason"] == "router"


def test_no_routing_flag_beats_the_profile_pin(tmp_path: Path) -> None:
    """The composition issue #110 calls out: --no-routing outranks the pin."""
    orch_path, profile_name = _orch_and_profile(tmp_path)

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=_cfg_with_bands(),
            adapter=RecordingAdapter(),
            profile_name=profile_name,
            routing_override=False,
        )
    )

    assert result.ok is True, result.error
    meta = result.state["prime"]["greet"]["meta"]
    # The pin never reaches the compiled definition: greet dispatches on the
    # run default, not the "top" band it was pinned to.
    assert meta["model"] == "default-model"
    assert meta["model_reason"] == "default"
    assert result.state["runtime"]["effective_settings"]["sources"]["complexity.routing"] == "cli"


def test_routing_flag_on_does_not_disturb_the_pin(tmp_path: Path) -> None:
    """--routing (forcing it *on*) is a no-op for a pin that already applies."""
    orch_path, profile_name = _orch_and_profile(tmp_path)
    cfg = CircuitryConfig(
        runtime={
            "complexity": {
                "scoring": {"enabled": True},
                "routing": {
                    "enabled": False,
                    "bands": [
                        {"name": "cheap", "max": 40, "model": "cheap-model"},
                        {"name": "top", "model": "top-model"},
                    ],
                },
            }
        }
    )

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=cfg,
            adapter=RecordingAdapter(),
            profile_name=profile_name,
            routing_override=True,
        )
    )

    assert result.ok is True, result.error
    meta = result.state["prime"]["greet"]["meta"]
    assert meta["model"] == "top-model"
    assert meta["model_reason"] == "router"
