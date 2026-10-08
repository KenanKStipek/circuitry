//! Strict-type equality and containment: the one place celpy's own
//! `==`/`!=`/`in` diverge from `cel`'s own, more permissive rules once a
//! `list`/`map` or `in` is involved (DESIGN.md §7.2, issue #379 review
//! finding 5).
//!
//! A top-level `==`/`!=` between two scalars already matches the CEL
//! spec in plain `cel` (cross-numeric equal, cross-type otherwise
//! false) — that part needs no help, and this module's functions
//! reproduce it too (DESIGN.md's heterogeneous-equality examples still
//! pass). The divergence is what celpy's own `ListType`/`MapType`
//! `__eq__` do *inside* a container (`celtypes.py`): each delegates to
//! its elements' own `__eq__`, which for `IntType`/`UintType`/
//! `DoubleType` is decorated with `@type_matched` — raising unless the
//! other side is the exact same type, not just numeric. `cel`'s own
//! list/map equality instead compares elements with its permissive,
//! cross-numeric `PartialEq`, never raising inside a container the way
//! celpy's own, nested dunders do.
//!
//! celpy's top-level `_==_`/`_!=_` override (`core/cel_eval.py`'s
//! `_spec_eq`/`_spec_ne`) catches that nested `TypeError` and falls back
//! to its own rule: equal only if the *container's own* two operands are
//! themselves numeric — never true for two lists/maps, which is why
//! `[1, 2] == [1.0, 2.0]` is `false` in celpy. [`spec_eq`]/[`spec_ne`]
//! reproduce that by attempting [`strict_eq`] (celpy's exact-type rule,
//! applied recursively) first and falling back the same way on `Err`.
//!
//! `in` has no such fallback (`evaluation.py`'s `operator_in`): a type
//! mismatch it can't look past raises outright rather than deciding
//! `false` (`1 in [1.0]`, `state.x in ['a', 'b']` with `x` an int).
//! [`strict_in_fn`] reproduces that directly — but only for a mismatch
//! that actually raises in celpy. `operator_in`'s own `c == item` is a
//! plain Python `==`, which already resolves the two-sided `NotImplemented`
//! fallback *before* `operator_in` ever sees a `TypeError`: a `string`,
//! `bool`, `null`, `bytes`, `timestamp` or `duration` compared with a
//! value of a different, equally undecorated type returns `false` with
//! no exception (`'b' in ['a', None]` is `false`, not an error) — only a
//! mismatch touching `int`/`uint`/`double` (`@type_matched`) or a
//! `list`/`map` against something other than `null` or its own kind
//! actually raises. [`raises_on_mismatch`] decides which of the two
//! [`strict_eq`]'s own catch-all falls into; the first review's P1 fix
//! (`strict_in_fn` erroring on *every* type mismatch) overcorrected,
//! regressing this specific case.
//!
//! [`paths::rewrite`](crate::paths::rewrite) substitutes this module's
//! functions for `cel`'s own `_==_`/`_!=_`/`@in`, the same way it does
//! for [`crate::ordering`]'s.
//!
//! `in` over a bare `string`/`bytes` operand (not a `list`/`map`) is
//! celpy's own `for c in container` (`evaluation.py`), which iterates a
//! `str` one Unicode character at a time and a `bytes` one `int` at a
//! time — `cel`'s own `@in` has no indexer for either at all (fourth
//! review finding 1). [`strict_in_fn`]'s `elements` covers both the
//! same way the `list`/`map` arms already did.
//!
//! A nested `list`/`map` comparison — reachable only through `in`,
//! since a top-level `==` already falls back to [`spec_eq`] — must
//! compare *every* element before deciding, not stop at the first one
//! that raises: celpy's own `ListType`/`MapType.__eq__` (`celtypes.py`)
//! fold the per-element comparisons with CEL's own error-absorbing
//! `&&`, where a `false` anywhere wins over an error anywhere else, so
//! `[1, 2] in [[1.0, 3]]` is `false` (the second pair, `2 == 3`,
//! decides it) rather than raising on the first pair's numeric
//! mismatch. [`strict_eq`]'s `List`/`Map` arms used to return the
//! first `Err` the `?` operator reached, including through a
//! `HashMap`'s own unspecified iteration order for `Map` — a `false`
//! result could flip to an error from one run to the next depending on
//! which key happened to be visited first (fourth review finding 6).
//! They now scan every element, deciding `false` immediately if any
//! comparison is `Ok(false)` (nothing later can undo that) and only
//! raising if every comparison that completed was `Ok(true)` or `Err`
//! and at least one was `Err`. `Map`'s own arm checks both maps hold
//! the *same key set* before comparing a single value, exactly the
//! "keys first" rule celpy's own dict comparison already gets for
//! free (a `dict`'s `==` is `False` outright for a mismatched key set,
//! with no value comparison, hence no chance for one to raise) —
//! without it, a value comparison for a key present in both maps could
//! still raise before the key-set mismatch that should have decided
//! `false` on its own was ever noticed.
//!
//! celpy's own asymmetry for `bool` on the *left* of `int`/`uint` *is*
//! reproduced: `BoolType` has no `__eq__` override, so Python's dispatch
//! tries it first and resolves via the plain `int` it subclasses —
//! `true == 1`/`true == 1u` is `true`, never raising, while `1 == true`/
//! `1u == true` (`int`'s own `@type_matched` tried first) raises, caught
//! by [`spec_eq`]'s fallback into `false`. `bool` against `double` raises
//! either way (`int.__eq__` returns `NotImplemented` for a `float`
//! comparand rather than resolving it, so Python falls through to
//! `double`'s own `@type_matched`, which raises for `bool` same as any
//! other non-`double`).

