//! Python-style list indexing, replacing `cel`'s own `_[_]` (DESIGN.md
//! §7.2, issue #379 review finding 3): celpy's `_[_]` on a `list` is
//! Python's own `operator.getitem` (`evaluation.py`'s `"_[_]":
//! operator.getitem`), which accepts a negative index (wraps from the
//! end), tolerates `bool` (an `int` subclass — `True`/`False` is `1`/
//! `0`), and rejects a `double` outright (`float` implements no
//! `__index__`, so Python's own list indexing never even tries it) —
//! `cel`'s own `list.rs` instead rejects a negative index and accepts a
//! whole-number `double`. Map indexing is unaffected: celpy's own
//! `MapType.__getitem__`/`valid_key_type` already restricts keys to
//! exactly `int`/`uint`/`bool`/`string`, the same set `cel`'s own
//! [`cel::objects::Map::get`] accepts (with the same implicit int/uint
//! cross-conversion), so [`python_index`] just calls that unchanged.
//!
//! [`paths::rewrite`](crate::paths::rewrite) substitutes [`strict_index_fn`]
//! for every plain `_[_]`, the same way it does for [`crate::ordering`]'s
//! and [`crate::equality`]'s functions. `has()`'s own safe-navigation
//! rewrite ([`paths::to_optional`](crate::paths::to_optional)) needs the
//! *same* Python semantics for the index it guards — `has(state.l[-1].x)`
//! must see the same successful lookup a plain `state.l[-1].x` would, and
//! `has(state.l[0.0].x)` must see the same failure — so [`opt_index`]
//! builds a call to [`strict_opt_index_fn`], a second, optional-returning
//! entry point into the same [`python_index`], instead of `cel`'s native
//! `_[?_]`. [`strict_opt_index_fn`] returns a [`cel::objects::OptionalValue`],
//! not `cel`'s own internal optional representation (there is no public
//! way to construct one directly) — but the public bridge
//! `impl TryFrom<Value> for Box<dyn Val>` (`objects.rs`) converts
//! `Value::Opaque` wrapping one into the engine's own internal optional
//! the moment it's used as an operand to a native `.?`/`hasValue()` call,
//! and the symmetric `impl TryFrom<&dyn Val> for Value` converts back the
//! same way when a native step's result is passed to *this* module's own
//! function — so a chain mixing native `.?` steps with this module's
//! `[?]` steps, in either order, composes correctly.

use std::sync::Arc;

use cel::common::ast::{CallExpr, Expr, IdedExpr, operators};
use cel::extractors::Arguments;
use cel::objects::{Key, OptionalValue};
use cel::{Context, ExecutionError, Value};

const STRICT_INDEX: &str = "__electricity_python_index";
const STRICT_OPT_INDEX: &str = "__electricity_python_opt_index";

/// The registered function name standing in for *func_name* if it is
/// `cel`'s own plain index operator, else `None`. The optional index has
/// no entry here on purpose: [`paths::to_optional`](crate::paths::to_optional)
/// builds a call to [`STRICT_OPT_INDEX`] directly via [`opt_index`]
/// rather than going through a `rewrite`-driven substitution.
pub fn strict_function_name(func_name: &str) -> Option<&'static str> {
    match func_name {
        operators::INDEX => Some(STRICT_INDEX),
        _ => None,
    }
}

/// Registers the functions this module's names stand in for.
pub fn register(ctx: &mut Context) {
    ctx.add_function(STRICT_INDEX, strict_index_fn)
        .expect("name not already declared");
    ctx.add_function(STRICT_OPT_INDEX, strict_opt_index_fn)
        .expect("name not already declared");
}

/// *operand* `[?` *key* `]`, Python list semantics included — the call
/// [`paths::to_optional`](crate::paths::to_optional) builds in place of
/// `cel`'s own `_[?_]`.
pub fn opt_index(operand: Expr, key: Expr) -> Expr {
    Expr::Call(CallExpr {
        func_name: STRICT_OPT_INDEX.to_string(),
        target: None,
        args: vec![
            IdedExpr {
                id: 0,
                expr: operand,
            },
            IdedExpr { id: 0, expr: key },
        ],
    })
}

