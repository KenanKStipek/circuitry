"""A model-mode `if`/`while` with no separate `prompt` effect gets the real
adapter, not the no-op one (#254).

Model mode is the compiler's default for an `if`/`while` condition. Before
this fix, `run()`'s `_has_prompt_effects` only counted `PromptDefinition`
instances, so a tool-only-looking document whose only model use was a
model-mode condition got the no-op adapter and crashed the moment the
condition tried to call `generate()`, with an error naming the wrong cause
(`"Attempted to call generate() on a tool-only orchestration."`).
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run


def _write(path: Path, content: str) -> Path:
    path.write_text(content, encoding="utf-8")
    return path


@dataclass(frozen=True)
class YesAdapter:
    name: str = "stub"

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        return GenerateResult(text="yes", raw={"prompt": prompt})


def test_model_mode_if_with_no_separate_prompt_effect_runs_without_crashing(
    tmp_path: Path,
) -> None:
    path = _write(
        tmp_path / "c.yml",
        "effects:\n"
        "  - {type: tool, name: n, provider: json, params: {input: '3'}}\n"
        "  - type: if\n"
        "    name: big\n"
        "    if: {mode: model, template: \"Is {{prime.n.value}} greater than 2?\"}\n"
        "    then:\n"
        "      - {type: tool, name: yes_branch, provider: json, params: {mode: stringify, input: 'yes'}}\n",
    )
    result = run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
            skip_preflight=True,
            adapter=YesAdapter(),
        )
    )
    assert result.ok is True, result.error
    assert result.state["prime"]["big"]["value"]["branch"] == "then"
    assert "yes_branch" in result.state["prime"]["big"]


def test_model_mode_while_with_no_separate_prompt_effect_runs_without_crashing(
    tmp_path: Path,
) -> None:
    path = _write(
        tmp_path / "c.yml",
        "effects:\n"
        "  - type: loop\n"
        "    name: poll\n"
        "    while: {mode: model, template: 'Keep going?'}\n"
        "    max_iterations: 1\n"
        "    body:\n"
        "      - {type: tool, name: t, provider: json, params: {mode: stringify, input: 'tick'}}\n",
    )
    result = run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
            skip_preflight=True,
            adapter=YesAdapter(),
        )
    )
    assert result.ok is True, result.error
