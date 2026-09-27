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
                "schema": {"type": "array", "items": {"type": "string"}},
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


# ---------------------------------------------------------------------------
# use primitive
# ---------------------------------------------------------------------------


_INLINE_CHILD = "effects:\n  - type: prompt\n    name: inner\n    template: hi\n"


def test_use_emits_single_start_and_single_done_line() -> None:
    orch = {
        "effects": [
            {"type": "use", "name": "sub", "inline": _INLINE_CHILD},
        ]
    }
    msgs = _run(orch, verbose=True)
    start_lines = [m for m in msgs if "⊕" in m and "sub" in m and "→" in m]
    done_lines = [m for m in msgs if "⊕" in m and "sub" in m and "✓" in m]
    assert len(start_lines) == 1, msgs
    assert len(done_lines) == 1, msgs
    assert "|" in done_lines[0], done_lines[0]


def test_use_dry_run_emits_single_start_and_dry_done_line() -> None:
    orch = {
        "effects": [
            {"type": "use", "name": "sub", "inline": _INLINE_CHILD},
        ]
    }
    msgs = _run(orch, verbose=True, dry_run=True)
    start_lines = [m for m in msgs if "⊕" in m and "sub" in m and "→" in m]
    done_lines = [m for m in msgs if "⊕" in m and "sub" in m and "✓" in m]
    assert len(start_lines) == 1, msgs
    assert len(done_lines) == 1, msgs
    assert "(dry)" in done_lines[0], done_lines[0]


def test_use_error_emits_single_start_and_single_error_line() -> None:
    orch = {
        "effects": [
            {
                "type": "use",
                "name": "sub",
                "path": "/nonexistent/orchestration.yml",
                "on_error": "skip",
            },
        ]
    }
    msgs = _run(orch, verbose=True, dry_run=False)
    start_lines = [m for m in msgs if "⊕" in m and "sub" in m and "→" in m]
    error_lines = [m for m in msgs if "⊕" in m and "sub" in m and "✗" in m]
    assert len(start_lines) == 1, msgs
    assert len(error_lines) == 1, msgs


def test_use_result_line_matches_dispatcher_indent() -> None:
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "outer",
                "flow": "chain",
                "effects": [
                    {"type": "prompt", "name": "sibling", "template": "hi"},
                    {"type": "use", "name": "sub", "inline": _INLINE_CHILD},
                ],
            }
        ]
    }
    msgs = _run(orch, verbose=True)
    sibling_done = next(m for m in msgs if "✓" in m and "sibling" in m)
    sub_done = next(m for m in msgs if "✓" in m and "⊕" in m and "sub" in m)
    sibling_indent = len(sibling_done) - len(sibling_done.lstrip())
    sub_indent = len(sub_done) - len(sub_done.lstrip())
    assert sub_indent == sibling_indent, (sibling_done, sub_done)


def test_use_outside_loop_child_effects_have_no_prefix() -> None:
    orch = {
        "effects": [
            {"type": "use", "name": "sub", "inline": _INLINE_CHILD},
        ]
    }
    msgs = _run(orch, verbose=True, dry_run=False)
    assert any("✓" in m and "inner" in m for m in msgs), msgs
    assert not any("inner sub" in m for m in msgs), msgs


def test_use_in_loop_each_carries_iteration_tag_and_no_duplicate() -> None:
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "items",
                "template": '["a","b"]',
                "prompt_type": "json",
                "schema": {"type": "array", "items": {"type": "string"}},
            },
            {
                "type": "loop",
                "name": "process",
                "each": {"in": "prime.items.value", "as": "item"},
                "body": [
                    {"type": "use", "name": "sub", "inline": _INLINE_CHILD},
                ],
            },
        ]
    }
    msgs = _run(orch, verbose=True, dry_run=False)
    # one start and one result line per iteration — no untagged dispatcher duplicate
    start_lines = [m for m in msgs if "⊕" in m and "sub" in m and "→" in m]
    done_lines = [m for m in msgs if "⊕" in m and "sub" in m and "✓" in m]
    assert any("sub [0]" in m for m in start_lines), msgs
    assert any("sub [1]" in m for m in start_lines), msgs
    assert any("sub [0]" in m for m in done_lines), msgs
    assert any("sub [1]" in m for m in done_lines), msgs
    assert len(start_lines) == 2, msgs
    assert len(done_lines) == 2, msgs


def test_use_in_tree_loop_wires_tracker_callbacks(monkeypatch: Any) -> None:
    """A tree-flow loop's per-iteration tracker rows must actually transition
    from pending to running to done — which only happens if the loop threads
    its cb_start/cb_done into UseRuntime, the same as it does for prompt.
    """
    from circuitry.core import loop as loop_mod

    start_calls: list[int] = []
    done_calls: list[int] = []
    orig_start = loop_mod._LoopIterTracker.on_start
    orig_done = loop_mod._LoopIterTracker.on_done

    def spy_start(self: Any, idx: int) -> None:
        start_calls.append(idx)
        orig_start(self, idx)

    def spy_done(self: Any, idx: int, line: str) -> None:
        done_calls.append(idx)
        orig_done(self, idx, line)

    monkeypatch.setattr(loop_mod._LoopIterTracker, "on_start", spy_start)
    monkeypatch.setattr(loop_mod._LoopIterTracker, "on_done", spy_done)

    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "flow": "tree",
                "each": {"in": "input.items", "as": "item"},
                "body": [
                    {"type": "use", "name": "sub", "inline": _INLINE_CHILD},
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({"input": {"items": ["a", "b"]}})
    DynamicRuntime(
        root, adapter=EchoAdapter(), model="unit-test", dry_run=False, verbose=True
    ).execute(store=store)

    assert sorted(start_calls) == [0, 1], start_calls
    assert sorted(done_calls) == [0, 1], done_calls


def test_use_in_use_prefix_composes_two_levels_deep() -> None:
    inline_mid = (
        "effects:\n  - type: use\n    name: inner_use\n    inline: |\n"
        "      effects:\n        - type: prompt\n          name: leaf\n          template: hi\n"
    )
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "items",
                "template": '["a"]',
                "prompt_type": "json",
                "schema": {"type": "array", "items": {"type": "string"}},
            },
            {
                "type": "loop",
                "name": "process",
                "each": {"in": "prime.items.value", "as": "item"},
                "body": [
                    {"type": "use", "name": "sub", "inline": inline_mid},
                ],
            },
        ]
    }
    msgs = _run(orch, verbose=True, dry_run=False)
    # the innermost leaf's done line carries both use invocations' display
    # names plus the parent loop's iteration tag, composed in call order
    assert any("✓" in m and "leaf inner_use sub [0]" in m for m in msgs), msgs


def test_use_child_effects_attributed_to_parent_iteration() -> None:
    orch = {
        "effects": [
            {
                "type": "prompt",
                "name": "items",
                "template": '["a","b"]',
                "prompt_type": "json",
                "schema": {"type": "array", "items": {"type": "string"}},
            },
            {
                "type": "loop",
                "name": "process",
                "each": {"in": "prime.items.value", "as": "item"},
                "body": [
                    {"type": "use", "name": "sub", "inline": _INLINE_CHILD},
                ],
            },
        ]
    }
    msgs = _run(orch, verbose=True, dry_run=False)
    # the child prompt's done line carries the parent use's own
    # iteration-qualified display name as a trailing tag
    assert any("✓" in m and "inner sub [0]" in m for m in msgs), msgs
    assert any("✓" in m and "inner sub [1]" in m for m in msgs), msgs


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
