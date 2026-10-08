//! Strict-type ordering: the one place `cel`'s own semantics diverge from
//! `celpy`'s for `<`, `<=`, `>` and `>=` (DESIGN.md §7.2).
//!
//! Only `celtypes.IntType` decorates `<`, `<=`, `>`, `>=` with
//! `@type_matched`, which requires the *exact* same type on both sides —
//! not just "numeric" (`celtypes.py`): `state.n < 1.5` (`n` an `int`)
//! raises, and so does `state.n < true`. `UintType`/`DoubleType` never
//! override ordering at all, leaving it to the plain `int`/`float`
//! comparison they subclass — which crosses numeric types freely, and
//! (since `BoolType` is a plain `int` subclass too) tolerates `bool` the
//! same way: `1.5 > true` and `true < 1.5` both hold. So the restriction
//! is **asymmetric on the left operand as written**, and `int` is the
//! only side that enforces it.
//!
//! `cel`'s own comparer for each type (`common/types/*.rs`) is close but
//! not identical: a `NaN` `double` and a cross-type comparison it
//! doesn't itself support both raise
//! ([`cel::common::traits::Comparer::compare`]'s own doc comment), where
//! celpy's plain `float`/`int` comparison instead *resolves* a `NaN`
//! comparison to `false` — never raising — the same way plain Python
//! does. `cel`'s comparer also has no notion of `bool` ordering at all
//! (`common/types/bool.rs`'s `compare` only accepts another `bool`).
//! [`strict_cmp`] is accordingly a small, self-contained comparison
//! rather than a call into `cel`'s own: it adds the `int`-only asymmetry,
//! `bool`-as-a-number tolerance and `NaN`-resolves-false behaviour celpy
//! has, while still rejecting what neither `cel` nor celpy ever ordered
//! (`null`, a `list`/`map`, two different non-numeric types).
//!
//! `<`/`<=`/`>`/`>=` are hardcoded special cases inside `cel`'s own
//! `Value::resolve_val` (`objects.rs`), dispatched by `call.func_name`
//! before any registered function gets a look — there is no override
//! hook for them the way there would be for an ordinary function. So
//! [`paths::rewrite`](crate::paths::rewrite) renames each occurrence to
//! one of the names below, which *are* ordinary registered functions
//! (`Context::add_function`, [`register`]), free to apply celpy's rules
//! before `cel`'s own dispatch ever runs.

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

fn not_comparable(a: &Value, b: &Value) -> ExecutionError {
    ExecutionError::ValuesNotComparable(a.clone(), b.clone())
}

/// `u` against `i`, the way `celtypes.UintType`'s own (undecorated,
/// plain-`int`-inherited) comparison would: if `u` doesn't fit in an
/// `i64`, it's greater than every possible `i64` — `cel`'s own
/// `common/types/uint.rs` resolves the same way.
fn uint_cmp_int(u: u64, i: i64) -> Ordering {
    i64::try_from(u)
        .map(|u_as_i| u_as_i.cmp(&i))
        .unwrap_or(Ordering::Greater)
}

/// `f` against *i*/*u*, cast to `f64` — matching `cel`'s own
/// `common/types/double.rs`, and celpy's plain `float` comparison. `None`
/// for a `NaN` operand, resolved to "false for every operator" by
/// [`strict_cmp`]'s caller, not an error.
fn float_cmp(f: f64, other: f64) -> Option<Ordering> {
    f.partial_cmp(&other)
}

