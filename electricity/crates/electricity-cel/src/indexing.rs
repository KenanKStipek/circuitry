//! Python-style indexing and containment, replacing `cel`'s own `_[_]`
//! (DESIGN.md §7.2, issue #379 review finding 3, and the fourth
//! review's finding 1): celpy's `_[_]` on a `list` is Python's own
//! `operator.getitem` (`evaluation.py`'s `"_[_]": operator.getitem`),
//! which accepts a negative index (wraps from the end), tolerates
//! `bool` (an `int` subclass — `True`/`False` is `1`/`0`), and rejects
//! a `double` outright (`float` implements no `__index__`, so Python's
//! own list indexing never even tries it) — `cel`'s own `list.rs`
//! instead rejects a negative index and accepts a whole-number
//! `double`. A `string`/`bytes` operand follows the same index rule,
//! indexing by Unicode character (celpy's `StringType` is a `str`
//! subclass) or by byte (`BytesType` a `bytes` subclass, each element
//! an `int`) rather than raising outright the way `cel`'s own
//! `_[_]` does for either — `cel` implements no indexer at all for a
//! bare string or byte string (only `map.rs`, `list.rs` and
//! `struct.rs` implement `as_indexer`).
//!
//! Map indexing needs its own strict key match, not `cel`'s own
//! cross-converting [`cel::objects::Map::get`]: celpy's own
//! `MapType.__getitem__` goes through a plain Python dict lookup, whose
//! `IntType`/`UintType.__eq__` are `@type_matched` — raising for a key
//! of the *other* integer type rather than silently finding it the way
//! `cel`'s own map indexing's implicit int/uint cross-conversion does.
//! [`python_index`]'s `Value::Map` arm looks a key up through the
//! underlying `HashMap` directly ([`cel::objects::Map`]'s own `map`
//! field, already public — `equality.rs`'s own map-equality arm reads
//! it the same way), bypassing [`Map::get`](cel::objects::Map::get)
//! specifically to avoid that cross-conversion. A `bool` key against an
//! `int`/`uint` lookup (or the reverse) is a narrower, asymmetric
//! divergence this doesn't close — see [`python_index`]'s own doc
//! comment.
//!
//! [`paths::rewrite`](crate::paths::rewrite) substitutes [`strict_index_fn`]
//! for every plain `_[_]`, the same way it does for [`crate::ordering`]'s
//! and [`crate::equality`]'s functions. `has()` no longer needs an
//! optional-returning entry point into this module at all
//! ([`crate::macros`]'s own rewrite covers an index that fails to
//! evaluate — in a key or in the lookup itself — without this module's
//! help).

use cel::common::ast::operators;
use cel::extractors::Arguments;
use cel::objects::Key;
use cel::{Context, ExecutionError, Value};

const STRICT_INDEX: &str = "__electricity_python_index";

/// The registered function name standing in for *func_name* if it is
/// `cel`'s own plain index operator, else `None`.
pub fn strict_function_name(func_name: &str) -> Option<&'static str> {
    match func_name {
        operators::INDEX => Some(STRICT_INDEX),
        _ => None,
    }
}

/// Registers the function this module's name stands in for.
pub fn register(ctx: &mut Context) {
    ctx.add_function(STRICT_INDEX, strict_index_fn)
        .expect("name not already declared");
}

/// The index Python's own `list`/`str`/`bytes` `__getitem__` would use
/// for a sequence of length *len*: a negative value wraps from the end,
/// a `bool` is `0`/`1`, anything else that isn't a whole `int`/`uint`
/// (a `double`, even a whole-number one — `float` implements no
/// `__index__`) is rejected, and so is one that's still out of range
/// after wrapping.
fn python_sequence_index(index: &Value, len: usize) -> Result<usize, ()> {
    let i: i64 = match index {
        Value::Int(n) => *n,
        Value::UInt(n) => i64::try_from(*n).map_err(|_| ())?,
        Value::Bool(b) => i64::from(*b),
        _ => return Err(()),
    };
    let len = len as i64;
    let idx = if i < 0 { i + len } else { i };
    if idx < 0 || idx >= len {
        return Err(());
    }
    Ok(idx as usize)
}

