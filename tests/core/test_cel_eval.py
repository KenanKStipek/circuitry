"""Tests for the safe CEL expression evaluator (cel_eval module)."""

from __future__ import annotations

import pytest

from circuitry.core.cel_eval import (
    CelEvaluationError,
    CelValidationError,
    evaluate_cel,
    unresolved_state_path,
    validate_cel_syntax,
)

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

    def test_not_equal(self):
        ctx = {"status": "error"}
        assert evaluate_cel("state.status != 'ok'", ctx) is True

    def test_string_literal_with_bang_still_evaluates(self):
        # '!' inside a quoted literal is data, not the unsupported operator.
        ctx = {"msg": "done!"}
        assert evaluate_cel("state.msg == 'done!'", ctx) is True


# ---------------------------------------------------------------------------
# Fail-loud: no more silent ``False`` on error (issue #184)
# ---------------------------------------------------------------------------


class TestFailLoud:
    def test_empty_expression_raises(self):
        with pytest.raises(CelEvaluationError):
            evaluate_cel("", {})

    def test_whitespace_expression_raises(self):
        with pytest.raises(CelEvaluationError):
            evaluate_cel("   ", {})

    def test_syntax_error_raises(self):
        with pytest.raises(CelEvaluationError):
            evaluate_cel("state.a ==", {"a": 1})

    def test_error_carries_expression_and_cause(self):
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel("size(state.a) > nope(1)", {"a": [1]})
        assert excinfo.value.expression == "size(state.a) > nope(1)"
        assert excinfo.value.__cause__ is not None
        assert "size(state.a) > nope(1)" in str(excinfo.value)

    def test_unsupported_negation_named_at_runtime(self):
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel("!state.prime.x.value", {"prime": {"x": {"value": True}}})
        assert "not supported yet" in str(excinfo.value)

    def test_unknown_function_raises(self):
        with pytest.raises(CelEvaluationError):
            evaluate_cel("nope(state.a)", {"a": 1})


# ---------------------------------------------------------------------------
# Security tests — blocked constructs now raise instead of returning False
# ---------------------------------------------------------------------------


class TestSecurity:
    def test_class_traversal_blocked(self):
        with pytest.raises(CelEvaluationError):
            evaluate_cel("().__class__.__bases__[0].__subclasses__()", {})

    def test_import_blocked(self):
        with pytest.raises(CelEvaluationError):
            evaluate_cel("__import__('os').system('echo pwned')", {})

    def test_eval_blocked(self):
        with pytest.raises(CelEvaluationError):
            evaluate_cel("eval('1+1')", {})

    def test_open_blocked(self):
        with pytest.raises(CelEvaluationError):
            evaluate_cel("open('/etc/passwd').read()", {})

    def test_long_expression_raises(self):
        # Exceeds _MAX_EXPR_LENGTH (4096 chars)
        expr = "state.x == 'a'" + " && state.x == 'a'" * 500
        assert len(expr) > 4096
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel(expr, {"x": "a"})
        assert "too long" in str(excinfo.value)


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


# ---------------------------------------------------------------------------
# Absent state — data, not a defect (see the module docstring)
# ---------------------------------------------------------------------------


class TestAbsentState:
    def test_missing_key_is_false(self):
        assert evaluate_cel("state.nonexistent.key == 'x'", {}) is False

    def test_path_through_none_is_false(self):
        # A disabled node writes {"value": None}; reading through it is the
        # same "nothing there" as a key that was never written.
        ctx = {"prime": {"a": {"value": None}}}
        assert evaluate_cel("state.prime.a.value.count == 2", ctx) is False
        assert evaluate_cel("size(state.prime.a.value) > 0", ctx) is False

    def test_absent_path_is_logged_not_silent(self, caplog):
        with caplog.at_level("WARNING", logger="circuitry.core.cel_eval"):
            assert evaluate_cel("state.prime.typo.value == 1", {}) is False
        assert "state.prime.typo.value" in caplog.text

    def test_string_literal_is_not_a_state_read(self):
        # Masked before resolution, so a quoted 'state.b.c' cannot make the
        # whole expression fall through to False.
        assert unresolved_state_path("state.a == 'state.b.c'", {"a": 1}) is None
        assert unresolved_state_path("state.b.c == 1", {"a": 1}) == "state.b.c"

    def test_present_falsy_values_are_not_absent(self):
        ctx = {"a": {"n": 0, "s": "", "l": [], "b": False}}
        assert evaluate_cel("state.a.n == 0", ctx) is True
        assert evaluate_cel("state.a.s == ''", ctx) is True
        assert evaluate_cel("size(state.a.l) == 0", ctx) is True
        assert evaluate_cel("state.a.b == false", ctx) is True


