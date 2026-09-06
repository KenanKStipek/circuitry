"""Tests for the CEL expression evaluator (cel_eval module).

Backed by ``cel-python`` since #185, so the suite covers real CEL —
macros, ``has()``, ``!``, ``?:``, CEL's own equality rules — alongside the
framework contracts layered on top of it (absent state reads false,
``strict: true``, the expression-length cap, the sandbox).
"""

from __future__ import annotations

import pytest

from circuitry.core.cel_eval import (
    CelEvaluationError,
    CelValidationError,
    evaluate_cel,
    state_paths,
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
            # Real CEL the old translator had to reject by name (#184).
            "!state.prime.x.value",
            "has(state.input.name)",
            "state.input.items.filter(i, i > 1).size() > 0",
            "state.input.items.exists(i, i > 1)",
            "state.input.items.all(i, i > 1)",
            "state.input.items.exists_one(i, i > 1)",
            "size(state.input.items.map(i, i * 2)) == 2",
            "state.input.ok ? state.input.a : state.input.b",
        ],
    )
    def test_supported_expressions_pass(self, expr):
        validate_cel_syntax(expr, effect_path="effects[0]")

    @pytest.mark.parametrize(
        "expr",
        [
            "[x for x in state.input.items]",  # a Python comprehension
            "not state.input.ok",  # Python's negation, not CEL's
            "state.input.a and state.input.b",  # && in CEL
            "state.input.n ** 2 > 4",  # no exponent operator in CEL
        ],
    )
    def test_non_cel_rejected(self, expr):
        with pytest.raises(CelValidationError) as excinfo:
            validate_cel_syntax(expr, effect_path="effects[0]")
        message = str(excinfo.value)
        assert "does not parse" in message
        assert expr in message

    def test_unparseable_expression_rejected(self):
        with pytest.raises(CelValidationError) as excinfo:
            validate_cel_syntax("state.a ==", effect_path="effects[2]")
        assert "does not parse" in str(excinfo.value)
        assert "effects[2]" in str(excinfo.value)

    def test_message_names_effect_and_expression(self):
        with pytest.raises(CelValidationError) as excinfo:
            validate_cel_syntax(
                "not state.prime.x.value",
                effect_path="effects[1]",
                effect_name="gate",
            )
        message = str(excinfo.value)
        assert "effects[1]" in message
        assert "gate" in message
        assert "not state.prime.x.value" in message

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
            validate_cel_syntax("state.a ==", effect_path="effects[0]")

    def test_label_is_used_in_the_message(self):
        with pytest.raises(CelValidationError) as excinfo:
            validate_cel_syntax(
                "state.a ==",
                effect_path="effects[0]",
                label="Loop while CEL expression",
            )
        assert str(excinfo.value).startswith("Loop while CEL expression")


# ---------------------------------------------------------------------------
# Real CEL — macros, has(), '!', ternary, spec equality (issue #185)
# ---------------------------------------------------------------------------


class TestMacros:
    CTX = {"input": {"items": [1, 2, 3], "empty": []}}

    def test_all(self):
        assert evaluate_cel("state.input.items.all(i, i > 0)", self.CTX) is True
        assert evaluate_cel("state.input.items.all(i, i > 1)", self.CTX) is False

    def test_exists(self):
        assert evaluate_cel("state.input.items.exists(i, i == 3)", self.CTX) is True
        assert evaluate_cel("state.input.items.exists(i, i == 9)", self.CTX) is False

    def test_exists_one(self):
        assert (
            evaluate_cel("state.input.items.exists_one(i, i > 2)", self.CTX) is True
        )
        assert (
            evaluate_cel("state.input.items.exists_one(i, i > 1)", self.CTX) is False
        )

    def test_map(self):
        expr = "size(state.input.items.map(i, i * 2)) == 3"
        assert evaluate_cel(expr, self.CTX) is True

    def test_filter(self):
        expr = "size(state.input.items.filter(i, i > 1)) == 2"
        assert evaluate_cel(expr, self.CTX) is True

    def test_all_on_empty_list_is_vacuously_true(self):
        assert evaluate_cel("state.input.empty.all(i, i > 0)", self.CTX) is True

    def test_macro_over_maps(self):
        ctx = {"input": {"rows": [{"n": 1}, {"n": 5}]}}
        assert evaluate_cel("state.input.rows.exists(r, r.n > 4)", ctx) is True

    def test_macro_variable_is_not_a_state_path(self):
        # `i` is macro-scoped; it must not be mistaken for an absent read.
        assert unresolved_state_path(
            "state.input.items.all(i, i > 0)", self.CTX
        ) is None


