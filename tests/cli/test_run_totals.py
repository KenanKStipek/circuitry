"""``state.runtime.last_run.totals`` (#271): wall time, effects run, tokens
in/out over every attempt, recorded once at the end of a run.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run


@dataclass
class _FakeAdapter:
    name: str = "fake"
    tokens_sent: int = 7
    tokens_received: int = 3
    calls: list[str] = field(default_factory=list)

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120, options=None) -> GenerateResult:
        self.calls.append(prompt)
        return GenerateResult(
            text=f"echo:{prompt}",
            raw={},
            tokens_sent=self.tokens_sent,
            tokens_received=self.tokens_received,
        )


def _write(tmp_path: Path, content: str) -> Path:
    p = tmp_path / "orch.yml"
    p.write_text(content, encoding="utf-8")
    return p


def test_totals_sum_tokens_and_count_effects(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "effects:\n"
        "  - {type: prompt, name: greet, template: 'hi'}\n"
        "  - {type: prompt, name: farewell, template: 'bye'}\n",
    )
    adapter = _FakeAdapter()
    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            config=CircuitryConfig(default_adapter="fake", default_model="test"),
            skip_preflight=True,
        )
    )
    assert result.ok is True

    totals = result.state["runtime"]["last_run"]["totals"]
    assert totals["tokens_sent"] == 14  # 7 + 7
    assert totals["tokens_received"] == 6  # 3 + 3
    assert totals["effects_run"] == 3  # prime (the root dynamic) + 2 prompts
    assert isinstance(totals["wall_time_s"], float)
    assert totals["wall_time_s"] >= 0.0
    assert totals["cost_usd"] is None  # no adapter reports cost yet


def test_totals_prefer_per_attempt_total_token_fields(tmp_path: Path) -> None:
    """A prompt that retried/fell back records ``tokens_sent_total`` (#313)
    covering every attempt; totals sum that, not just the last attempt's
    ``tokens_sent``."""
    orch = _write(
        tmp_path,
        "effects:\n"
        "  - {type: prompt, name: greet, template: 'hi'}\n",
    )
    adapter = _FakeAdapter(tokens_sent=5, tokens_received=2)
    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            config=CircuitryConfig(default_adapter="fake", default_model="test"),
            skip_preflight=True,
        )
    )
    assert result.ok is True
    # Simulate what a retried/fallback prompt's meta carries: the *_total
    # fields cover 3 attempts worth, bigger than the lone tokens_sent/
    # tokens_received this fake adapter actually reported.
    result.state["prime"]["greet"]["meta"]["tokens_sent_total"] = 15
    result.state["prime"]["greet"]["meta"]["tokens_received_total"] = 6

    from circuitry.cli.runtime_shim import _run_totals

    totals = _run_totals(result.state, wall_time_s=1.0)
    assert totals["tokens_sent"] == 15
    assert totals["tokens_received"] == 6


def test_totals_count_a_use_childs_effects_even_in_declared_outputs_mode(
    tmp_path: Path,
) -> None:
    """A ``use`` child with declared ``outputs:`` (no ``record_children``)
    never leaves its own effects in the final state — only the mapped
    output value does — but every one of its effects still ran and still
    fired ``effect_complete`` namespaced under the parent (#331 finding 4).
    Totals accumulated from that stream see them; a post-hoc walk of the
    finished state tree cannot.
    """
    child = tmp_path / "child.yml"
    child.write_text(
        "effects:\n"
        "  - {type: prompt, name: inner_a, template: 'a'}\n"
        "  - {type: prompt, name: inner_b, template: 'b'}\n",
        encoding="utf-8",
    )
    orch = _write(
        tmp_path,
        "effects:\n"
        "  - {type: prompt, name: greet, template: 'hi'}\n"
        f"  - {{type: use, name: sub, path: {child}, outputs: {{b: prime.inner_b.value}}}}\n",
    )
    adapter = _FakeAdapter()
    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            config=CircuitryConfig(default_adapter="fake", default_model="test"),
            skip_preflight=True,
        )
    )
    assert result.ok is True

    # Declared-outputs mode: the child's own effects never land in the
    # final state — only the mapped `b` output does.
    assert "inner_a" not in result.state["prime"]["sub"]
    assert "inner_b" not in result.state["prime"]["sub"]

    totals = result.state["runtime"]["last_run"]["totals"]
    # prime + greet + sub + inner_a + inner_b = 5, not 3 (which is what a
    # state-walk would see, since inner_a/inner_b never land in state).
    assert totals["effects_run"] == 5
    # greet + inner_a + inner_b each made one call
    assert totals["tokens_sent"] == 7 * 3
    assert totals["tokens_received"] == 3 * 3


def test_totals_recorded_on_failure_too(tmp_path: Path) -> None:
    """A run that raises mid-execution still gets ``totals`` — counting
    whatever actually landed, not just a run that reached the end."""
    orch = _write(
        tmp_path,
        "effects:\n"
        "  - {type: prompt, name: greet, template: 'hi'}\n"
        "  - {type: prompt, name: boom, template: 'bye'}\n",
    )

    @dataclass
    class _FailsOnSecondCall:
        name: str = "fake"
        calls: int = 0

        def generate(self, *, model, prompt, timeout_seconds=120, options=None):
            self.calls += 1
            if self.calls == 2:
                raise RuntimeError("boom")
            return GenerateResult(text="ok", raw={}, tokens_sent=1, tokens_received=1)

    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=_FailsOnSecondCall(),
            config=CircuitryConfig(default_adapter="fake", default_model="test"),
            skip_preflight=True,
        )
    )
    assert result.ok is False
    totals = result.state["runtime"]["last_run"]["totals"]
    assert totals["effects_run"] >= 1  # `greet` landed before `boom` failed
    assert isinstance(totals["wall_time_s"], float)


def test_totals_are_already_written_when_on_run_failure_plugins_see_the_state(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """``on_run_success`` plugins could already read ``last_run.totals``;
    an ``on_run_failure`` plugin could not, because totals used to be
    written only *after* that hook ran (#331 finding 11).
    """
    import circuitry.cli.runtime_shim as runtime_shim

    orch = _write(
        tmp_path,
        "effects:\n"
        "  - {type: prompt, name: greet, template: 'hi'}\n"
        "  - {type: prompt, name: boom, template: 'bye'}\n",
    )

    @dataclass
    class _FailsOnSecondCall:
        name: str = "fake"
        calls: int = 0

        def generate(self, *, model, prompt, timeout_seconds=120, options=None):
            self.calls += 1
            if self.calls == 2:
                raise RuntimeError("boom")
            return GenerateResult(text="ok", raw={}, tokens_sent=1, tokens_received=1)

    seen_totals: list[Any] = []
    original_invoke_plugins = runtime_shim.invoke_plugins

    def _spy_invoke_plugins(*, hook_name, state, **kwargs):
        if hook_name == "on_run_failure":
            seen_totals.append(
                state.get("runtime", {}).get("last_run", {}).get("totals")
            )
        return original_invoke_plugins(hook_name=hook_name, state=state, **kwargs)

    monkeypatch.setattr(runtime_shim, "invoke_plugins", _spy_invoke_plugins)

    result = run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=_FailsOnSecondCall(),
            config=CircuitryConfig(default_adapter="fake", default_model="test"),
            skip_preflight=True,
        )
    )
    assert result.ok is False
    assert len(seen_totals) == 1
    assert seen_totals[0] is not None
    assert seen_totals[0]["effects_run"] >= 1