use cel::common::ast::operators;
use cel::extractors::Arguments;
use cel::{Context, ExecutionError, Value};

const STRICT_EQ: &str = "__electricity_strict_eq";
const STRICT_NE: &str = "__electricity_strict_ne";
const STRICT_IN: &str = "__electricity_strict_in";

/// The registered function name standing in for *func_name* if it is
/// `cel`'s own `==`, `!=` or `in`, else `None`.
pub fn strict_function_name(func_name: &str) -> Option<&'static str> {
    match func_name {
        operators::EQUALS => Some(STRICT_EQ),
        operators::NOT_EQUALS => Some(STRICT_NE),
        operators::IN => Some(STRICT_IN),
        _ => None,
    }
}

/// Registers the functions [`strict_function_name`] renames operators to.
pub fn register(ctx: &mut Context) {
    ctx.add_function(STRICT_EQ, strict_eq_fn)
        .expect("name not already declared");
    ctx.add_function(STRICT_NE, strict_ne_fn)
        .expect("name not already declared");
    ctx.add_function(STRICT_IN, strict_in_fn)
        .expect("name not already declared");
}

/// CEL's numeric family for the top-level fallback: `int`, `uint` and
/// `double` compare across types; `bool` doesn't join them here either
/// (`Value::Bool` is its own variant, never confused with a number).
fn is_numeric(v: &Value) -> bool {
    matches!(v, Value::Int(_) | Value::UInt(_) | Value::Float(_))
}

fn numeric_f64(v: &Value) -> f64 {
    match v {
        Value::Int(i) => *i as f64,
        Value::UInt(u) => *u as f64,
        Value::Float(f) => *f,
        _ => unreachable!("only called after is_numeric"),
    }
}