# ---------------------------------------------------------------------------
# Compile-time validator
# ---------------------------------------------------------------------------


class TestValidator:
    @pytest.mark.parametrize(
        "expr",
        [
            "state.input.ok == true",
            "state.prime.get_role.value == 'admin'",
            "size(state.input.items) >= 1",
            "state.a == true && state.b == false",
            "state.a == 1 || state.b != 2",
            "state.msg == 'done!'",  # '!' inside a literal is fine
            "state.msg == 'why?'",  # '?' inside a literal is fine
        ],
    )
    def test_supported_expressions_pass(self, expr):
        validate_cel_syntax(expr, effect_path="effects[0]")

    @pytest.mark.parametrize(
        ("expr", "needle"),
        [
            ("!state.prime.x.value", "'!' is not supported"),
            ("has(state.input.name)", "'has()' macro"),
            ("state.input.items.filter(i, i > 1)", "comprehension macros"),
            ("state.input.items.exists(i, i > 1)", "comprehension macros"),
            ("state.input.items.all(i, i > 1)", "comprehension macros"),
            ("state.input.items.map(i, i)", "comprehension macros"),
            ("[x for x in state.input.items]", "comprehensions are not supported"),
            ("state.input.ok ? 1 : 2", "conditional/ternary"),
        ],
    )
    def test_unsupported_constructs_rejected(self, expr, needle):
        with pytest.raises(CelValidationError) as excinfo:
            validate_cel_syntax(expr, effect_path="effects[0]")
        message = str(excinfo.value)
        assert needle in message
        assert "not supported yet" in message
        assert expr in message

    def test_unparseable_expression_rejected(self):
        with pytest.raises(CelValidationError) as excinfo:
            validate_cel_syntax("state.a ==", effect_path="effects[2]")
        assert "does not parse" in str(excinfo.value)
        assert "effects[2]" in str(excinfo.value)

    def test_message_names_effect_and_expression(self):
        with pytest.raises(CelValidationError) as excinfo:
            validate_cel_syntax(
                "!state.prime.x.value",
                effect_path="effects[1]",
                effect_name="gate",
            )
        message = str(excinfo.value)
        assert "effects[1]" in message
        assert "gate" in message
        assert "!state.prime.x.value" in message

    def test_empty_expression_rejected(self):
        with pytest.raises(CelValidationError):
            validate_cel_syntax("   ", effect_path="effects[0]")

    def test_long_expression_rejected(self):
        expr = "state.x == 'a'" + " && state.x == 'a'" * 500
        with pytest.raises(CelValidationError) as excinfo:
            validate_cel_syntax(expr, effect_path="effects[0]")
        assert "too long" in str(excinfo.value)

    def test_validation_error_is_a_value_error(self):
        # The compiler reports every other rejection as ValueError; CEL
        # validation must flow through the same handling.
        with pytest.raises(ValueError):
            validate_cel_syntax("!state.a", effect_path="effects[0]")

    def test_label_is_used_in_the_message(self):
        with pytest.raises(CelValidationError) as excinfo:
            validate_cel_syntax(
                "!state.a",
                effect_path="effects[0]",
                label="Loop while CEL expression",
            )
        assert str(excinfo.value).startswith("Loop while CEL expression")