class TestHasMacro:
    def test_present_field(self):
        assert evaluate_cel("has(state.input.name)", {"input": {"name": "x"}}) is True

    def test_absent_field(self):
        assert evaluate_cel("has(state.input.name)", {"input": {}}) is False

    def test_guarded_read_is_exempt_from_the_absent_rule(self):
        # The whole point of has(): the expression handles absence itself,
        # so it must not collapse to false via the structural pre-check.
        expr = "has(state.input.n) && state.input.n > 1"
        assert evaluate_cel(expr, {"input": {"n": 5}}) is True
        assert evaluate_cel(expr, {"input": {}}) is False

    def test_guarded_negation_reaches_the_other_branch(self):
        expr = "!has(state.input.n) || state.input.n > 1"
        assert evaluate_cel(expr, {"input": {}}) is True

    def test_has_argument_is_still_a_state_path_for_the_grammar(self):
        assert state_paths("has(state.input.name)") == ("state.input.name",)


class TestNegationAndTernary:
    def test_negation(self):
        ctx = {"prime": {"x": {"value": False}}}
        assert evaluate_cel("!state.prime.x.value", ctx) is True

    def test_double_negation(self):
        ctx = {"prime": {"x": {"value": True}}}
        assert evaluate_cel("!!state.prime.x.value", ctx) is True

    def test_ternary_picks_a_branch(self):
        ctx = {"input": {"ok": True, "a": 1, "b": 2}}
        assert evaluate_cel("(state.input.ok ? state.input.a : state.input.b) == 1", ctx) is True
        ctx["input"]["ok"] = False
        assert evaluate_cel("(state.input.ok ? state.input.a : state.input.b) == 2", ctx) is True


class TestSpecEquality:
    def test_int_is_not_bool(self):
        # Python would say 1 == True; CEL says values of different types
        # are simply unequal.
        assert evaluate_cel("1 == true", {}) is False
        assert evaluate_cel("1 != true", {}) is True

    def test_state_int_is_not_state_bool(self):
        ctx = {"input": {"n": 1, "flag": True}}
        assert evaluate_cel("state.input.n == state.input.flag", ctx) is False

    def test_string_is_not_int(self):
        assert evaluate_cel("'1' == 1", {}) is False

    def test_cross_numeric_equality_still_holds(self):
        # int/uint/double are one numeric family in CEL; only *type*
        # mismatch is false.
        assert evaluate_cel("1 == 1.0", {}) is True
        assert evaluate_cel("1 == 1u", {}) is True

    def test_list_and_map_equality(self):
        ctx = {"input": {"a": [1, 2], "m": {"k": "v"}}}
        assert evaluate_cel("state.input.a == [1, 2]", ctx) is True
        assert evaluate_cel("state.input.m == {'k': 'v'}", ctx) is True

    def test_equality_does_not_swallow_a_real_error(self):
        # A no-such-overload on == is false; a genuine evaluation failure
        # inside an operand still raises.
        with pytest.raises(CelEvaluationError):
            evaluate_cel("size(state.input.n) == 1", {"input": {"n": 3}})


class TestStandardFunctions:
    def test_size_of_string_list_and_map(self):
        ctx = {"input": {"s": "abc", "l": [1, 2], "m": {"a": 1, "b": 2, "c": 3}}}
        assert evaluate_cel("size(state.input.s) == 3", ctx) is True
        assert evaluate_cel("size(state.input.l) == 2", ctx) is True
        assert evaluate_cel("size(state.input.m) == 3", ctx) is True

    def test_int_and_string_conversions(self):
        ctx = {"input": {"n": 7, "s": "42"}}
        assert evaluate_cel("string(state.input.n) == '7'", ctx) is True
        assert evaluate_cel("int(state.input.s) == 42", ctx) is True

    def test_string_methods(self):
        ctx = {"input": {"s": "hello world"}}
        assert evaluate_cel("state.input.s.startsWith('hello')", ctx) is True
        assert evaluate_cel("state.input.s.contains('lo wo')", ctx) is True
        assert evaluate_cel("state.input.s.matches('^h.*d$')", ctx) is True

    def test_in_operator(self):
        ctx = {"input": {"items": ["a", "b"]}}
        assert evaluate_cel("'a' in state.input.items", ctx) is True


