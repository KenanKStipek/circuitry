"""Tests for the safe CEL expression evaluator (cel_eval module)."""

from __future__ import annotations

from circuitry.core.cel_eval import evaluate_cel

# ---------------------------------------------------------------------------
# Correctness tests (backward compatibility)
# ---------------------------------------------------------------------------


class TestCorrectness:
    def test_simple_equality_true(self):
        ctx = {"input": {"ok": True}}
        assert evaluate_cel("state.input.ok == true", ctx) is True

    def test_simple_equality_false(self):
        ctx = {"input": {"ok": False}}
        assert evaluate_cel("state.input.ok == true", ctx) is False

    def test_nested_string_match(self):
        ctx = {"prime": {"get_role": {"value": "admin"}}}
        assert evaluate_cel("state.prime.get_role.value == 'admin'", ctx) is True

    def test_nested_string_mismatch(self):
        ctx = {"prime": {"get_role": {"value": "viewer"}}}
        assert evaluate_cel("state.prime.get_role.value == 'admin'", ctx) is False

    def test_size_function(self):
        ctx = {"items": [1, 2, 3]}
        assert evaluate_cel("size(state.items) >= 1", ctx) is True

    def test_size_function_empty(self):
        ctx = {"items": []}
        assert evaluate_cel("size(state.items) >= 1", ctx) is False

    def test_and_operator(self):
        ctx = {"a": True, "b": True}
        assert evaluate_cel("state.a == true && state.b == true", ctx) is True

    def test_or_operator(self):
        ctx = {"a": False, "b": True}
        assert evaluate_cel("state.a == true || state.b == true", ctx) is True

    def test_empty_expression(self):
        assert evaluate_cel("", {}) is False

    def test_whitespace_expression(self):
        assert evaluate_cel("   ", {}) is False

    def test_not_equal(self):
        ctx = {"status": "error"}
        assert evaluate_cel("state.status != 'ok'", ctx) is True


# ---------------------------------------------------------------------------
# Security tests
# ---------------------------------------------------------------------------


class TestSecurity:
    def test_class_traversal_blocked(self):
        assert evaluate_cel("().__class__.__bases__[0].__subclasses__()", {}) is False

    def test_import_blocked(self):
        assert evaluate_cel("__import__('os').system('echo pwned')", {}) is False

    def test_eval_blocked(self):
        assert evaluate_cel("eval('1+1')", {}) is False

    def test_open_blocked(self):
        assert evaluate_cel("open('/etc/passwd').read()", {}) is False

    def test_long_expression_returns_false(self):
        # Exceeds _MAX_EXPR_LENGTH (4096 chars)
        expr = "state.x == 'a'" + " && state.x == 'a'" * 500
        assert len(expr) > 4096
        assert evaluate_cel(expr, {"x": "a"}) is False


# ---------------------------------------------------------------------------
# Edge cases
# ---------------------------------------------------------------------------


class TestEdgeCases:
    def test_boolean_literal_true(self):
        assert evaluate_cel("true", {}) is True

    def test_boolean_literal_false(self):
        assert evaluate_cel("false", {}) is False

    def test_numeric_equality(self):
        assert evaluate_cel("1 == 1", {}) is True

    def test_deeply_nested_access(self):
        ctx = {"a": {"b": {"c": {"d": {"e": {"f": 42}}}}}}
        assert evaluate_cel("state.a.b.c.d.e.f == 42", ctx) is True

    def test_missing_key_returns_false(self):
        assert evaluate_cel("state.nonexistent.key == 'x'", {}) is False
