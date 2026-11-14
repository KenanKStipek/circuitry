"""Tests for the CEL expression evaluator (cel_eval module).

Backed by ``cel-python`` since #185, so the suite covers real CEL —
macros, ``has()``, ``!``, ``?:``, CEL's own equality rules — alongside the
framework contracts layered on top of it (absent state reads false,
``strict: true``, the expression-length cap, the sandbox).
"""

from __future__ import annotations

from typing import ClassVar

import pytest

from circuitry.core.cel_eval import (
    CelEvaluationError,
    CelValidationError,
    evaluate_cel,
    evaluate_cel_expect,
    evaluate_cel_value,
    free_identifiers,
    state_paths,
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

    def test_string_literal_is_not_a_state_read(self, caplog):
        # Masked before resolution, so a quoted 'state.b.c' cannot make the
        # whole expression fall through to False via the absent-path rule.
        with caplog.at_level("WARNING", logger="circuitry.core.cel_eval"):
            assert evaluate_cel("state.a == 'state.b.c'", {"a": 1}) is False
        assert caplog.text == ""
        with caplog.at_level("WARNING", logger="circuitry.core.cel_eval"):
            assert evaluate_cel("state.b.c == 1", {"a": 1}) is False
        assert "state.b.c" in caplog.text

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
    CTX: ClassVar[dict] = {"input": {"items": [1, 2, 3], "empty": []}}

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

    def test_macro_variable_is_not_a_state_path(self, caplog):
        # `i` is macro-scoped; it must not be mistaken for an absent read.
        with caplog.at_level("WARNING", logger="circuitry.core.cel_eval"):
            assert evaluate_cel("state.input.items.all(i, i > 0)", self.CTX) is True
        assert caplog.text == ""


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
    """Regression for the translator's string mangling (noted on #187, #188)."""

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

    def test_state_path_literal_compares_against_the_literal(self):
        # The exact case from #188: `state.a == 'state.b.c'` must compare
        # against the literal string, not a rewritten `state["b"]["c"]`.
        ctx = {"a": "state.b.c", "b": {"c": "unrelated"}}
        assert evaluate_cel("state.a == 'state.b.c'", ctx) is True
        assert state_paths("state.a == 'state.b.c'") == ("state.a",)

    def test_double_quoted_state_path_literal_is_data(self):
        ctx = {"a": "state.b.c"}
        assert evaluate_cel('state.a == "state.b.c"', ctx) is True
        assert state_paths('state.a == "state.b.c"') == ("state.a",)

    def test_escaped_quote_inside_a_literal_is_data(self):
        ctx = {"a": "it's state.b.c"}
        assert evaluate_cel(r"state.a == 'it\'s state.b.c'", ctx) is True

    def test_literal_with_a_deeper_state_path_is_data(self):
        ctx = {"a": "state.b.c.d.e"}
        assert evaluate_cel("state.a == 'state.b.c.d.e'", ctx) is True
        assert state_paths("state.a == 'state.b.c.d.e'") == ("state.a",)

    def test_literal_next_to_a_real_state_path(self):
        # A literal containing a state-shaped string alongside an actual
        # state read: only the real path is collected and resolved.
        ctx = {"a": "state.b.c", "x": 1}
        assert evaluate_cel("state.a == 'state.b.c' && state.x == 1", ctx) is True
        assert state_paths("state.a == 'state.b.c' && state.x == 1") == (
            "state.a",
            "state.x",
        )


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


# ---------------------------------------------------------------------------
# Context projection — only what the expression reads is converted
# ---------------------------------------------------------------------------


class TestProjection:
    def test_unread_state_does_not_have_to_be_convertible(self):
        # A store holds every effect's output; a condition reads two paths
        # out of it. Nothing outside those paths is touched.
        class Unconvertible:
            def __iter__(self):
                raise AssertionError("state outside the expression was read")

        ctx = {"input": {"n": 2}, "prime": {"big": {"value": Unconvertible()}}}
        assert evaluate_cel("state.input.n > 1", ctx) is True

    def test_projection_does_not_mutate_the_caller_state(self):
        ctx = {"input": {"a": {"b": 1}}}
        before = {"input": {"a": {"b": 1}}}
        assert evaluate_cel("state.input.a.b == 1 && has(state.input.a.c)", ctx) is False
        assert ctx == before

    def test_overlapping_paths_keep_the_whole_subtree(self):
        # `state.input.a` is read whole *and* through a longer path; the
        # shorter read must still see every sibling key.
        ctx = {"input": {"a": {"b": 1, "c": 2}}}
        expr = "size(state.input.a) == 2 && state.input.a.b == 1"
        assert evaluate_cel(expr, ctx) is True

    def test_state_used_as_a_value_still_sees_everything(self):
        ctx = {"input": {"n": 1}, "prime": {"x": {"value": 2}}}
        assert evaluate_cel("size(state) == 2", ctx) is True


# ---------------------------------------------------------------------------
# The unset-path rule follows evaluation (#657)
# ---------------------------------------------------------------------------


class TestUnsetPathFollowsEvaluation:
    """Each row of the design's behaviour table, both plain and strict."""

    CTX: ClassVar[dict] = {"input": {"a": {"q": 1}, "items": [1, 2]}}

    def test_ternary_guarded_deeper_read_gives_fallback(self):
        expr = "has(state.input.a.b) ? state.input.a.b.c : 'fallback'"
        assert evaluate_cel_value(expr, self.CTX) == "fallback"

    def test_ternary_untaken_branch_gives_fallback(self):
        expr = "false ? state.input.x.y : 'fallback'"
        assert evaluate_cel_value(expr, self.CTX) == "fallback"

    def test_ternary_guarded_exact_read_gives_fallback(self):
        expr = "has(state.input.a.b.c) ? state.input.a.b.c : 'fallback'"
        assert evaluate_cel_value(expr, self.CTX) == "fallback"

    def test_guarded_negation_or_reaches_other_branch(self, caplog):
        expr = "!has(state.input.a.b) || state.input.a.b.c == 1"
        with caplog.at_level("WARNING", logger="circuitry.core.cel_eval"):
            assert evaluate_cel(expr, self.CTX) is True
        assert caplog.text == ""

    def test_guarded_and_short_circuits_without_warning(self, caplog):
        expr = "has(state.input.a.b) && state.input.a.b.c == 1"
        with caplog.at_level("WARNING", logger="circuitry.core.cel_eval"):
            assert evaluate_cel(expr, self.CTX) is False
        assert caplog.text == ""

    def test_false_and_short_circuits_without_reading_the_right(self, caplog):
        expr = "false && state.input.x.y == 1"
        with caplog.at_level("WARNING", logger="circuitry.core.cel_eval"):
            assert evaluate_cel(expr, self.CTX) is False
        assert caplog.text == ""

    def test_or_with_true_on_the_right_wins_over_the_errored_left(self, caplog):
        expr = "state.input.x.y == 1 || true"
        with caplog.at_level("WARNING", logger="circuitry.core.cel_eval"):
            assert evaluate_cel(expr, self.CTX) is True
        assert caplog.text == ""

    def test_exists_deciding_element_wins_over_an_errored_one(self, caplog):
        expr = "state.input.items.exists(i, i == 2 || state.input.z.y == 1)"
        with caplog.at_level("WARNING", logger="circuitry.core.cel_eval"):
            assert evaluate_cel(expr, self.CTX) is True
        assert caplog.text == ""

    def test_plain_unset_read_is_unchanged(self):
        assert evaluate_cel("state.input.a.b.c == 1", self.CTX) is False

    def test_two_unset_reads_in_or_names_the_right_hand_one(self, caplog):
        expr = "state.input.x.y == 1 || state.input.z.w == 1"
        with caplog.at_level("WARNING", logger="circuitry.core.cel_eval"):
            assert evaluate_cel(expr, self.CTX) is False
        assert len(caplog.records) == 1
        assert "reads unset state path 'state.input.z.w'" in caplog.text

    def test_map_literal_with_an_unset_value_gives_null(self):
        assert evaluate_cel_value("{'k': state.input.missing}", self.CTX) is None

    def test_disabled_node_after_has_gives_false_not_an_evaluation_error(self, caplog):
        ctx = {"prime": {"x": {"value": None}}}
        expr = "has(state.prime.x.value) && state.prime.x.value > 1"
        with caplog.at_level("WARNING", logger="circuitry.core.cel_eval"):
            assert evaluate_cel(expr, ctx) is False
        assert "state.prime.x.value" in caplog.text

    def test_an_unset_read_that_does_not_decide_reports_its_own_error(self):
        expr = "(state.input.m.x == 1 || true) && 1/0 == 1"
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel(expr, self.CTX)
        assert "CEL evaluation failed for" in str(excinfo.value)
        assert "division by zero" in str(excinfo.value)

    def test_strict_ternary_guarded_read_does_not_raise(self):
        expr = "has(state.input.a.b) ? state.input.a.b.c == 1 : true"
        assert evaluate_cel(expr, self.CTX, strict=True) is True

    def test_strict_false_and_short_circuit_does_not_raise(self):
        assert evaluate_cel("false && state.input.z == 1", self.CTX, strict=True) is False

    def test_strict_plain_unset_read_still_raises(self):
        with pytest.raises(CelEvaluationError):
            evaluate_cel("state.input.a.b.c == 1", self.CTX, strict=True)

    def test_strict_deciding_or_with_true_does_not_raise(self):
        expr = "state.prime.tick.value.price <= 10 || state.input.force"
        ctx = {"input": {"force": True}}
        assert evaluate_cel(expr, ctx, strict=True) is True

    def test_true_or_with_an_errored_map_body_wins_without_a_warning(self, caplog):
        expr = "true || [1].map(i, state.input.z.y).size() == 1"
        with caplog.at_level("WARNING", logger="circuitry.core.cel_eval"):
            assert evaluate_cel(expr, self.CTX) is True
        assert caplog.text == ""

    def test_true_or_with_an_errored_filter_body_wins_without_a_warning(self, caplog):
        expr = "true || state.input.items.filter(i, i > state.input.z.y).size() > 0"
        with caplog.at_level("WARNING", logger="circuitry.core.cel_eval"):
            assert evaluate_cel(expr, self.CTX) is True
        assert caplog.text == ""

    def test_true_or_with_an_errored_exists_one_body_wins_without_a_warning(self, caplog):
        expr = "true || state.input.items.exists_one(i, i == state.input.z.y)"
        with caplog.at_level("WARNING", logger="circuitry.core.cel_eval"):
            assert evaluate_cel(expr, self.CTX) is True
        assert caplog.text == ""

    def test_guarded_filter_body_evaluates_properly(self):
        expr = (
            "!has(state.input.a.b) || "
            "state.input.items.filter(i, i > state.input.a.b.min).size() > 0"
        )
        assert evaluate_cel(expr, self.CTX) is True


class TestAllExistsDecisiveElement:
    def test_all_false_element_wins_over_an_earlier_error(self, caplog):
        ctx = {"input": {"items": [1, 2]}}
        expr = "state.input.items.all(i, i == 1 && state.input.z.y == 1)"
        # i=1: i == 1 is True, which doesn't decide "&&", so the errored
        # right-hand side is the element's own result; i=2: i == 1 is
        # False, which decides "&&" to False without reading the right
        # side at all -- that decided False wins "all" over the earlier
        # error, with no warning (the unset read never decided anything).
        with caplog.at_level("WARNING", logger="circuitry.core.cel_eval"):
            assert evaluate_cel(expr, ctx) is False
        assert caplog.text == ""

    def test_all_vacuous_true_with_no_elements_errored(self):
        assert evaluate_cel("state.input.empty.all(i, i > 0)", {"input": {"empty": []}}) is True

    def test_exists_all_elements_error_propagates_the_last_errors(self, caplog):
        ctx = {"input": {"items": [1, 2]}}
        expr = "state.input.items.exists(i, state.input.z.y == i)"
        with caplog.at_level("WARNING", logger="circuitry.core.cel_eval"):
            assert evaluate_cel(expr, ctx) is False
        assert "state.input.z.y" in caplog.text


class TestNestingErrorWinsOverUnsetPath:
    def test_unset_read_past_the_nesting_cap_is_the_nesting_error_not_false(self):
        # cof's nesting limit is Python's recursion limit, so the depth at
        # which evaluation fails depends on the interpreter (3.12 moved it).
        # Find it rather than assume one: below it, the unset read gives the
        # unset-path rule's False; at it, the failure is the nesting error,
        # never that False, as it was when the rule ran before evaluation.
        def expr(depth: int) -> str:
            return "(" * depth + "state.input.missing" + ")" * depth + " == 1"

        for depth in range(40, 141):
            try:
                result = evaluate_cel(expr(depth), {"input": {}})
            except CelEvaluationError as exc:
                assert "reads unset state path" not in str(exc)
                return
            assert result is False
        pytest.fail("no depth up to 140 reached the nesting limit")


# ---------------------------------------------------------------------------
# Index/key/division errors: one line, never a container's contents (#629)
# ---------------------------------------------------------------------------


class TestIndexAndKeyErrorsNeverEmbedAContainer:
    CTX: ClassVar[dict] = {"input": {"m": {"a": 1, "b": [1, 2]}, "l": [1, 2, 3], "n": 5}}

    def _error_text(self, expr: str) -> str:
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel(expr, self.CTX, strict=True)
        return str(excinfo.value)

    def test_missing_string_key(self):
        assert self._error_text("state.input.m['zz'] == 1").endswith("no such key: 'zz'")

    def test_missing_int_key(self):
        assert self._error_text("state.input.m[7] == 1").endswith("no such key: 7")

    def test_list_index_out_of_range(self):
        text = self._error_text("state.input.l[5] == 1")
        assert text.endswith("list index out of range: 5")

    def test_list_indexed_by_a_string(self):
        text = self._error_text("state.input.l['a'] == 1")
        assert text.endswith("cannot index a list with a string")

    def test_int_indexed_by_an_int(self):
        text = self._error_text("state.input.n[0] == 1")
        assert text.endswith("cannot index an int with an int")

    def test_negative_index_is_unaffected(self):
        assert evaluate_cel("state.input.l[-1] == 3", self.CTX, strict=True) is True

    def test_dot_access_of_a_missing_map_key(self):
        # A macro-bound variable, not a ``state.`` path: the absent-path
        # rule only ever marks a ``state.`` occurrence, so this is the
        # only way to reach ``member_dot``'s own map lookup rather than
        # that rule's own "unset" outcome.
        text = self._error_text("[{'a': 1}].exists(x, x.zz == 1)")
        assert text.endswith("no such key: 'zz'")

    def test_field_selection_on_a_list_never_shows_its_elements(self):
        text = self._error_text("[[1, 2, 3]].exists(x, x.zz == 1)")
        assert text.endswith("cannot select field 'zz' on a list")

    def test_no_error_text_contains_a_tuple_repr(self):
        for expr in (
            "state.input.m['zz'] == 1",
            "state.input.m[7] == 1",
            "state.input.l[5] == 1",
            "state.input.l['a'] == 1",
            "state.input.n[0] == 1",
            "1 / 0 == 1",
            "1 % 0 == 1",
        ):
            text = self._error_text(expr)
            assert "KeyError" not in text
            assert "IndexError" not in text
            assert "TypeError" not in text
            assert "ZeroDivisionError" not in text


class TestDivisionAndModulusByZero:
    def test_division_by_zero(self):
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel("1 / 0 == 1", {})
        assert str(excinfo.value).endswith("division by zero")

    def test_modulus_by_zero(self):
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel("1 % 0 == 1", {})
        assert str(excinfo.value).endswith("modulus by zero")

    def test_float_division_by_zero_is_unaffected(self):
        assert evaluate_cel("1.0 / 0.0 > 0.0", {}) is True


class TestOtherOverloadErrorsNeverEmbedAValue:
    """Type-mismatch errors cel-python's own base ``Evaluator`` methods
    raise (relational/arithmetic operators ``_StateEvaluator`` leaves
    alone) -- some of these otherwise carry a whole operand's repr
    (``celtypes.celtypes.py``'s own ``no such overload: <repr> != <repr>``
    for ``<``/``<=``/``>``/``>=``), never just surfaced here."""

    CTX: ClassVar[dict] = {"input": {"m": {"a": 1, "b": [1, 2]}, "l": [1, 2, 3], "n": 5}}

    def _error_text(self, expr: str) -> str:
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel(expr, self.CTX)
        return str(excinfo.value)

    def test_relational_type_mismatch_names_the_types_not_the_values(self):
        # Matches `electricity-cel`'s own wording for the same failure
        # (`cel::ExecutionError::ValuesNotComparable`) exactly.
        text = self._error_text("state.input.n < state.input.m")
        assert text.endswith("cannot compare an int to a map")

    def test_in_type_mismatch(self):
        text = self._error_text("5 in state.input.n")
        assert text.endswith("cannot apply 'in' to an int and an int")

    def test_addition_type_mismatch(self):
        text = self._error_text("state.input.l + state.input.m")
        assert text.endswith("cannot apply '+' to a list and a map")

    def test_logical_and_type_mismatch(self):
        text = self._error_text("1 && true")
        assert text.endswith("cannot apply '&&' to an int and a bool")

    def test_logical_or_type_mismatch(self):
        text = self._error_text("1 || false")
        assert text.endswith("cannot apply '||' to an int and a bool")

    def test_ternary_non_bool_condition(self):
        text = self._error_text("(1 ? 2 : 3) == 2")
        assert text.endswith("cannot apply '?:' to an int, an int and a bool")

    def test_no_error_text_contains_a_python_class_repr_or_tuple(self):
        for expr in (
            "state.input.n < state.input.m",
            "5 in state.input.n",
            "state.input.l + state.input.m",
            "1 && true",
            "1 || false",
            "(1 ? 2 : 3) == 2",
        ):
            text = self._error_text(expr)
            assert "<class" not in text
            assert "Token(" not in text
            assert "MapType(" not in text
            assert "ListType(" not in text
            assert "StringType(" not in text
            assert "IntType(" not in text

    def test_in_element_type_mismatch_keeps_cel_pythons_own_bare_text(self):
        # cel-python's own ``operator_in`` raises this one without going
        # through the "found no matching overload for ..." shape
        # ``_error_text`` otherwise rewords, so it stays exactly this --
        # matching electricity's own ``in``-mismatch text for the same
        # case (issue #629).
        assert (
            self._error_text("1 in [1.0]")
            == "CEL evaluation failed for '1 in [1.0]': no such overload"
        )


class TestOverflowDuplicateKeyAndInvalidMapKeyType:
    """Every other cel-python error that embeds a Python class repr or
    value, worded Circuitry's own way instead (issue #629)."""

    def _error_text(self, expr: str) -> str:
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel(expr, {})
        return str(excinfo.value)

    def test_int_addition_overflow(self):
        text = self._error_text("9223372036854775807 + 1 == 1")
        assert text.endswith("return error for overflow")

    def test_int_multiplication_overflow(self):
        text = self._error_text("9223372036854775807 * 2 == 1")
        assert text.endswith("return error for overflow")

    def test_uint_addition_overflow(self):
        text = self._error_text("18446744073709551615u + 1u == 1u")
        assert text.endswith("return error for overflow")

    def test_uint_multiplication_overflow(self):
        text = self._error_text("18446744073709551615u * 2u == 1u")
        assert text.endswith("return error for overflow")

    def test_duplicate_map_literal_key(self):
        text = self._error_text("{'a': 1, 'a': 2}.a == 1")
        assert text.endswith("duplicate key: 'a'")

    def test_invalid_map_literal_key_type(self):
        text = self._error_text("{[1]: 2} == {}")
        assert text.endswith("cannot use a list as a key")

    def test_no_error_text_contains_a_python_class_repr(self):
        for expr in (
            "9223372036854775807 + 1 == 1",
            "9223372036854775807 * 2 == 1",
            "18446744073709551615u + 1u == 1u",
            "18446744073709551615u * 2u == 1u",
            "{'a': 1, 'a': 2}.a == 1",
            "{[1]: 2} == {}",
        ):
            text = self._error_text(expr)
            assert "StringType(" not in text
            assert "ListType(" not in text
            assert "<class" not in text


class TestMismatchedIntegerMapKeys:
    CTX: ClassVar[dict] = {"input": {"m": {1: "a"}}}

    def _error_text(self, expr: str) -> str:
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel(expr, self.CTX, strict=True)
        return str(excinfo.value)

    def test_uint_probe_against_an_int_key_of_the_same_value(self):
        assert self._error_text("state.input.m[1u] == 1").endswith(
            "cannot index a map with a uint"
        )

    def test_bool_probe_against_an_int_key_of_the_same_value(self):
        assert self._error_text("state.input.m[true] == 1").endswith(
            "cannot index a map with a bool"
        )

    def test_uint_probe_with_no_value_collision_is_a_plain_miss(self):
        assert self._error_text("state.input.m[5u] == 1").endswith("no such key: 5")


class TestEvaluateCelValueErrorText:
    """The ``cel`` tool's own entry point (#735) shares this evaluator,
    so every row above applies to it too."""

    def _error_text(self, expr: str) -> str:
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel_value(expr, {"input": {"l": [1, 2, 3]}}, strict=True)
        return str(excinfo.value)

    def test_list_index_out_of_range(self):
        assert self._error_text("state.input.l[5]").endswith(
            "list index out of range: 5"
        )

    def test_division_by_zero(self):
        assert self._error_text("1 / 0").endswith("division by zero")


# ---------------------------------------------------------------------------
# Nested too deeply: a check error, named at its own location (#629)
# ---------------------------------------------------------------------------


class TestNestedTooDeeplyIsACheckError:
    def _nested_expr(self, depth: int = 150) -> str:
        return "(" * depth + "state.input.missing" + ")" * depth

    def test_validate_cel_syntax_names_the_location(self):
        with pytest.raises(CelValidationError) as excinfo:
            validate_cel_syntax(
                self._nested_expr(),
                effect_path="prime.effects[0]",
                effect_name="g",
            )
        text = str(excinfo.value)
        assert text == (
            "CEL expression at 'prime.effects[0]' (effect 'g'): expression is "
            "nested too deeply"
        )

    def test_never_the_raw_recursion_error_text(self):
        with pytest.raises(CelValidationError) as excinfo:
            validate_cel_syntax(self._nested_expr(), effect_path="prime.effects[0]")
        text = str(excinfo.value)
        assert "maximum recursion depth" not in text
        assert "getting the str of an object" not in text

    def test_chain_length_sweep_is_always_clean(self):
        """Every length from just under to just over cof's own floor for a
        member-access chain (614 links on 3.11, after this fix) is either
        a clean success or exactly the one-line check error -- never a raw
        ``RecursionError`` escaping uncaught."""
        from circuitry.core import cel_eval

        expected = (
            "CEL expression at 'prime.effects[0]' (effect 'g'): expression is "
            "nested too deeply"
        )
        for depth in range(590, 701):
            cel_eval.clear_expression_cache()
            expr = "state.input" + ".a" * depth
            try:
                validate_cel_syntax(
                    expr, effect_path="prime.effects[0]", effect_name="g"
                )
            except CelValidationError as exc:
                assert str(exc) == expected, (depth, str(exc))


# ---------------------------------------------------------------------------
# Undeclared references: one line, never the activation (#658)
# ---------------------------------------------------------------------------


class TestUndeclaredReferenceIsOneLine:
    def _assert_one_line(self, text: str, name: str) -> None:
        assert "Activation(" not in text
        assert len(text) < 200
        assert f"undeclared reference to {name!r}" in text

    def test_unknown_identifier(self):
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel("unit == 1", {})
        self._assert_one_line(str(excinfo.value), "unit")

    def test_unknown_function(self):
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel("nope(1) == 1", {})
        self._assert_one_line(str(excinfo.value), "nope")

    def test_unknown_method(self):
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel("'a'.nope() == 1", {})
        self._assert_one_line(str(excinfo.value), "nope")

    def test_unknown_name_inside_map_macro(self):
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel("[1, 2].map(i, unit.text)", {})
        self._assert_one_line(str(excinfo.value), "unit")

    def test_unknown_name_inside_map_literal(self):
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel("{'first': unit.text} == {'first': 'a'}", {})
        self._assert_one_line(str(excinfo.value), "unit")

    def test_in_expect(self):
        with pytest.raises(CelEvaluationError) as excinfo:
            evaluate_cel_expect("nope(value)", value=1, meta={}, state={})
        self._assert_one_line(str(excinfo.value), "nope")


# ---------------------------------------------------------------------------
# free_identifiers (#658 check-time rule's input)
# ---------------------------------------------------------------------------


class TestFreeIdentifiers:
    def test_bare_loop_binding(self):
        assert free_identifiers("unit.text == 'a'") == ("unit",)

    def test_bare_namespace(self):
        assert free_identifiers("input.x == 1") == ("input",)

    def test_undefined_name(self):
        assert free_identifiers("stat.input.x == 1") == ("stat",)

    def test_type_names_and_two_arg_all_are_free_of_their_own_names(self):
        assert free_identifiers("type(1) == int && [1, 2].all(i, i > 0)") == ("int",)

    def test_expect_bindings(self):
        assert free_identifiers("has(value.x) && meta.error == null") == (
            "value",
            "meta",
        )

    def test_reduce_binds_nothing(self):
        assert free_identifiers("[0, 1, 2].reduce(r, i, 0, r + 2 * i + 1)") == (
            "r",
            "i",
            "r",
            "i",
        )

    def test_three_argument_map_binds_nothing(self):
        assert free_identifiers("[1, 2, 3].map(i, i > 0, i * 2)") == ("i", "i", "i")

    def test_own_walk_overflow_is_the_same_check_error_not_a_bare_one(self, monkeypatch):
        # This walk is a second, separate recursive pass over the tree
        # _compile already built -- a shape that compiles cleanly can still
        # overflow *this* walk's own, independent recursion budget.
        import circuitry.core.cel_eval as cel_eval_module

        def _raise(*_args, **_kwargs):
            raise RecursionError("maximum recursion depth exceeded")

        monkeypatch.setattr(cel_eval_module, "_walk_free_identifiers", _raise)
        with pytest.raises(CelValidationError) as excinfo:
            free_identifiers("state.a == 1")
        assert str(excinfo.value) == "expression is nested too deeply"

    def test_macro_variable_named_like_a_loop_binding_is_bound(self):
        assert free_identifiers("state.input.items.all(unit, unit > 0)") == ("state",)

    def test_nested_macros_accumulate_and_shadow(self):
        expr = "state.input.rows.all(r, r.items.exists(r, r > 0))"
        assert free_identifiers(expr) == ("state",)

    def test_message_construction_type_name_counts(self):
        assert free_identifiers("Msg{f: 1} == 1") == ("Msg",)

    def test_function_and_method_names_are_not_identifiers(self):
        assert free_identifiers("size(state.a) > 0 && state.a.startsWith('x')") == (
            "state",
            "state",
        )

    def test_first_name_in_source_order(self):
        assert free_identifiers("unit == 1 && stat == 2") == ("unit", "stat")

    def test_leading_dot_identifier_is_free(self):
        assert free_identifiers(".unit.text == 1") == ("unit",)

    def test_leading_dot_identifier_escapes_a_macro_variables_binding(self):
        # `.v` explicitly asks CEL to skip local scoping (the leading
        # dot) and resolve against the root package, so it is never
        # exempt as the macro's own bound variable the bare `v` is.
        assert free_identifiers("state.input.items.all(v, .v > 0)") == ("state", "v")

    def test_leading_dot_function_call_is_not_an_identifier(self):
        assert free_identifiers(".size(1) == 1") == ()
