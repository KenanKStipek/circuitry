"""Issue #158: `runtime.adapters.<name>.timeout_seconds` is config-driven.

Two things are asserted end-to-end through :func:`circuitry.cli.runtime_shim.run`,
because that is where the config value is resolved and threaded into the
adapter call (see `_has_prompt_effects` handling in `run()`):

1. A configured value reaches `adapter.generate(timeout_seconds=...)`, and an
   unset one still falls back to the historical 120s default.
2. The resolution is recorded in `effective_settings.sources`, the same
   provenance mechanism every other config-driven setting (model, adapter,
   persistence, complexity...) already uses.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run


@dataclass
class RecordingAdapter:
    name: str = "ollama"
    calls: list[tuple[str, int]] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append((model, timeout_seconds))
        return GenerateResult(text="ok", raw={})


def _write_prompt_orch(tmp_path: Path) -> Path:
    orch: dict[str, Any] = {
        "model": "some-model",
        "effects": [
            {"type": "prompt", "name": "task", "template": "hello"},
        ],
    }
    path = tmp_path / "orch.yml"
    path.write_text(yaml.safe_dump(orch, sort_keys=False), encoding="utf-8")
    return path


def test_configured_timeout_reaches_adapter_generate(tmp_path: Path) -> None:
    adapter = RecordingAdapter()
    cfg = CircuitryConfig(
        default_adapter="ollama",
        runtime={"adapters": {"ollama": {"timeout_seconds": 300}}},
    )
    result = run(
        RunRequest(
            orchestration_path=_write_prompt_orch(tmp_path),
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            adapter=adapter,
            skip_preflight=True,
            config=cfg,
        )
    )
    assert result.ok, result.error
    assert adapter.calls == [("some-model", 300)]


def test_unconfigured_timeout_defaults_to_120(tmp_path: Path) -> None:
    adapter = RecordingAdapter()
    cfg = CircuitryConfig(default_adapter="ollama")
    result = run(
        RunRequest(
            orchestration_path=_write_prompt_orch(tmp_path),
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            adapter=adapter,
            skip_preflight=True,
            config=cfg,
        )
    )
    assert result.ok, result.error
    assert adapter.calls == [("some-model", 120)]


def test_effective_settings_records_configured_timeout_provenance(
    tmp_path: Path,
) -> None:
    cfg = CircuitryConfig(
        default_adapter="ollama",
        runtime={"adapters": {"ollama": {"timeout_seconds": 300}}},
    )
    result = run(
        RunRequest(
            orchestration_path=_write_prompt_orch(tmp_path),
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            adapter=RecordingAdapter(),
            skip_preflight=True,
            config=cfg,
        )
    )
    assert result.ok, result.error
    sources = result.state["runtime"]["effective_settings"]["sources"]
    assert sources["adapters.ollama.timeout_seconds"] == "config"


def test_effective_settings_records_default_timeout_provenance(
    tmp_path: Path,
) -> None:
    cfg = CircuitryConfig(default_adapter="ollama")
    result = run(
        RunRequest(
            orchestration_path=_write_prompt_orch(tmp_path),
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            adapter=RecordingAdapter(),
            skip_preflight=True,
            config=cfg,
        )
    )
    assert result.ok, result.error
    sources = result.state["runtime"]["effective_settings"]["sources"]
    assert sources["adapters.ollama.timeout_seconds"] == "default"
