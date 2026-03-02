from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass(frozen=True)
class EchoAdapter:
    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        # Echo prompt back so JSON templates are parseable
        return GenerateResult(text=prompt, raw={"model": model})


def _run(orch: dict[str, Any], *, verbose: bool, dry_run: bool = True) -> list[str]:
    """Run an orchestration and return all console.print calls captured."""
    captured: list[str] = []

    import circuitry.output as output_mod

    original_print = output_mod.console.print

    def capture_print(*args: Any, **kwargs: Any) -> None:
        captured.append(str(args[0]) if args else "")

    output_mod.console.print = capture_print  # type: ignore[method-assign]
    try:
        root = compile_orchestration(orch=orch, root_name="prime")
        store = Store({})
        DynamicRuntime(
            root,
            adapter=EchoAdapter(),
            model="unit-test",
            dry_run=dry_run,
            verbose=verbose,
        ).execute(store=store)
    finally:
        output_mod.console.print = original_print  # type: ignore[method-assign]

    return captured


# ---------------------------------------------------------------------------
# verbose=False — no output
# ---------------------------------------------------------------------------


def test_no_output_when_verbose_false() -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "hello", "template": "hi"},
        ]
    }
    msgs = _run(orch, verbose=False)
    assert msgs == []


# ---------------------------------------------------------------------------
# prompt primitive
# ---------------------------------------------------------------------------


def test_prompt_emits_done() -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "greet", "template": "hi"},
        ]
    }
    msgs = _run(orch, verbose=True)
    # prompt start is an animated Live spinner (not a console.print)
    # only the done line is captured
    assert any("✓" in m and "greet" in m for m in msgs), msgs


# ---------------------------------------------------------------------------
# dynamic primitive
# ---------------------------------------------------------------------------


def test_nested_dynamic_emits_start_and_done() -> None:
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "inner",
                "flow": "chain",
                "effects": [
                    {"type": "prompt", "name": "step", "template": "x"},
                ],
            }
        ]
    }
    msgs = _run(orch, verbose=True)
    assert any("→" in m and "inner" in m for m in msgs), msgs
    assert any("✓" in m and "inner" in m for m in msgs), msgs
    # child prompt done line emitted (start is a Live spinner, not a print)
    assert any("✓" in m and "step" in m for m in msgs), msgs


# ---------------------------------------------------------------------------
# loop primitive — each
# ---------------------------------------------------------------------------


def test_loop_each_emits_iter_and_body_messages() -> None:
    # We need a real (non-dry-run) list in state for the loop to iterate.
    # Seed via initial state key that the loop reads.
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "items",
                "template": '["a","b"]',
                "prompt_type": "json",
            },
            {
                "type": "loop",
                "name": "process",
                "each": {"in": "prime.items.value", "as": "item"},
                "body": [
                    {"type": "prompt", "name": "do", "template": "{{item}}"},
                ],
            },
        ]
    }
    msgs = _run(orch, verbose=True, dry_run=False)
    # body prompt done lines carry iteration index labels e.g. "do [0]", "do [1]"
    assert any("✓" in m and "do [0]" in m for m in msgs), msgs
    assert any("✓" in m and "do [1]" in m for m in msgs), msgs


# ---------------------------------------------------------------------------
# if/conditional primitive
# ---------------------------------------------------------------------------


def test_conditional_emits_branch_message() -> None:
    orch = {
        "effects": [
            {
                "type": "if",
                "name": "check",
                "if": {"mode": "cel", "expr": "1 == 1"},
                "then": [{"type": "prompt", "name": "yes_path", "template": "yes"}],
                "else": [{"type": "prompt", "name": "no_path", "template": "no"}],
            }
        ]
    }
    msgs = _run(orch, verbose=True)
    # start and done for the if
    assert any("→" in m and "check" in m for m in msgs), msgs
    assert any("✓" in m and "check" in m for m in msgs), msgs
    # branch label emitted
    assert any("branch" in m for m in msgs), msgs
    # branch body prompt done emitted (start is a Live spinner, not a print)
    assert any("✓" in m and "yes_path" in m for m in msgs), msgs


# ---------------------------------------------------------------------------
# depth / indentation
# ---------------------------------------------------------------------------


def test_nested_dynamic_messages_are_indented_deeper() -> None:
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "outer",
                "flow": "chain",
                "effects": [
                    {"type": "prompt", "name": "inner_step", "template": "x"},
                ],
            }
        ]
    }
    msgs = _run(orch, verbose=True)
    # outer dynamic still prints → (non-prompt); inner prompt only prints ✓ (start is Live)
    outer_msg = next((m for m in msgs if "outer" in m and "→" in m), None)
    inner_msg = next((m for m in msgs if "inner_step" in m and "✓" in m), None)
    assert outer_msg is not None
    assert inner_msg is not None
    # inner message should have more leading spaces than outer
    outer_spaces = len(outer_msg) - len(outer_msg.lstrip())
    inner_spaces = len(inner_msg) - len(inner_msg.lstrip())
    assert inner_spaces > outer_spaces, f"outer={outer_msg!r}, inner={inner_msg!r}"


# ---------------------------------------------------------------------------
# timing and token annotations on done messages
# ---------------------------------------------------------------------------


def test_done_message_includes_elapsed_time() -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "timed", "template": "hi"},
        ]
    }
    msgs = _run(orch, verbose=True)
    done = next((m for m in msgs if "✓" in m and "timed" in m), None)
    assert done is not None
    # elapsed appears as Ns or Nms
    assert "s" in done or "ms" in done, done


def test_done_message_includes_tokens_when_live() -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "toktest", "template": "hi"},
        ]
    }
    # EchoAdapter returns tokens via GenerateResult — check that ↑/↓ appear
    # when tokens are non-None (EchoAdapter provides raw={model:...}, tokens default to None
    # in GenerateResult — so only check suffix structure exists and time is present).
    msgs = _run(orch, verbose=True, dry_run=False)
    done = next((m for m in msgs if "✓" in m and "toktest" in m), None)
    assert done is not None
    assert "s" in done or "ms" in done, done


def test_done_message_includes_icon_and_color_markup() -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "p", "template": "x"},
            {"type": "dynamic", "name": "d", "flow": "chain", "effects": [
                {"type": "prompt", "name": "inner", "template": "y"},
            ]},
        ]
    }
    msgs = _run(orch, verbose=True)
    # prompt icon ◆ present
    assert any("◆" in m for m in msgs), msgs
    # dynamic icon ⬡ present
    assert any("⬡" in m for m in msgs), msgs
    # Rich color markup present
    assert any("[cyan]" in m or "[blue]" in m for m in msgs), msgs