/// celpy's list `getitem`, and `cel`'s own (celpy-equivalent) map
/// `get` — `Err(())` for an index with no CEL [`Key`] counterpart, out
/// of range, or a container that isn't a `list`/`map` at all.
fn python_index(container: &Value, index: &Value) -> Result<Value, ()> {
    match container {
        Value::List(items) => {
            let i: i64 = match index {
                Value::Int(n) => *n,
                Value::UInt(n) => i64::try_from(*n).map_err(|_| ())?,
                Value::Bool(b) => i64::from(*b),
                // A `double` (whole-number or not): Python's `list`
                // indexing requires `__index__`, which `float` has no
                // more than any other non-integral type does.
                _ => return Err(()),
            };
            let len = items.len() as i64;
            let idx = if i < 0 { i + len } else { i };
            if idx < 0 || idx >= len {
                return Err(());
            }
            Ok(items[idx as usize].clone())
        }
        Value::Map(map) => {
            let key: Key = index.clone().try_into().map_err(|_| ())?;
            map.get(&key).cloned().ok_or(())
        }
        _ => Err(()),
    }
}

fn strict_index_fn(Arguments(args): Arguments) -> Result<Value, ExecutionError> {
    python_index(&args[0], &args[1])
        .map_err(|()| ExecutionError::UnsupportedIndex(args[1].clone(), args[0].clone()))
}

/// Unwraps *v* if it's an optional from an earlier `.?`/`[?]` step —
/// native or this module's own, both represented the same way once
/// resolved (`objects.rs`'s `impl TryFrom<&dyn Val> for Value`).
/// `Some(None)` for an empty one (the caller should short-circuit the
/// same way native `_[?_]`/`_?._` does), `Some(Some(_))` for a present
/// one, `None` if *v* isn't optional at all — the chain's root, or any
/// plain value.
fn unwrap_our_optional(v: &Value) -> Option<Option<Value>> {
    let Value::Opaque(o) = v else {
        return None;
    };
    o.downcast_ref::<OptionalValue>()
        .map(|opt| opt.value().cloned())
}

fn none_optional() -> Value {
    Value::Opaque(Arc::new(OptionalValue::none()))
}

fn strict_opt_index_fn(Arguments(args): Arguments) -> Result<Value, ExecutionError> {
    let container = match unwrap_our_optional(&args[0]) {
        Some(None) => return Ok(none_optional()),
        Some(Some(v)) => v,
        None => args[0].clone(),
    };
    match python_index(&container, &args[1]) {
        Ok(v) => Ok(Value::Opaque(Arc::new(OptionalValue::of(v)))),
        Err(()) => Ok(none_optional()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{equality, ordering, paths};
    use std::sync::Arc as StdArc;

    fn eval(expr: &str) -> Result<Value, String> {
        let env = StdArc::new(cel::Env::stdlib());
        let program = env.compile(expr).unwrap();
        let mut tree = program.expression().clone();
        paths::rewrite(&mut tree);
        let mut ctx = Context::with_env(StdArc::clone(&env));
        register(&mut ctx);
        ordering::register(&mut ctx);
        equality::register(&mut ctx);
        Value::resolve(&tree, &ctx).map_err(|e| e.to_string())
    }

    #[test]
    fn negative_index_wraps_from_the_end() {
        assert_eq!(eval("[10, 20, 30][-1]"), Ok(Value::Int(30)));
        assert_eq!(eval("[10, 20, 30][-3]"), Ok(Value::Int(10)));
    }

    #[test]
    fn negative_index_out_of_range_raises() {
        assert!(eval("[10, 20, 30][-4]").is_err());
    }

    #[test]
    fn bool_index_is_zero_or_one() {
        assert_eq!(eval("[10, 20][true]"), Ok(Value::Int(20)));
        assert_eq!(eval("[10, 20][false]"), Ok(Value::Int(10)));
    }

    #[test]
    fn double_index_raises() {
        assert!(eval("[10, 20][0.0]").is_err());
    }

    #[test]
    fn positive_index_still_works() {
        assert_eq!(eval("[10, 20, 30][1]"), Ok(Value::Int(20)));
    }

    #[test]
    fn map_indexing_is_unaffected() {
        assert_eq!(eval("{'a': 1, 'b': 2}['a']"), Ok(Value::Int(1)));
        assert!(eval("{'a': 1}['missing']").is_err());
    }

    #[test]
    fn has_over_a_negative_index_matches_the_plain_lookup() {
        assert_eq!(eval("has([{'x': 1}][-1].x)"), Ok(Value::Bool(true)));
    }

    #[test]
    fn has_over_a_double_index_is_false_not_an_error() {
        assert_eq!(eval("has([{'x': 1}][0.0].x)"), Ok(Value::Bool(false)));
    }
}