/// celpy's `list`/`str`/`bytes.__getitem__`, and a strict (non-
/// cross-converting) map lookup — `Err(())` for an index with no CEL
/// [`Key`] counterpart, out of range, or a container that isn't a
/// `list`/`map`/`string`/`bytes` at all.
fn python_index(container: &Value, index: &Value) -> Result<Value, ()> {
    match container {
        Value::List(items) => {
            let idx = python_sequence_index(index, items.len())?;
            Ok(items[idx].clone())
        }
        Value::String(s) => {
            // Python indexes a `str` by Unicode code point, not by byte
            // — `s.chars()` is the Rust equivalent.
            let chars: Vec<char> = s.chars().collect();
            let idx = python_sequence_index(index, chars.len())?;
            Ok(Value::String(std::sync::Arc::new(chars[idx].to_string())))
        }
        Value::Bytes(b) => {
            // Python's `bytes.__getitem__` with a plain (non-slice)
            // index returns the byte as an `int`, not a length-1
            // `bytes`.
            let idx = python_sequence_index(index, b.len())?;
            Ok(Value::Int(i64::from(b[idx])))
        }
        Value::Map(map) => {
            // `cel::objects::Map::get` cross-converts an int/uint key
            // the way `cel`'s native map indexing always has; celpy's
            // own `IntType`/`UintType.__eq__` is `@type_matched` and
            // raises for the other one instead of finding it — this
            // looks the key up directly through the underlying
            // `HashMap`, matching celpy's own strict type match. A
            // `bool` key is a narrower, asymmetric case this doesn't
            // reproduce either way: celpy's own `BoolType`, lacking an
            // `__eq__` override, lets an `int` *probe* find a `bool`
            // *stored* key (`BoolType.__eq__` resolves through the
            // plain `int` it subclasses), but not the reverse (an
            // `int`-keyed entry has no overridden `__eq__` to fall back
            // through for a `bool` probe, so `IntType`'s own
            // `@type_matched` raises) — a correct implementation would
            // have to know which side was the lookup key and which was
            // already in the map, not just compare the two. Rare enough
            // (fourth review finding 5) that this crate pins the
            // current behaviour (`map.rs` test
            // `bool_key_against_an_int_entry_is_a_known_divergence`)
            // rather than reproducing it.
            let key: Key = index.clone().try_into().map_err(|_| ())?;
            map.map.get(&key).cloned().ok_or(())
        }
        _ => Err(()),
    }
}

fn strict_index_fn(Arguments(args): Arguments) -> Result<Value, ExecutionError> {
    python_index(&args[0], &args[1])
        .map_err(|()| ExecutionError::UnsupportedIndex(args[1].clone(), args[0].clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{equality, ordering, paths};
    use std::sync::Arc as StdArc;

    fn eval(expr: &str) -> Result<Value, String> {
        let env = StdArc::new(crate::macros::env());
        let program = env.compile(expr).unwrap();
        let mut tree = program.expression().clone();
        paths::rewrite(&mut tree);
        let mut ctx = Context::with_env(StdArc::clone(&env));
        register(&mut ctx);
        ordering::register(&mut ctx);
        equality::register(&mut ctx);
        crate::macros::register(&mut ctx);
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
    fn uint_key_against_an_int_entry_raises() {
        // fourth review finding 1/5: `cel`'s own map indexing
        // cross-converts int/uint keys; celpy's own `IntType.__eq__`
        // raises instead of finding the entry.
        assert!(eval("{1: 'a'}[1u]").is_err());
        assert!(eval("{1u: 'a'}[1]").is_err());
    }

    #[test]
    fn bool_key_against_an_int_entry_is_a_known_divergence() {
        // fourth review finding 5: `cel`'s own map indexing treats a
        // `bool` key as entirely distinct from an `int`/`uint` one in
        // both directions, so this always raises on a cross-type
        // lookup. celpy's own asymmetry means that's only a real
        // divergence one way — an `int`/`uint` probe *finds* a
        // `bool`-keyed entry in celpy (confirmed against the real
        // evaluator: `state.m[1] == 'a'` with `{m: {true: 'a'}}` is
        // `true`), so this crate's raise here is pinned as a known
        // divergence (`python_index`'s own doc comment). The reverse
        // (a `bool` probe against an `int`/`uint`-keyed entry) raises in
        // celpy too, so that assertion isn't a divergence at all — both
        // are asserted here for the one behavior (always raise) that
        // makes this crate's own rule simple to state.
        assert!(eval("{true: 'a'}[1]").is_err());
        assert!(eval("{1: 'a'}[true]").is_err());
    }

    #[test]
    fn string_index_is_by_character() {
        assert_eq!(eval("'abc'[0]"), Ok(Value::String(StdArc::new("a".into()))));
        assert_eq!(
            eval("'abc'[-1]"),
            Ok(Value::String(StdArc::new("c".into())))
        );
        assert_eq!(
            eval("'abc'[true]"),
            Ok(Value::String(StdArc::new("b".into())))
        );
        assert!(eval("'abc'[0.0]").is_err());
        assert!(eval("'abc'[3]").is_err());
    }

    #[test]
    fn bytes_index_is_the_byte_as_an_int() {
        assert_eq!(eval("b'abc'[0]"), Ok(Value::Int(97)));
        assert_eq!(eval("b'abc'[-1]"), Ok(Value::Int(99)));
        assert!(eval("b'abc'[3]").is_err());
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