/// *a op b*'s ordering, or `None` if a `NaN` double participated (no
/// ordering holds, but unlike an incomparable pair this isn't an error —
/// every comparison operator simply answers `false`, matching celpy's
/// plain `float` behaviour, `cel`'s own `common/types/double.rs`
/// raising instead).
///
/// # Errors
///
/// `null`, a `list`/`map`/`struct`, or two genuinely incomparable types
/// (a `string` against a `bytes`, say) all raise — `cel` and celpy agree
/// on that much. An `int` against any other numeric type also raises
/// (celpy's `IntType.__lt__` et al., `@type_matched`, requiring the
/// *exact* same type): that restriction applies only when `int` is the
/// left operand, matching celpy's asymmetry (`ordering.rs`'s module doc).
fn strict_cmp(a: &Value, b: &Value) -> Result<Option<Ordering>, ExecutionError> {
    use Value::*;
    match (a, b) {
        (Null, _) | (_, Null) => Err(not_comparable(a, b)),

        // `int` insists on an exact type match; every other numeric type,
        // and `bool`, is tolerant in both directions.
        (Int(x), Int(y)) => Ok(Some(x.cmp(y))),
        (Int(_), _) => Err(not_comparable(a, b)),

        (UInt(x), UInt(y)) => Ok(Some(x.cmp(y))),
        (UInt(x), Int(y)) => Ok(Some(uint_cmp_int(*x, *y))),
        (UInt(x), Float(y)) => Ok(float_cmp(*x as f64, *y)),
        (UInt(x), Bool(y)) => Ok(Some(uint_cmp_int(*x, *y as i64))),

        (Float(x), Float(y)) => Ok(float_cmp(*x, *y)),
        (Float(x), Int(y)) => Ok(float_cmp(*x, *y as f64)),
        (Float(x), UInt(y)) => Ok(float_cmp(*x, *y as f64)),
        (Float(x), Bool(y)) => Ok(float_cmp(*x, *y as i64 as f64)),

        (Bool(x), Bool(y)) => Ok(Some(x.cmp(y))),
        (Bool(x), Int(y)) => Ok(Some((*x as i64).cmp(y))),
        (Bool(x), UInt(y)) => Ok(Some(uint_cmp_int(*y, *x as i64).reverse())),
        (Bool(x), Float(y)) => Ok(float_cmp(*x as i64 as f64, *y)),

        (String(x), String(y)) => Ok(Some(x.cmp(y))),
        (Bytes(x), Bytes(y)) => Ok(Some(x.cmp(y))),
        (Timestamp(x), Timestamp(y)) => Ok(Some(x.cmp(y))),
        (Duration(x), Duration(y)) => Ok(Some(x.cmp(y))),

        _ => Err(not_comparable(a, b)),
    }
}

fn strict_lt(Arguments(args): Arguments) -> Result<Value, ExecutionError> {
    Ok(Value::Bool(
        strict_cmp(&args[0], &args[1])? == Some(Ordering::Less),
    ))
}

fn strict_le(Arguments(args): Arguments) -> Result<Value, ExecutionError> {
    Ok(Value::Bool(matches!(
        strict_cmp(&args[0], &args[1])?,
        Some(Ordering::Less) | Some(Ordering::Equal)
    )))
}

fn strict_gt(Arguments(args): Arguments) -> Result<Value, ExecutionError> {
    Ok(Value::Bool(
        strict_cmp(&args[0], &args[1])? == Some(Ordering::Greater),
    ))
}

fn strict_ge(Arguments(args): Arguments) -> Result<Value, ExecutionError> {
    Ok(Value::Bool(matches!(
        strict_cmp(&args[0], &args[1])?,
        Some(Ordering::Greater) | Some(Ordering::Equal)
    )))
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
    fn int_against_bool_raises() {
        assert!(eval("2 > true").is_err());
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

    #[test]
    fn bytes_ordering_works() {
        // finding 2: a previous version of this fell back to
        // `Value::partial_cmp`, which has no `Bytes` arm.
        assert_eq!(eval("b'a' < b'b'"), Ok(Value::Bool(true)));
        assert_eq!(eval("b'ab' <= b'ab'"), Ok(Value::Bool(true)));
    }

    #[test]
    fn null_ordering_raises() {
        // finding 2: `Value::partial_cmp` returned `Some(Equal)` for
        // `(Null, Null)`, making `null <= null` `true` where celpy (and
        // plain `cel`) raise.
        assert!(eval("null <= null").is_err());
        assert!(eval("null < 1").is_err());
        assert!(eval("1 < null").is_err());
    }

    #[test]
    fn bool_against_a_number_is_tolerant() {
        // finding 8: `BoolType` is a plain `int` subclass with no
        // ordering override in celpy, so it behaves like one on either
        // side, unlike `int` itself.
        assert_eq!(eval("true < 2"), Ok(Value::Bool(true)));
        assert_eq!(eval("1.5 > true"), Ok(Value::Bool(true)));
        assert_eq!(eval("true < 1.5"), Ok(Value::Bool(true)));
        assert_eq!(eval("1u > true"), Ok(Value::Bool(false)));
        assert_eq!(eval("true < 1u"), Ok(Value::Bool(false)));
    }

    #[test]
    fn nan_resolves_every_comparison_to_false_without_raising() {
        // finding 8: celpy's plain `float` comparison never raises for a
        // `NaN` operand; `cel`'s own comparer does.
        let nan = f64::NAN;
        let env = Arc::new(cel::Env::stdlib());
        let program = env.compile("state.n < 1.0").unwrap();
        let mut tree = program.expression().clone();
        paths::rewrite(&mut tree);
        let mut ctx = Context::with_env(Arc::clone(&env));
        register(&mut ctx);
        ctx.add_variable_from_value("state", {
            let mut map = std::collections::HashMap::new();
            map.insert(
                cel::objects::Key::String(std::sync::Arc::new("n".to_string())),
                Value::Float(nan),
            );
            Value::Map(cel::objects::Map {
                map: std::sync::Arc::new(map),
            })
        });
        assert_eq!(Value::resolve(&tree, &ctx), Ok(Value::Bool(false)));
    }
}
