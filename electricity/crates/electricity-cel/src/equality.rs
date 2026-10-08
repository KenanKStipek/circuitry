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
//! [`strict_in_fn`] reproduces that directly.
//!
//! [`paths::rewrite`](crate::paths::rewrite) substitutes this module's
//! functions for `cel`'s own `_==_`/`_!=_`/`@in`, the same way it does
//! for [`crate::ordering`]'s.
//!
//! Not reproduced: celpy's own asymmetry for `bool` (`BoolType` is a
//! plain `int` subclass with no equality override, so which side's
//! dunder Python's dispatch tries first can change the outcome — `1 in
//! [true]` is `true`, `true in [1]` raises). This module treats `bool`
//! like every other exact-type-only value instead, a documented,
//! narrower divergence (`lib.rs`).

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
        (Value::List(x), Value::List(y)) => {
            if x.len() != y.len() {
                return Ok(false);
            }
            for (xi, yi) in x.iter().zip(y.iter()) {
                if !strict_eq(xi, yi)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Value::Map(x), Value::Map(y)) => {
            if x.map.len() != y.map.len() {
                return Ok(false);
            }
            for (key, value) in x.map.iter() {
                match y.map.get(key) {
                    Some(other) if strict_eq(value, other)? => {}
                    _ => return Ok(false),
                }
            }
            Ok(true)
        }
        _ => Err(()),
    }
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
/// raise in celpy (finding 5), not just decide `false`.
fn strict_in_fn(Arguments(args): Arguments) -> Result<Value, ExecutionError> {
    let item = &args[0];
    let container = &args[1];
    let elements: Vec<Value> = match container {
        Value::List(items) => items.iter().cloned().collect(),
        Value::Map(map) => map.map.keys().map(Value::from).collect(),
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
        let env = Arc::new(cel::Env::stdlib());
        let program = env.compile(expr).unwrap();
        let mut tree = program.expression().clone();
        paths::rewrite(&mut tree);
        let mut ctx = Context::with_env(Arc::clone(&env));
        register(&mut ctx);
        crate::ordering::register(&mut ctx);
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
}