/// celpy's exact-type equality, applied recursively: `Err` wherever a
/// celtypes dunder would raise — a numeric type mismatch, or a
/// list/map compared against a different shape or length — `Ok` with
/// the structural answer otherwise. [`spec_eq`]/[`spec_ne`] catch the
/// `Err` at the top; `in` ([`strict_in_fn`]) does not.
fn strict_eq(a: &Value, b: &Value) -> Result<bool, ()> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x == y),
        (Value::UInt(x), Value::UInt(y)) => Ok(x == y),
        (Value::Float(x), Value::Float(y)) => Ok(x == y),
        (Value::Bool(x), Value::Bool(y)) => Ok(x == y),
        (Value::String(x), Value::String(y)) => Ok(x == y),
        (Value::Bytes(x), Value::Bytes(y)) => Ok(x == y),
        (Value::Null, Value::Null) => Ok(true),
        (Value::Timestamp(x), Value::Timestamp(y)) => Ok(x == y),
        (Value::Duration(x), Value::Duration(y)) => Ok(x == y),
        // `BoolType` has no `__eq__` override in celpy, so a `bool` on
        // the *left* of an `int`/`uint` resolves through the plain `int`
        // it subclasses rather than reaching the other side's
        // `@type_matched` — `true == 1`/`true == 1u` is `true`, not a
        // raise (finding 5). The reverse order (`int`/`uint` first) has
        // no arm here on purpose: it falls to the catch-all below and
        // raises, matching celpy exactly.
        (Value::Bool(x), Value::Int(y)) => Ok(i64::from(*x) == *y),
        (Value::Bool(x), Value::UInt(y)) => Ok(u64::from(*x) == *y),
        (Value::List(x), Value::List(y)) => {
            if x.len() != y.len() {
                return Ok(false);
            }
            // Scans every pair before raising (module docs, finding
            // 6): a `false` pair anywhere wins over an `Err` anywhere
            // else, matching celpy's own error-absorbing `&&` fold.
            let mut saw_error = false;
            for (xi, yi) in x.iter().zip(y.iter()) {
                match strict_eq(xi, yi) {
                    Ok(true) => {}
                    Ok(false) => return Ok(false),
                    Err(()) => saw_error = true,
                }
            }
            if saw_error { Err(()) } else { Ok(true) }
        }
        (Value::Map(x), Value::Map(y)) => {
            if x.map.len() != y.map.len() {
                return Ok(false);
            }
            // Key sets compared first, independently of any value
            // (module docs, finding 6): a differing key set decides
            // `false` on its own, the same way celpy's own dict `==`
            // never even reaches a value comparison for one.
            if x.map.keys().any(|key| !y.map.contains_key(key)) {
                return Ok(false);
            }
            let mut saw_error = false;
            for (key, value) in x.map.iter() {
                let other = y.map.get(key).expect("same key set checked above");
                match strict_eq(value, other) {
                    Ok(true) => {}
                    Ok(false) => return Ok(false),
                    Err(()) => saw_error = true,
                }
            }
            if saw_error { Err(()) } else { Ok(true) }
        }
        _ if raises_on_mismatch(a, b) => Err(()),
        _ => Ok(false),
    }
}

/// Whether celpy actually raises comparing *a* and *b* once no exact-type
/// (or `bool`-vs-`int`/`uint`) arm above matched — the distinction
/// [`strict_eq`]'s catch-all needs and its own flat `Err` used to erase
/// (finding 1): a plain Python `==` between two celpy values of
/// unrelated, *both* non-numeric, non-container types resolves through
/// the two-sided `NotImplemented` fallback to `false` with no exception
/// (a `string`/`bytes`/`timestamp`/`duration`/`bool`/`null` against a
/// different one of those). Only `int`/`uint`/`double` (`@type_matched`)
/// on either side, or a `list`/`map` against anything but `null` or its
/// own kind, actually raises.
fn raises_on_mismatch(a: &Value, b: &Value) -> bool {
    let numeric = |v: &Value| matches!(v, Value::Int(_) | Value::UInt(_) | Value::Float(_));
    if numeric(a) || numeric(b) {
        return true;
    }
    let container = |v: &Value| matches!(v, Value::List(_) | Value::Map(_));
    if container(a) || container(b) {
        return !matches!(a, Value::Null) && !matches!(b, Value::Null);
    }
    false
}

/// `==` the way celpy's own top-level override falls back once
/// [`strict_eq`] can't decide (`core/cel_eval.py`'s `_spec_eq`): equal
/// only if both operands are themselves numeric and their values match
/// — never true for two containers, which is what makes `[1, 2] == [1.0,
/// 2.0]` `false` (finding 5) rather than `cel`'s own, permissive
/// cross-numeric list equality.
fn spec_eq(a: &Value, b: &Value) -> bool {
    strict_eq(a, b)
        .unwrap_or_else(|()| is_numeric(a) && is_numeric(b) && numeric_f64(a) == numeric_f64(b))
}

