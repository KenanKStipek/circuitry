"""Tests for the ``python_eval`` sandboxed evaluation plugin.

Covers AC C.5 (compile-time rejection of sandbox escapes) plus the
``_write_``/``_inplacevar_`` guards added for subscript and augmented
assignment (issue #208), and the comprehension-scope fix for ``inputs``.
"""

from __future__ import annotations

from dataclasses import dataclass

import pytest

pytest.importorskip("RestrictedPython")

from circuitry.plugins.python_eval import PythonEvalPlugin


def _run(code: str, *, mode: str = "eval", inputs: dict | None = None):
    plugin = PythonEvalPlugin()
    params: dict = {"code": code, "mode": mode}
    if inputs is not None:
        params["inputs"] = inputs
    return plugin.execute(params=params)


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