class TestStringLiterals:
    """Regression for the translator's string mangling (noted on #187)."""

    def test_state_path_inside_a_literal_is_data(self):
        ctx = {"input": {"msg": "state.x.y"}}
        assert evaluate_cel("state.input.msg == 'state.x.y'", ctx) is True

    def test_literal_is_not_collected_as_a_path(self):
        assert state_paths("state.input.msg == 'state.x.y'") == (
            "state.input.msg",
        )

    def test_operators_inside_literals_are_data(self):
        ctx = {"input": {"msg": "a && b || !c ? d : e"}}
        assert evaluate_cel(
            "state.input.msg == 'a && b || !c ? d : e'", ctx
        ) is True


# ---------------------------------------------------------------------------
# strict: true — an unresolved path is a failure, not a false condition
# ---------------------------------------------------------------------------


class TestStrict:
    def test_absent_path_raises_under_strict(self):
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel("state.prime.tick.value.price <= 10", {}, strict=True)
        assert "state.prime.tick.value.price" in str(excinfo.value)
        assert "strict" in str(excinfo.value)

    def test_path_through_none_raises_under_strict(self):
        ctx = {"prime": {"tick": {"value": None}}}
        with pytest.raises(CelEvaluationError):
            evaluate_cel("state.prime.tick.value.price <= 10", ctx, strict=True)

    def test_resolved_path_evaluates_normally_under_strict(self):
        ctx = {"prime": {"tick": {"value": {"price": 9}}}, "input": {"stop": 10}}
        expr = "state.prime.tick.value.price <= state.input.stop"
        assert evaluate_cel(expr, ctx, strict=True) is True

    def test_default_is_still_false_by_rule(self):
        assert evaluate_cel("state.prime.tick.value.price <= 10", {}) is False


# ---------------------------------------------------------------------------
# Sandbox — no traversal from CEL into Python objects
# ---------------------------------------------------------------------------


class TestSandbox:
    def test_state_object_exposes_no_attributes(self):
        # A Python object in state has no CEL counterpart, so it is not
        # traversable: the read is structurally unresolved (false by rule,
        # or a loud failure under strict) and never yields the attribute.
        class Secret:
            token = "s3cr3t"

        ctx = {"input": {"obj": Secret()}}
        assert evaluate_cel("state.input.obj.token == 's3cr3t'", ctx) is False
        with pytest.raises(CelEvaluationError):
            evaluate_cel("state.input.obj.token == 's3cr3t'", ctx, strict=True)

    def test_state_object_exposes_no_dunders(self):
        ctx = {"input": {"obj": object()}}
        assert evaluate_cel("state.input.obj.__class__ == 1", ctx) is False
        with pytest.raises(CelEvaluationError):
            evaluate_cel("state.input.obj.__class__ == 1", ctx, strict=True)

    def test_an_object_leaf_is_null_not_the_object(self):
        ctx = {"input": {"obj": object()}}
        assert evaluate_cel("state.input.obj == null", ctx) is True

    def test_callables_in_state_are_not_callable_from_cel(self):
        ctx = {"input": {"f": len}}
        with pytest.raises(CelEvaluationError):
            evaluate_cel("state.input.f('abc') == 3", ctx)

    def test_nothing_but_state_is_in_scope(self):
        with pytest.raises(CelEvaluationError):
            evaluate_cel("__builtins__ == 1", {})


# ---------------------------------------------------------------------------
# Parse-tree state paths (the namespace safety layer's input)
# ---------------------------------------------------------------------------


class TestStatePaths:
    def test_longest_chain_only(self):
        assert state_paths("state.a.b.c == 1") == ("state.a.b.c",)

    def test_multiple_paths_in_order(self):
        assert state_paths("state.a.b == 1 && state.c.d == 2") == (
            "state.a.b",
            "state.c.d",
        )

    def test_indexed_reads_report_the_resolvable_prefix(self):
        assert state_paths("state.input.items[0].n == 1") == ("state.input.items",)

    def test_non_state_identifiers_are_ignored(self):
        assert state_paths("item.name == 'x' && 1 == 1") == ()

    def test_unparseable_expression_raises(self):
        with pytest.raises(CelValidationError):
            state_paths("state.a ==")


# ---------------------------------------------------------------------------
# Compiled-expression cache
# ---------------------------------------------------------------------------


class TestCache:
    def test_repeated_evaluation_reuses_one_compiled_program(self):
        from circuitry.core import cel_eval

        cel_eval.clear_expression_cache()
        expr = "state.input.n > 1"
        for _ in range(5):
            evaluate_cel(expr, {"input": {"n": 2}})
        assert list(cel_eval._CACHE) == [expr]

    def test_cache_does_not_leak_results_between_contexts(self):
        expr = "state.input.n > 1"
        assert evaluate_cel(expr, {"input": {"n": 2}}) is True
        assert evaluate_cel(expr, {"input": {"n": 0}}) is False