fn spec_ne(a: &Value, b: &Value) -> bool {
    !spec_eq(a, b)
}

fn strict_eq_fn(Arguments(args): Arguments) -> Result<Value, ExecutionError> {
    Ok(Value::Bool(spec_eq(&args[0], &args[1])))
}

fn strict_ne_fn(Arguments(args): Arguments) -> Result<Value, ExecutionError> {
    Ok(Value::Bool(spec_ne(&args[0], &args[1])))
}

/// `in`, matching celpy's `operator_in`: `true` as soon as a strict match
/// is found; otherwise, the type error the scan hit along the way if it
/// hit one, else `false`. Unlike `==`/`!=`, there's no top-level fallback
/// here — `1 in [1.0]` and `state.x in ['a', 'b']` with `x` an int both
/// raise in celpy (finding 5), not just decide `false`. A bare `string`
/// or `bytes` operand iterates its characters or bytes the same way a
/// `for c in container` loop over a celpy `str`/`bytes` does (module
/// docs, fourth review finding 1) — `'lo' in 'hello'` is `false` (no
/// single character equals the two-character item), not an error.
fn strict_in_fn(Arguments(args): Arguments) -> Result<Value, ExecutionError> {
    let item = &args[0];
    let container = &args[1];
    let elements: Vec<Value> = match container {
        Value::List(items) => items.iter().cloned().collect(),
        Value::Map(map) => map.map.keys().map(Value::from).collect(),
        Value::String(s) => s
            .chars()
            .map(|c| Value::String(std::sync::Arc::new(c.to_string())))
            .collect(),
        Value::Bytes(b) => b.iter().map(|byte| Value::Int(i64::from(*byte))).collect(),
        _ => {
            return Err(ExecutionError::ValuesNotComparable(
                item.clone(),
                container.clone(),
            ));
        }
    };
    let mut had_error = false;
    for element in &elements {
        match strict_eq(element, item) {
            Ok(true) => return Ok(Value::Bool(true)),
            Ok(false) => {}
            Err(()) => had_error = true,
        }
    }
    if had_error {
        Err(ExecutionError::ValuesNotComparable(
            item.clone(),
            container.clone(),
        ))
    } else {
        Ok(Value::Bool(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths;
    use std::sync::Arc;

    fn eval(expr: &str) -> Result<Value, String> {
        let env = Arc::new(crate::macros::env());
        let program = env.compile(expr).unwrap();
        let mut tree = program.expression().clone();
        paths::rewrite(&mut tree);
        let mut ctx = Context::with_env(Arc::clone(&env));
        register(&mut ctx);
        crate::ordering::register(&mut ctx);
        crate::macros::register(&mut ctx);
        Value::resolve(&tree, &ctx).map_err(|e| e.to_string())
    }

    #[test]
    fn scalar_heterogeneous_equality_is_unaffected() {
        assert_eq!(eval("1 == true"), Ok(Value::Bool(false)));
        assert_eq!(eval("1 == 1.0"), Ok(Value::Bool(true)));
        assert_eq!(eval("'a' == 1"), Ok(Value::Bool(false)));
    }

    #[test]
    fn list_equality_does_not_cross_numeric_types() {
        // finding 5: celpy's own `ListType.__eq__` raises comparing an
        // `IntType` element to a `DoubleType` one, which celpy's
        // top-level override then turns into `false` (neither list is
        // itself numeric) — unlike `cel`'s own, permissive list equality.
        assert_eq!(eval("[1, 2] == [1.0, 2.0]"), Ok(Value::Bool(false)));
        assert_eq!(eval("[1, 2] != [1.0, 2.0]"), Ok(Value::Bool(true)));
    }

    #[test]
    fn map_equality_does_not_cross_numeric_types() {
        assert_eq!(eval("{'a': 1} != {'a': 1.0}"), Ok(Value::Bool(true)));
        assert_eq!(eval("{'a': 1} == {'a': 1.0}"), Ok(Value::Bool(false)));
    }

    #[test]
    fn in_raises_on_a_numeric_type_mismatch() {
        assert!(eval("1 in [1.0]").is_err());
    }

    #[test]
    fn in_raises_on_a_string_int_mismatch() {
        assert!(eval("1 in ['a', 'b']").is_err());
    }

    #[test]
    fn in_still_finds_a_same_type_match() {
        assert_eq!(eval("1 in [1, 2, 3]"), Ok(Value::Bool(true)));
        assert_eq!(eval("'a' in ['a', 'b']"), Ok(Value::Bool(true)));
        assert_eq!(eval("1 in [2, 3]"), Ok(Value::Bool(false)));
    }

    #[test]
    fn in_decides_false_on_a_benign_mismatch_instead_of_raising() {
        // finding 1: `'b' in ['a', None]` is `false` in celpy (a `string`
        // against `null` resolves via the two-sided `NotImplemented`
        // fallback, no exception) — a prior fix made `strict_in_fn` raise
        // on *every* type mismatch, regressing this.
        assert_eq!(eval("'b' in ['a', null]"), Ok(Value::Bool(false)));
        assert_eq!(eval("true in ['a', 'b']"), Ok(Value::Bool(false)));
        assert_eq!(eval("null in ['a', 'b']"), Ok(Value::Bool(false)));
        // `null` against `int`/`uint`/`double` raises either way — `int`'s
        // own `@type_matched` has no exemption for `None`.
        assert!(eval("null in [1, 2]").is_err());
    }

    #[test]
    fn bool_on_the_left_of_int_or_uint_compares_by_value() {
        // finding 5: celpy's `BoolType` has no `__eq__` override, so
        // `true == 1`/`true == 1u` resolve through the plain `int` it
        // subclasses rather than raising — unlike the reverse order.
        assert_eq!(eval("true == 1"), Ok(Value::Bool(true)));
        assert_eq!(eval("true == 1u"), Ok(Value::Bool(true)));
        assert_eq!(eval("false == 0"), Ok(Value::Bool(true)));
        assert_eq!(eval("1 == true"), Ok(Value::Bool(false)));
        assert_eq!(eval("1u == true"), Ok(Value::Bool(false)));
        assert_eq!(eval("true == 1.0"), Ok(Value::Bool(false)));
        assert_eq!(eval("1 in [true]"), Ok(Value::Bool(true)));
        assert!(eval("true in [1]").is_err());
    }

    #[test]
    fn in_over_a_string_iterates_characters() {
        // fourth review finding 1: `cel`'s own `@in` has no indexer for
        // a bare string; celpy's `operator_in` loops over its
        // characters.
        assert_eq!(eval("'h' in 'hello'"), Ok(Value::Bool(true)));
        assert_eq!(eval("'lo' in 'hello'"), Ok(Value::Bool(false)));
        assert!(eval("1 in 'hello'").is_err());
    }

    #[test]
    fn in_over_bytes_iterates_ints() {
        assert_eq!(eval("97 in b'abc'"), Ok(Value::Bool(true)));
        assert_eq!(eval("200 in b'abc'"), Ok(Value::Bool(false)));
    }

    #[test]
    fn nested_list_equality_does_not_depend_on_which_pair_raises_first() {
        // fourth review finding 6: the second pair (`2 == 3`) decides
        // `false` on its own; a version that stopped at the first
        // raising pair (`1 == 1.0`) would raise instead.
        assert_eq!(eval("[1, 2] in [[1.0, 3]]"), Ok(Value::Bool(false)));
    }

    #[test]
    fn nested_map_equality_checks_key_sets_before_any_value() {
        // fourth review finding 6: a mismatched key set decides `false`
        // without comparing any value, so this can never raise
        // regardless of a `HashMap`'s own iteration order.
        for _ in 0..20 {
            assert_eq!(
                eval("{'a': 1, 'b': 2} in [{'a': 1.0, 'c': 2}]"),
                Ok(Value::Bool(false))
            );
        }
    }

    #[test]
    fn nested_map_equality_does_not_depend_on_which_pair_raises_first() {
        for _ in 0..20 {
            assert_eq!(
                eval("{'a': 1, 'b': 2} in [{'a': 1.0, 'b': 3}]"),
                Ok(Value::Bool(false))
            );
        }
    }
}
