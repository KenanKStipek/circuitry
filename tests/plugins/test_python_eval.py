"""Tests for the ``python_eval`` sandboxed evaluation plugin.

Covers AC C.5 (compile-time rejection of sandbox escapes) plus the
``_write_``/``_inplacevar_`` guards added for subscript and augmented
assignment (issue #208), and the comprehension-scope fix for ``inputs``.
"""

from __future__ import annotations

import os
import time
import types
from dataclasses import dataclass

import pytest

pytest.importorskip("RestrictedPython")

from circuitry.plugins.python_eval import PythonEvalPlugin


def _run(code: str, *, mode: str = "eval", inputs: dict | None = None, timeout_seconds: int = 300):
    plugin = PythonEvalPlugin()
    params: dict = {"code": code, "mode": mode}
    if inputs is not None:
        params["inputs"] = inputs
    return plugin.execute(params=params, timeout_seconds=timeout_seconds)


class TestWriteGuard:
    def test_dict_item_assignment(self):
        result = _run('d = {}\nd["k"] = 1\nresult = d', mode="exec")
        assert result.value == {"k": 1}

    def test_list_item_assignment(self):
        result = _run("xs = [0]\nxs[0] = 5\nresult = xs", mode="exec")
        assert result.value == [5]

    def test_augmented_assignment_name(self):
        result = _run("n = 1\nn += 1\nresult = n", mode="exec")
        assert result.value == 2

    @pytest.mark.parametrize(
        ("code", "expected"),
        [
            ("n = 10\nn -= 3\nresult = n", 7),
            ("n = 3\nn *= 4\nresult = n", 12),
            ("n = 10\nn //= 3\nresult = n", 3),
            ("n = 10\nn %= 3\nresult = n", 1),
            ("n = 2\nn **= 5\nresult = n", 32),
            ("n = [1]\nn += [2]\nresult = n", [1, 2]),
        ],
    )
    def test_augmented_assignment_operators(self, code, expected):
        result = _run(code, mode="exec")
        assert result.value == expected

    def test_write_to_string_rejected(self):
        with pytest.raises(TypeError):
            _run("s[0] = 'x'", mode="exec", inputs={"s": "hello"})

    def test_write_to_frozen_dataclass_rejected(self):
        @dataclass(frozen=True)
        class Point:
            x: int

        with pytest.raises(TypeError):
            _run("p.x = 5\nresult = p", mode="exec", inputs={"p": Point(1)})

    def test_write_to_plain_object_attribute_rejected(self):
        # Unlike a `str`, this is a genuinely mutable object — asserting
        # on the wrapper's own error text (not just "some TypeError")
        # tells apart the guard actually running from a do-nothing guard
        # that would let Python's normal attribute assignment through.
        ns = types.SimpleNamespace()
        with pytest.raises(TypeError, match="attribute-less object"):
            _run("ns.x = 1\nresult = ns", mode="exec", inputs={"ns": ns})

    def test_write_to_plain_object_subscript_rejected(self):
        ns = types.SimpleNamespace()
        with pytest.raises(TypeError, match="item or slice assignment"):
            _run("ns[0] = 1\nresult = ns", mode="exec", inputs={"ns": ns})

    def test_augmented_assignment_does_not_bypass_write_guard(self):
        # `a += 99` must not fall back to `a.__iadd__(99)`: that would
        # mutate `a` in place through a dunder RestrictedPython's
        # attribute guard would otherwise keep out of reach, bypassing
        # `_write_` entirely (issue #208 follow-up).
        class Accumulator:
            def __init__(self) -> None:
                self.total = 0

            def __iadd__(self, other):
                self.total += other
                return self

        acc = Accumulator()
        with pytest.raises(TypeError):
            _run("a += 99", mode="exec", inputs={"a": acc})
        # Not `assert acc.total == 0`: `a` runs in the forked child's copy
        # of memory, so the parent's `acc` is untouched regardless of what
        # happens in the child -- that would pass even with no guard at
        # all. The `pytest.raises(TypeError)` above is what actually
        # proves the bypass is blocked.

    def test_subscript_augmented_assignment_rejected_at_compile_time(self):
        # RestrictedPython forbids augmented assignment of subscripts
        # outright (not routed through _inplacevar_ at all).
        with pytest.raises(PermissionError):
            _run("xs = [1]\nxs[0] += 1\nresult = xs", mode="exec")


class TestComprehensionScope:
    def test_nested_comprehension_sees_inputs(self):
        code = "result = [x for y in range(int(h)) for x in range(int(w))]"
        result = _run(code, mode="exec", inputs={"w": 2, "h": 2})
        assert result.value == [0, 1, 0, 1]

    def test_simple_comprehension_sees_inputs(self):
        result = _run(
            'result = [c for c in palette.split(",")]',
            mode="exec",
            inputs={"palette": "a,b,c"},
        )
        assert result.value == ["a", "b", "c"]


class TestSandboxRejections:
    """AC C.5 — unchanged: sandbox escapes are rejected at compile time."""

    def test_import_rejected(self):
        # `import` syntax itself compiles; it's rejected because
        # `__import__` isn't exposed in the sandboxed builtins.
        with pytest.raises(ImportError):
            _run("import os", mode="exec")

    def test_dunder_import_rejected(self):
        with pytest.raises(PermissionError):
            _run("__import__('os')")

    def test_dunder_attribute_access_rejected(self):
        with pytest.raises(PermissionError):
            _run("x.__class__", inputs={"x": 1})


class TestTimeout:
    def test_infinite_loop_is_killed_on_overrun(self):
        start = time.monotonic()
        with pytest.raises(RuntimeError, match="exceeded timeout of 1s"):
            _run("while True:\n    pass", mode="exec", timeout_seconds=1)
        assert time.monotonic() - start < 10

    def test_sleep_past_budget_is_killed(self):
        # The sandbox blocks `import`, so pass the sleep call in as an
        # input instead — this isn't CPU-bound, so only the wall-clock
        # join+terminate (not RLIMIT_CPU) can catch it.
        start = time.monotonic()
        with pytest.raises(RuntimeError, match="exceeded timeout of 1s"):
            _run(
                "sleep_fn(30)",
                mode="exec",
                inputs={"sleep_fn": time.sleep},
                timeout_seconds=1,
            )
        assert time.monotonic() - start < 10

    def test_fast_code_within_budget_still_succeeds(self):
        result = _run("1 + 1", timeout_seconds=1)
        assert result.value == 2

    def test_child_exit_without_result_raises_instead_of_hanging(self):
        # A child that dies without ever sending a result (crash, killed,
        # or `os._exit`) must not hang the parent's read forever: the
        # parent closes its own copy of the pipe's write end right after
        # start(), so EOF is reachable once the child's copy closes too.
        start = time.monotonic()
        with pytest.raises(RuntimeError, match="exited unexpectedly"):
            _run(
                "die(1)",
                mode="exec",
                inputs={"die": os._exit},
                timeout_seconds=5,
            )
        assert time.monotonic() - start < 5

    def test_result_larger_than_pipe_buffer_does_not_deadlock(self):
        # Regression: reading only after join() let a child's write block
        # on a full OS pipe buffer (tens of KiB) for the whole budget,
        # turning a real result into a false timeout.
        start = time.monotonic()
        result = _run("'x' * 2_000_000", timeout_seconds=10)
        assert result.value == "x" * 2_000_000
        assert time.monotonic() - start < 10
