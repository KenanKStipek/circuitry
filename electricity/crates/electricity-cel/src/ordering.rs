//! Strict-type ordering: the one place `cel`'s own semantics diverge from
//! `celpy`'s for an operator Circuitry's own `cel_eval.py` doesn't
//! override itself (DESIGN.md §7.2).
//!
//! Only `celtypes.IntType` decorates `<`, `<=`, `>`, `>=` with
//! `@type_matched` (`celtypes.py`); `UintType`/`DoubleType` decorate only
//! `==`/`!=`, leaving their own ordering to the plain `int`/`float`
//! comparison they subclass, which crosses numeric types freely. So the
//! restriction is **asymmetric**, keyed on the *left* operand as
//! written: `state.n < 1.5` (`n` an `int`) raises, but `1.5 > state.n`
//! does not — `DoubleType.__gt__` was never overridden, so it's plain
//! `float.__gt__` against an `int`, which Python already allows. `cel`'s
//! own `impl PartialOrd for Value` (`objects.rs`) compares across
//! `Int`/`UInt`/`Float` unconditionally instead, in both directions.
//!
//! `<`/`<=`/`>`/`>=` are hardcoded special cases inside `cel`'s own
//! `Value::resolve_val` (`objects.rs`), dispatched by `call.func_name`
//! before any registered function gets a look — there is no override
//! hook for them the way there would be for an ordinary function. So
//! [`paths::rewrite`](crate::paths::rewrite) renames each occurrence to
//! one of the names below, which *are* ordinary registered functions
//! (`Context::add_function`, [`register`]), free to reject a cross-type
//! comparison before ever calling `cel`'s own, permissive
//! `PartialOrd for Value`.

use std::cmp::Ordering;

use cel::common::ast::operators;
use cel::extractors::Arguments;
use cel::{Context, ExecutionError, Value};

const STRICT_LT: &str = "__electricity_strict_lt";
const STRICT_LE: &str = "__electricity_strict_le";
const STRICT_GT: &str = "__electricity_strict_gt";
const STRICT_GE: &str = "__electricity_strict_ge";

/// The registered function name standing in for *func_name* if it is one
/// of `cel`'s own ordering operators, else `None` — anything else
/// (`_==_`, `_+_`, a plain function call) is left alone.
pub fn strict_function_name(func_name: &str) -> Option<&'static str> {
    match func_name {
        operators::LESS => Some(STRICT_LT),
        operators::LESS_EQUALS => Some(STRICT_LE),
        operators::GREATER => Some(STRICT_GT),
        operators::GREATER_EQUALS => Some(STRICT_GE),
        _ => None,
    }
}

/// Registers the functions [`strict_function_name`] renames operators to.
pub fn register(ctx: &mut Context) {
    ctx.add_function(STRICT_LT, strict_lt)
        .expect("name not already declared");
    ctx.add_function(STRICT_LE, strict_le)
        .expect("name not already declared");
    ctx.add_function(STRICT_GT, strict_gt)
        .expect("name not already declared");
    ctx.add_function(STRICT_GE, strict_ge)
        .expect("name not already declared");
}

fn is_numeric(v: &Value) -> bool {
    matches!(v, Value::Int(_) | Value::UInt(_) | Value::Float(_))
}

/// *a op b*'s ordering, matching celpy's asymmetric rule: raise only when
/// *a* (the left operand as written) is an `Int` being compared against
/// a *different* numeric type. An `Int` against an `Int`, or anything
/// with a `UInt`/`Float` on the left, is left to `cel`'s own, permissive
/// `PartialOrd for Value` — that's what celpy's own `UintType`/
/// `DoubleType` (neither overrides ordering) end up doing too.
fn strict_cmp(a: &Value, b: &Value) -> Result<Ordering, ExecutionError> {
    if matches!(a, Value::Int(_)) && is_numeric(b) && !matches!(b, Value::Int(_)) {
        return Err(ExecutionError::ValuesNotComparable(a.clone(), b.clone()));
    }
    a.partial_cmp(b)
        .ok_or_else(|| ExecutionError::ValuesNotComparable(a.clone(), b.clone()))
}

fn strict_lt(Arguments(args): Arguments) -> Result<Value, ExecutionError> {
    Ok(Value::Bool(
        strict_cmp(&args[0], &args[1])? == Ordering::Less,
    ))
}

fn strict_le(Arguments(args): Arguments) -> Result<Value, ExecutionError> {
    Ok(Value::Bool(
        strict_cmp(&args[0], &args[1])? != Ordering::Greater,
    ))
}

fn strict_gt(Arguments(args): Arguments) -> Result<Value, ExecutionError> {
    Ok(Value::Bool(
        strict_cmp(&args[0], &args[1])? == Ordering::Greater,
    ))
}

fn strict_ge(Arguments(args): Arguments) -> Result<Value, ExecutionError> {
    Ok(Value::Bool(
        strict_cmp(&args[0], &args[1])? != Ordering::Less,
    ))
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
        paths::rewrite(&mut tree, &[]);
        let mut ctx = Context::with_env(Arc::clone(&env));
        register(&mut ctx);
        Value::resolve(&tree, &ctx).map_err(|e| e.to_string())
    }

    #[test]
    fn same_type_ordering_still_works() {
        assert_eq!(eval("1 < 2"), Ok(Value::Bool(true)));
        assert_eq!(eval("2.0 <= 2.0"), Ok(Value::Bool(true)));
    }

    #[test]
    fn int_against_a_different_numeric_type_on_the_left_raises() {
        assert!(eval("1 < 1.5").is_err());
        assert!(eval("1 > 1.5").is_err());
        assert!(eval("2 < 1u").is_err());
    }

    #[test]
    fn uint_or_double_on_the_left_is_unrestricted() {
        // celpy's `UintType`/`DoubleType` never override ordering, so
        // crossing numeric types is fine when *they* are the left
        // operand — only `IntType`'s own `@type_matched` restricts it.
        assert_eq!(eval("1.5 > 1"), Ok(Value::Bool(true)));
        assert_eq!(eval("1u < 2"), Ok(Value::Bool(true)));
    }

    #[test]
    fn string_ordering_is_unaffected() {
        assert_eq!(eval("'a' < 'b'"), Ok(Value::Bool(true)));
    }
}
