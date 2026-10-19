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
//! specifically to avoid that cross-conversion. An `int`/`uint` probe
//! against a `bool`-keyed entry (or the reverse: a `bool` probe
//! against an `int`/`uint`-keyed entry) is celpy's own real asymmetry,
//! reproduced too — `BoolType` has no `__eq__` override in celpy, so a
//! plain dict lookup with an `int`/`uint` probe finds a `bool`-keyed
//! entry (hashing and comparing through the plain `int` `BoolType`
//! subclasses), but a `bool` probe against an `int`/`uint`-keyed entry
//! still raises (`IntType`/`UintType.__eq__`'s own `@type_matched`
//! has no exemption for the reverse direction) — confirmed against the
//! real evaluator, not assumed from the value arm above. See
//! [`python_index`]'s own doc
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
use cel::common::types::{
    CelBytes as BorrowedBytes, CelList as BorrowedList, CelMap as BorrowedMap,
    CelMapKey as BorrowedMapKey, CelString as BorrowedString,
};
use cel::common::value::Val;
use cel::{Context, ExecutionError, FunctionContext, Value};

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
/// cross-converting) map lookup, on a borrowed container rather than
/// one `strict_index_fn` has already fully converted into an owned
/// [`Value`] — `Err(())` for an index with no CEL key counterpart, out
/// of range, or a container that isn't a `list`/`map`/`string`/`bytes`
/// at all. Only the single looked-up element is ever converted via
/// [`Value::try_from`], not the whole container (issue #400): a `list`
/// index copies just that one element, out of a `DefaultList` whose own
/// elements `cel` already stores as individually boxed `Val`s, rather
/// than rebuilding every element into an owned `Value::List` only to
/// throw away every one but the indexed element, paid again on every
/// pass of a comprehension like `state.l.filter(x, state.l[0] == 0)`.
fn python_index(container: &dyn Val, index: &Value) -> Result<Value, ()> {
    if let Some(items) = container.downcast_ref::<BorrowedList>() {
        let idx = python_sequence_index(index, items.inner().len())?;
        return Value::try_from(items.inner()[idx].as_ref()).map_err(|_| ());
    }
    if let Some(s) = container.downcast_ref::<BorrowedString>() {
        // Python indexes a `str` by Unicode code point, not by byte —
        // `s.inner().chars()` is the Rust equivalent. Collects only the
        // characters, never the whole container's `Value` tree.
        let chars: Vec<char> = s.inner().chars().collect();
        let idx = python_sequence_index(index, chars.len())?;
        return Ok(Value::String(std::sync::Arc::new(chars[idx].to_string())));
    }
    if let Some(b) = container.downcast_ref::<BorrowedBytes>() {
        // Python's `bytes.__getitem__` with a plain (non-slice) index
        // returns the byte as an `int`, not a length-1 `bytes`.
        let bytes = b.inner();
        let idx = python_sequence_index(index, bytes.len())?;
        return Ok(Value::Int(i64::from(bytes[idx])));
    }
    if let Some(map) = container.downcast_ref::<BorrowedMap>() {
        return borrowed_map_index(map, index);
    }
    Err(())
}

/// [`python_index`]'s `map` arm: a strict (non-cross-converting) key
/// match through the underlying `HashMap` directly, same semantics as
/// the comment this replaces explained, but reading only the one
/// matched entry's value rather than every one in the map.
fn borrowed_map_index(map: &BorrowedMap, index: &Value) -> Result<Value, ()> {
    let key: Option<BorrowedMapKey> = match index {
        Value::Int(n) => Some(BorrowedMapKey::from(*n)),
        Value::UInt(n) => Some(BorrowedMapKey::from(*n)),
        Value::String(s) => Some(BorrowedMapKey::from(s.as_str().to_string())),
        Value::Bool(b) => Some(BorrowedMapKey::from(*b)),
        _ => None,
    };
    if let Some(key) = key {
        if let Some(value) = map.inner().get(&key) {
            return Value::try_from(value.as_ref()).map_err(|_| ());
        }
    }
    // celpy's own asymmetry (module docs): a plain dict lookup with an
    // `int`/`uint` probe finds a `bool`-keyed entry (`BoolType` has no
    // `__eq__` override, so it hashes and compares through the plain
    // `int` it subclasses) even though the strict key match above never
    // matches a bool key against an int/uint probe directly. Only tried
    // for an `int`/`uint` probe that missed above — a `bool` probe keeps
    // raising against an `int`/`uint`-keyed entry, since
    // `IntType`/`UintType.__eq__`'s own `@type_matched` has no exemption
    // for the reverse direction.
    let bool_equivalent = match index {
        Value::Int(n) if *n == 0 || *n == 1 => Some(*n == 1),
        Value::UInt(n) if *n == 0 || *n == 1 => Some(*n == 1),
        _ => None,
    };
    match bool_equivalent.and_then(|b| map.inner().get(&BorrowedMapKey::from(b))) {
        Some(value) => Value::try_from(value.as_ref()).map_err(|_| ()),
        None => Err(()),
    }
}

fn strict_index_fn(ftx: &FunctionContext) -> Result<Value, ExecutionError> {
    let container_ref = ftx.args[0].as_ref();
    let index_ref = ftx.args[1].as_ref();
    let index = Value::try_from(index_ref)?;
    match python_index(container_ref, &index) {
        Ok(value) => Ok(value),
        Err(()) => Err(ExecutionError::UnsupportedIndex(
            index,
            Value::try_from(container_ref)?,
        )),
    }
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
    fn int_or_uint_probe_finds_a_bool_keyed_entry() {
        // celpy's own asymmetry (confirmed against the real evaluator:
        // `state.m[1] == 'a'`/`state.m[1u] == 'a'` with `{m: {true:
        // 'a'}}` are both `true`) — `BoolType` has no `__eq__` override,
        // so a plain dict lookup with an `int`/`uint` probe resolves
        // through the plain `int` it subclasses.
        assert_eq!(
            eval("{true: 'a'}[1]"),
            Ok(Value::String(StdArc::new("a".into())))
        );
        assert_eq!(
            eval("{true: 'a'}[1u]"),
            Ok(Value::String(StdArc::new("a".into())))
        );
        assert_eq!(
            eval("{false: 'a'}[0]"),
            Ok(Value::String(StdArc::new("a".into())))
        );
        // A probe that isn't 0 or 1 never matches a `bool`-keyed entry,
        // whether or not one is present.
        assert!(eval("{true: 'a'}[2]").is_err());
    }

    #[test]
    fn bool_probe_against_an_int_entry_still_raises() {
        // The reverse direction is not a divergence at all: celpy's
        // own `IntType`/`UintType.__eq__` is `@type_matched` with no
        // exemption for a `bool` probe, so this raises in celpy too
        // (confirmed against the real evaluator).
        assert!(eval("{1: 'a'}[true]").is_err());
        assert!(eval("{1u: 'a'}[true]").is_err());
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
