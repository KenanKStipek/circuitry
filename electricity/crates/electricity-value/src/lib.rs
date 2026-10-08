//! Python-semantics `Value`: the runtime data type shared by every other
//! `electricity` crate (DESIGN.md §3.1). Circuitry's state is Python data,
//! not JSON data, so `Value` can hold things JSON cannot (big integers,
//! raw bytes, `NaN`, naive and timezone-aware date/times) and follows
//! Python's own rules for equality, hashing, ordering, `str()`, and
//! `repr()` rather than Rust's derived ones.
//!
//! This crate owns three things every later crate in the workspace builds
//! on:
//! - the [`Value`] enum itself, plus the constructors and accessors needed
//!   to build and inspect it;
//! - Python dict-key semantics for [`PartialEq`]/[`Hash`]/[`Eq`] (the
//!   numeric tower `True == 1 == 1.0`, `NaN != NaN`, naive/aware
//!   date-times never equal, `str`/`bytes` never equal) so `Value` can be
//!   an `IndexMap` key directly;
//! - a fallible Python-style ordering ([`Value::py_partial_cmp`]) and
//!   [`Value::py_str`]/[`Value::py_repr`], byte-identical to CPython
//!   3.11's `str()`/`repr()`.

mod datetime_repr;
mod float_repr;
mod generated;
pub mod int_value;
mod numeric;
mod string_repr;

pub use int_value::IntValue;

/// The nesting-depth limit shared by every crate in this workspace that
/// reads, writes or evaluates nested data (`electricity-json`,
/// `electricity-yaml`, `electricity-cel`'s `convert` module,
/// `electricity-template`'s section nesting): a document, expression or
/// template nested deeper than this is rejected with that crate's own
/// depth error instead of being read, written or converted. One shared
/// number, rather than each crate picking its own, so "how deep can
/// nested data in this runtime go" has a single answer regardless of
/// which format it arrived in (#394).
///
/// 512 is half of CPython's default `sys.getrecursionlimit()` (1000 call
/// frames) — Python's own loaders/evaluators for the same data raise
/// `RecursionError` well before frame 1000 in practice, since each
/// logical nesting level costs more than one Python call frame (several
/// for `json.loads`, more for a CEL evaluation through `celpy`), and
/// however many frames of the caller's own stack already exist before
/// Circuitry's loader is entered are *subtracted* from the budget, not
/// added to it. 512 is comfortably inside that moving target for any
/// realistic caller depth without being so small that it rejects
/// documents an ordinary orchestration produces. It is not, and is not
/// meant to be, the exact frame count at which Python itself would raise
/// — there is no such exact number (`electricity-json`'s crate docs).
///
/// A `Value` built by reading JSON or YAML through this workspace can
/// never exceed this depth (both readers enforce it while reading, not
/// after). One built at run time instead — a future state merge, or a
/// loop that wraps a value (nothing in this workspace yet converts a
/// `cel::Value` back into this `Value`, so a CEL evaluation result
/// specifically is not one of today's examples) — is not automatically
/// bounded by this constant; [`Value::depth`] lets a caller that builds
/// such a value check it before relying on [`Value::py_str`],
/// [`Value::py_repr`], equality, hashing or [`Value::py_partial_cmp`],
/// every one of which recurses over nested values and assumes this
/// invariant rather than enforcing it itself. [`Drop`] is the one
/// exception: it is iterative regardless of depth (below), because a
/// `Value` of unexpected depth dropped on a worker thread must not abort
/// the process no matter how it got that deep.
pub const MAX_DEPTH: usize = 512;

use chrono::{Duration, FixedOffset, NaiveDate, NaiveDateTime};
use indexmap::IndexMap;
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

/// A `Value`-keyed, insertion-ordered mapping — the representation of a
/// Circuitry/Python `dict`.
pub type Dict = IndexMap<Value, Value>;

/// A Circuitry runtime value, modeled on Python's own data model.
///
/// `Dict` keys are themselves `Value` (not `String`): YAML and JSON can
/// produce non-string keys (`yes:` resolves to the bool key `true`; a bare
/// integer can be a mapping key), so the key side of a mapping has to go
/// through the same resolver as any other value.
#[derive(Debug, Clone)]
pub enum Value {
    /// Python's `None`.
    None,
    /// Python's `bool`. Part of the numeric tower: `True`/`False` compare,
    /// hash, and dict-key-collide with `1`/`0` and `1.0`/`0.0`.
    Bool(bool),
    /// Python ints are unbounded; see [`IntValue`].
    Int(IntValue),
    /// Python's `float`, an IEEE-754 double. Part of the numeric tower.
    Float(f64),
    /// Python's `str`, a sequence of Unicode codepoints.
    Str(String),
    /// Python's `bytes`, a sequence of raw bytes. Never equal to a `Str`,
    /// even one with the same content decoded.
    Bytes(Vec<u8>),
    /// Python's `list`. Unlike `Dict`/`Str`/`Bytes`, Python lists aren't
    /// hashable, but this `Value::List` still has a total `Hash` impl
    /// (see the [`Hash`] impl below) so `Value` itself always implements
    /// `Hash`.
    List(Vec<Value>),
    /// Python's `dict`, insertion-ordered like CPython's since 3.7.
    Dict(Dict),
    /// Python's `datetime.date`.
    Date(NaiveDate),
    /// A naive date-time plus an optional fixed UTC offset. `None` means
    /// naive (no `tzinfo`), exactly as PyYAML's timestamp resolver
    /// produces for a timestamp scalar with no explicit offset — this is
    /// why the offset is a separate `Option` rather than always defaulting
    /// a missing offset to UTC (naive and aware date-times are never
    /// equal, and ordering between them raises; defaulting to UTC would
    /// erase that distinction).
    DateTime(NaiveDateTime, Option<FixedOffset>),
}

// ---------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Int(IntValue::Small(n))
    }
}

impl From<BigInt> for Value {
    fn from(n: BigInt) -> Self {
        Value::Int(IntValue::from_bigint(n))
    }
}

impl From<IntValue> for Value {
    fn from(n: IntValue) -> Self {
        Value::Int(n)
    }
}

impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Value::Float(f)
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(s)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.to_string())
    }
}

impl From<Vec<u8>> for Value {
    fn from(b: Vec<u8>) -> Self {
        Value::Bytes(b)
    }
}

impl From<Vec<Value>> for Value {
    fn from(items: Vec<Value>) -> Self {
        Value::List(items)
    }
}

impl From<Dict> for Value {
    fn from(dict: Dict) -> Self {
        Value::Dict(dict)
    }
}

impl From<NaiveDate> for Value {
    fn from(date: NaiveDate) -> Self {
        Value::Date(date)
    }
}

// ---------------------------------------------------------------------
// Accessors
// ---------------------------------------------------------------------

impl Value {
    /// The Python type name this variant corresponds to, e.g. for error
    /// messages (`"NoneType"`, `"bool"`, `"int"`, `"float"`, `"str"`,
    /// `"bytes"`, `"list"`, `"dict"`, `"datetime.date"`,
    /// `"datetime.datetime"`).
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::None => "NoneType",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "str",
            Value::Bytes(_) => "bytes",
            Value::List(_) => "list",
            Value::Dict(_) => "dict",
            Value::Date(_) => "datetime.date",
            Value::DateTime(..) => "datetime.datetime",
        }
    }

    /// `true` iff this is `Value::None` (Python's `value is None`).
    pub fn is_none(&self) -> bool {
        matches!(self, Value::None)
    }

    /// The underlying `bool`, or `None` if this isn't a `Value::Bool`.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The underlying [`IntValue`], or `None` if this isn't a `Value::Int`.
    pub fn as_int(&self) -> Option<&IntValue> {
        match self {
            Value::Int(i) => Some(i),
            _ => None,
        }
    }

    /// The underlying `f64`, or `None` if this isn't a `Value::Float`.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// The underlying string slice, or `None` if this isn't a `Value::Str`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The underlying byte slice, or `None` if this isn't a `Value::Bytes`.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// The underlying item slice, or `None` if this isn't a `Value::List`.
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    /// A mutable reference to the underlying items, or `None` if this
    /// isn't a `Value::List`.
    pub fn as_list_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    /// The underlying [`Dict`], or `None` if this isn't a `Value::Dict`.
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Value::Dict(d) => Some(d),
            _ => None,
        }
    }

    /// A mutable reference to the underlying [`Dict`], or `None` if this
    /// isn't a `Value::Dict`.
    pub fn as_dict_mut(&mut self) -> Option<&mut Dict> {
        match self {
            Value::Dict(d) => Some(d),
            _ => None,
        }
    }

    /// The underlying [`NaiveDate`], or `None` if this isn't a `Value::Date`.
    pub fn as_date(&self) -> Option<&NaiveDate> {
        match self {
            Value::Date(d) => Some(d),
            _ => None,
        }
    }

    /// The underlying naive date-time plus its UTC offset (`None` for a
    /// naive date-time), or `None` if this isn't a `Value::DateTime`.
    pub fn as_datetime(&self) -> Option<(&NaiveDateTime, Option<&FixedOffset>)> {
        match self {
            Value::DateTime(naive, offset) => Some((naive, offset.as_ref())),
            _ => None,
        }
    }

    fn is_numeric_tower(&self) -> bool {
        matches!(self, Value::Bool(_) | Value::Int(_) | Value::Float(_))
    }

    /// This value's containment depth: 0 for any scalar, or
    /// `1 + the deepest child's depth` for a `List`/`Dict` (a dict's keys
    /// count the same as its values -- a key that is itself a nested
    /// container, however unusual, still contributes to depth). Computed
    /// with an explicit work stack rather than recursion, so calling this
    /// on an already arbitrarily deep `Value` (one built at run time,
    /// never one read through this workspace's own JSON/YAML readers,
    /// which enforce [`MAX_DEPTH`] themselves) cannot itself overflow the
    /// stack -- it is exactly the check a caller needs before an
    /// operation that does recurse ([`Value::py_str`], [`Value::py_repr`],
    /// equality, hashing, [`Value::py_partial_cmp`]) would be unsafe to
    /// run on it.
    ///
    /// Counts nesting *to the deepest value*, not every container: an
    /// empty `List`/`Dict` has no child to push a deeper `depth` for, so
    /// it contributes the same `depth` as its own parent would see from
    /// any other child, not one more the way `electricity-json`'s own
    /// reader/writer count *every* `[`/`{` — 513 nested *empty* lists
    /// (`depth() == 512`, since the innermost, empty one contributes
    /// nothing beyond what its parent already counted) therefore passes
    /// a `<= MAX_DEPTH` check despite being one bracket deeper than
    /// `electricity-json` would accept.
    /// Immaterial in practice (JSON/YAML can never produce a `Value` this
    /// function needs to check in the first place — both readers enforce
    /// `MAX_DEPTH` themselves while reading, long before a reader could
    /// hand back a `Value` for this to measure; see this constant's own
    /// docs) and off by at most one regardless of how deep the value
    /// actually is, against a limit (512) already far under where the
    /// recursive operations this guards (`py_str`/`py_repr`/equality/
    /// hashing/`py_partial_cmp`) would actually become unsafe.
    pub fn depth(&self) -> usize {
        let mut max_depth = 0usize;
        let mut stack: Vec<(&Value, usize)> = vec![(self, 0)];
        while let Some((value, depth)) = stack.pop() {
            if depth > max_depth {
                max_depth = depth;
            }
            match value {
                Value::List(items) => {
                    for item in items {
                        stack.push((item, depth + 1));
                    }
                }
                Value::Dict(entries) => {
                    for (key, val) in entries {
                        stack.push((key, depth + 1));
                        stack.push((val, depth + 1));
                    }
                }
                _ => {}
            }
        }
        max_depth
    }
}

/// Drops a `Value` tree without ever recursing: the default, derived
/// `Drop` glue for an enum holding `Vec<Value>`/`IndexMap<Value, Value>`
/// drops each child in place, which for a `List`/`Dict` means calling
/// `Value`'s own `Drop` again on every element -- one stack frame per
/// nesting level. [`MAX_DEPTH`] bounds a `Value` built by reading JSON or
/// YAML, but not one built at run time (a future state merge or a loop
/// that wraps a value), and a worker thread's 2 MiB
/// stack is not generous: overflowing it in `Drop` aborts the whole
/// process (Rust cannot unwind out of a `Drop` panic the normal way,
/// and a stack overflow isn't a catchable panic to begin with), unlike
/// overflowing it in `py_str`/equality/etc., which only need
/// [`Value::depth`] checked first because *they* can be guarded by a
/// caller -- nothing guards an implicit drop at the end of a scope. This
/// impl instead flattens the tree into an explicit, heap-allocated stack
/// and drops each node after already emptying its own children into that
/// same stack, so by the time a node's own (otherwise-recursive) `Drop`
/// runs, it has no children left and returns immediately.
///
/// A `List`/`Dict` with no `List`/`Dict` child of its own -- the common
/// case; a template render clones and drops a whole scope stack on
/// every section, and most state is flat -- skips building that stack
/// at all: none of its children can recurse either way, so the
/// ordinary per-field drop glue is exactly as safe and does not cost
/// this `impl` a `Vec`/`flat_map`/`collect` it would not otherwise need.
impl Drop for Value {
    fn drop(&mut self) {
        let has_container_child = match self {
            Value::List(items) => items.iter().any(is_container),
            Value::Dict(entries) => entries
                .iter()
                .any(|(k, v)| is_container(k) || is_container(v)),
            _ => return,
        };
        if !has_container_child {
            // No child can recurse either, so the ordinary per-field
            // drop glue below (about to run for every field of this
            // `List`/`Dict` regardless) is exactly as safe, and this
            // `impl` doesn't need to pay for a `Vec`/`flat_map`/`collect`
            // it would not otherwise use.
            return;
        }
        let mut pending: Vec<Value> = match self {
            Value::List(items) => std::mem::take(items),
            Value::Dict(entries) => std::mem::take(entries)
                .into_iter()
                .flat_map(|(k, v)| [k, v])
                .collect(),
            _ => unreachable!("matched List/Dict above"),
        };
        while let Some(mut value) = pending.pop() {
            match &mut value {
                Value::List(items) => pending.extend(std::mem::take(items)),
                Value::Dict(entries) => pending.extend(
                    std::mem::take(entries)
                        .into_iter()
                        .flat_map(|(k, v)| [k, v]),
                ),
                _ => {}
            }
            // `value`'s own children are now empty, so dropping it here
            // (end of this loop iteration) cannot recurse any further.
        }
    }
}

fn is_container(value: &Value) -> bool {
    matches!(value, Value::List(_) | Value::Dict(_))
}

// ---------------------------------------------------------------------
// Equality (Python dict-key semantics)
// ---------------------------------------------------------------------

/// The numeric tower (`bool`/`int`/`float`) reduced to one of three exact
/// representations for cross-variant comparison. `I64` is the fast path
/// for `Bool` and a fitting `Int`: no `BigInt` allocation for the common
/// case of two small ints.
enum Num {
    I64(i64),
    Int(BigInt),
    Float(f64),
}

fn to_num(v: &Value) -> Num {
    match v {
        Value::Bool(b) => Num::I64(if *b { 1 } else { 0 }),
        Value::Int(IntValue::Small(n)) => Num::I64(*n),
        Value::Int(IntValue::Big(n)) => Num::Int(n.clone()),
        Value::Float(f) => Num::Float(*f),
        _ => unreachable!("to_num called on a non-numeric-tower Value"),
    }
}

fn numeric_eq(a: &Value, b: &Value) -> bool {
    match (to_num(a), to_num(b)) {
        (Num::I64(x), Num::I64(y)) => x == y,
        (Num::Int(x), Num::Int(y)) => x == y,
        (Num::Float(x), Num::Float(y)) => x == y,
        (Num::I64(x), Num::Int(y)) | (Num::Int(y), Num::I64(x)) => BigInt::from(x) == y,
        (Num::I64(x), Num::Float(y)) | (Num::Float(y), Num::I64(x)) => numeric::i64_eq_f64(x, y),
        (Num::Int(x), Num::Float(y)) | (Num::Float(y), Num::Int(x)) => {
            numeric::bigint_eq_f64(&x, y)
        }
    }
}

fn numeric_partial_cmp(a: &Value, b: &Value) -> Option<Ordering> {
    match (to_num(a), to_num(b)) {
        (Num::I64(x), Num::I64(y)) => Some(x.cmp(&y)),
        (Num::Int(x), Num::Int(y)) => Some(x.cmp(&y)),
        (Num::Float(x), Num::Float(y)) => x.partial_cmp(&y),
        (Num::I64(x), Num::Int(y)) => Some(BigInt::from(x).cmp(&y)),
        (Num::Int(x), Num::I64(y)) => Some(x.cmp(&BigInt::from(y))),
        (Num::I64(x), Num::Float(y)) => numeric::i64_cmp_f64(x, y),
        (Num::Float(x), Num::I64(y)) => numeric::i64_cmp_f64(y, x).map(Ordering::reverse),
        (Num::Int(x), Num::Float(y)) => numeric::bigint_cmp_f64(&x, y),
        (Num::Float(x), Num::Int(y)) => numeric::bigint_cmp_f64(&y, x).map(Ordering::reverse),
    }
}

fn utc_instant(naive: &NaiveDateTime, offset: &FixedOffset) -> NaiveDateTime {
    *naive - Duration::seconds(offset.local_minus_utc() as i64)
}

fn datetime_eq(
    a_naive: &NaiveDateTime,
    a_offset: &Option<FixedOffset>,
    b_naive: &NaiveDateTime,
    b_offset: &Option<FixedOffset>,
) -> bool {
    match (a_offset, b_offset) {
        (None, None) => a_naive == b_naive,
        (Some(a_off), Some(b_off)) => utc_instant(a_naive, a_off) == utc_instant(b_naive, b_off),
        _ => false, // naive vs aware: never equal
    }
}

impl Value {
    /// Python `==` for dict-key purposes: `True`/`1`/`1.0` are equal,
    /// `NaN` is never equal to itself, a naive and an aware date-time are
    /// never equal, and `str`/`bytes` are never equal to each other or to
    /// anything outside their own variant.
    pub fn py_eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::None, Value::None) => true,
            (a, b) if a.is_numeric_tower() && b.is_numeric_tower() => numeric_eq(a, b),
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Bytes(a), Value::Bytes(b)) => a == b,
            (Value::List(a), Value::List(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.py_eq(y))
            }
            (Value::Dict(a), Value::Dict(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(k, v)| b.get(k).is_some_and(|bv| v.py_eq(bv)))
            }
            (Value::Date(a), Value::Date(b)) => a == b,
            (Value::DateTime(an, ao), Value::DateTime(bn, bo)) => datetime_eq(an, ao, bn, bo),
            _ => false,
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.py_eq(other)
    }
}

/// `NaN` breaks reflexivity (`NaN != NaN`), same as Python's own `float`.
/// Any `Value` holding a `NaN` is therefore only ever its own `HashMap`
/// bucket-mate by coincidence, never found again by lookup — exactly
/// Python's own quirk when a `NaN` ends up as (part of) a dict key.
impl Eq for Value {}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Value::None => state.write_u8(0),
            Value::Bool(b) => hash_small_int(if *b { 1 } else { 0 }, state),
            Value::Int(IntValue::Small(n)) => hash_small_int(*n, state),
            Value::Int(IntValue::Big(n)) => hash_numeric_bigint(n, state),
            Value::Float(f) => hash_float(*f, state),
            Value::Str(s) => {
                state.write_u8(1);
                s.hash(state);
            }
            Value::Bytes(b) => {
                state.write_u8(2);
                b.hash(state);
            }
            Value::List(items) => {
                // Python lists are themselves unhashable; nothing upstream
                // of this crate is expected to use one as a dict key. This
                // impl only needs to stay consistent with `py_eq`, which it
                // does (same elements, same order -> same hash).
                state.write_u8(3);
                for item in items {
                    item.hash(state);
                }
            }
            Value::Dict(entries) => {
                // Likewise unhashable in Python; kept total rather than
                // panicking. `py_eq` treats dicts as equal regardless of
                // key order, so this can only combine per-entry hashes
                // order-independently (here: XOR) rather than folding them
                // into the `Hasher`'s running state in iteration order,
                // or two dicts with the same entries in different
                // insertion order would hash differently despite being
                // equal.
                state.write_u8(4);
                state.write_usize(entries.len());
                let mut combined: u64 = 0;
                for (k, v) in entries {
                    let mut entry_hasher = std::collections::hash_map::DefaultHasher::new();
                    k.hash(&mut entry_hasher);
                    v.hash(&mut entry_hasher);
                    combined ^= entry_hasher.finish();
                }
                state.write_u64(combined);
            }
            Value::Date(d) => {
                state.write_u8(5);
                d.hash(state);
            }
            Value::DateTime(naive, offset) => {
                state.write_u8(6);
                match offset {
                    None => {
                        state.write_u8(0);
                        naive.hash(state);
                    }
                    Some(off) => {
                        state.write_u8(1);
                        utc_instant(naive, off).hash(state);
                    }
                }
            }
        }
    }
}

/// Shared by `Bool`/`Int`/integral `Float` so `True`, `1`, and `1.0` hash
/// identically (required: they are the same dict key), and always through
/// [`hash_small_int`] whenever the value fits an `i64` so that `Small`,
/// `Big`, and a `Float`'s [`numeric::exact_integer_value`] never diverge
/// just because they reached this function by different routes.
fn hash_numeric_bigint<H: Hasher>(n: &BigInt, state: &mut H) {
    match n.to_i64() {
        Some(small) => hash_small_int(small, state),
        None => {
            state.write_u8(10);
            n.hash(state);
        }
    }
}

/// The `i64` fast path for [`hash_numeric_bigint`]: no `BigInt` allocation
/// for `Bool` or an `Int::Small` (the common case).
fn hash_small_int<H: Hasher>(n: i64, state: &mut H) {
    state.write_u8(10);
    n.hash(state);
}

fn hash_float<H: Hasher>(f: f64, state: &mut H) {
    match numeric::exact_integer_value(f) {
        Some(exact) => hash_numeric_bigint(&exact, state),
        None => {
            // Non-integral, NaN, or infinite: never equal to any `Bool`/
            // `Int`, so this only needs to be consistent with other
            // `Float`s, which bit-identical hashing trivially is.
            state.write_u8(11);
            f.to_bits().hash(state);
        }
    }
}

// ---------------------------------------------------------------------
// Fallible, Python-style ordering
// ---------------------------------------------------------------------

/// Why [`Value::py_partial_cmp`] could not order two values — Python
/// raises `TypeError` in both cases. Not an error when values are simply
/// `NaN`-unordered: that case returns `Ok(None)`, since Python's `<`/`>`/
/// `<=`/`>=` against a `NaN` return `False` rather than raising.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompareError {
    /// The two values have types Python's `<`/`>`/`<=`/`>=` never compares
    /// (different variants outside the numeric tower, or a type — `dict`,
    /// `NoneType` — that is never orderable even against its own type).
    IncomparableTypes {
        left: &'static str,
        right: &'static str,
    },
    /// One `datetime.datetime` is naive and the other is timezone-aware.
    NaiveAwareMismatch,
    /// A bare `datetime.date` compared against a `datetime.datetime`, in
    /// either operand order. CPython raises this distinct message rather
    /// than the usual `IncomparableTypes` one because `datetime.datetime`
    /// subclasses `datetime.date` — without this variant, callers
    /// couldn't tell this pair apart from truly unrelated types to
    /// reproduce CPython's exact `TypeError` text.
    DateDateTimeMismatch,
}

impl fmt::Display for CompareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompareError::IncomparableTypes { left, right } => {
                write!(f, "'{left}' and '{right}' are not orderable")
            }
            CompareError::NaiveAwareMismatch => {
                write!(f, "can't compare offset-naive and offset-aware datetimes")
            }
            CompareError::DateDateTimeMismatch => {
                write!(f, "can't compare datetime.datetime to datetime.date")
            }
        }
    }
}

impl std::error::Error for CompareError {}

fn list_partial_cmp(a: &[Value], b: &[Value]) -> Result<Option<Ordering>, CompareError> {
    for (x, y) in a.iter().zip(b.iter()) {
        if !x.py_eq(y) {
            return x.py_partial_cmp(y);
        }
    }
    Ok(Some(a.len().cmp(&b.len())))
}

fn datetime_partial_cmp(
    a_naive: &NaiveDateTime,
    a_offset: &Option<FixedOffset>,
    b_naive: &NaiveDateTime,
    b_offset: &Option<FixedOffset>,
) -> Result<Option<Ordering>, CompareError> {
    match (a_offset, b_offset) {
        (None, None) => Ok(Some(a_naive.cmp(b_naive))),
        (Some(a_off), Some(b_off)) => Ok(Some(
            utc_instant(a_naive, a_off).cmp(&utc_instant(b_naive, b_off)),
        )),
        _ => Err(CompareError::NaiveAwareMismatch),
    }
}

impl Value {
    /// A fallible, Python-style three-way comparison.
    ///
    /// - `Ok(Some(ordering))`: a normal, defined order.
    /// - `Ok(None)`: the values are numerically unordered (one side is
    ///   `NaN`) — not an error; Python's `<`, `>`, `<=`, `>=` all evaluate
    ///   to `false` against a `NaN`, which is exactly what deriving all
    ///   four relational operators from this `Option<Ordering>` the usual
    ///   way (`<` iff `Some(Less)`, `<=` iff `Some(Less) | Some(Equal)`,
    ///   ...) produces.
    /// - `Err(_)`: Python itself raises `TypeError` for this pair (see
    ///   [`CompareError`]).
    pub fn py_partial_cmp(&self, other: &Value) -> Result<Option<Ordering>, CompareError> {
        match (self, other) {
            (a, b) if a.is_numeric_tower() && b.is_numeric_tower() => Ok(numeric_partial_cmp(a, b)),
            (Value::Str(a), Value::Str(b)) => Ok(Some(a.cmp(b))),
            (Value::Bytes(a), Value::Bytes(b)) => Ok(Some(a.cmp(b))),
            (Value::List(a), Value::List(b)) => list_partial_cmp(a, b),
            (Value::Date(a), Value::Date(b)) => Ok(Some(a.cmp(b))),
            (Value::DateTime(an, ao), Value::DateTime(bn, bo)) => {
                datetime_partial_cmp(an, ao, bn, bo)
            }
            (Value::Date(_), Value::DateTime(..)) | (Value::DateTime(..), Value::Date(_)) => {
                Err(CompareError::DateDateTimeMismatch)
            }
            _ => Err(CompareError::IncomparableTypes {
                left: self.type_name(),
                right: other.type_name(),
            }),
        }
    }

    /// Python `<`.
    pub fn py_lt(&self, other: &Value) -> Result<bool, CompareError> {
        Ok(self.py_partial_cmp(other)? == Some(Ordering::Less))
    }

    /// Python `<=`.
    pub fn py_le(&self, other: &Value) -> Result<bool, CompareError> {
        Ok(matches!(
            self.py_partial_cmp(other)?,
            Some(Ordering::Less | Ordering::Equal)
        ))
    }

    /// Python `>`.
    pub fn py_gt(&self, other: &Value) -> Result<bool, CompareError> {
        Ok(self.py_partial_cmp(other)? == Some(Ordering::Greater))
    }

    /// Python `>=`.
    pub fn py_ge(&self, other: &Value) -> Result<bool, CompareError> {
        Ok(matches!(
            self.py_partial_cmp(other)?,
            Some(Ordering::Greater | Ordering::Equal)
        ))
    }
}

// ---------------------------------------------------------------------
// py_str / py_repr
// ---------------------------------------------------------------------

impl Value {
    /// Python `str(value)`.
    ///
    /// Known difference from CPython 3.11: CPython raises `ValueError:
    /// Exceeds the limit (4300 digits) for integer string conversion`
    /// converting an `int` with more than 4300 decimal digits to a string
    /// ([PEP 0, `sys.set_int_max_str_digits`](https://docs.python.org/3.11/library/stdtypes.html#int-max-str-digits)).
    /// `py_str`/`py_repr` always print the digits; later lanes building
    /// Python-exact error behavior need to decide whether to reproduce
    /// the limit.
    pub fn py_str(&self) -> String {
        match self {
            Value::None => "None".to_string(),
            Value::Bool(b) => py_bool_str(*b),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => float_repr::py_float_repr(*f),
            Value::Str(s) => s.clone(),
            Value::Bytes(b) => string_repr::py_bytes_repr(b),
            Value::List(_) | Value::Dict(_) => self.py_repr(),
            Value::Date(d) => datetime_repr::date_str(d),
            Value::DateTime(naive, offset) => datetime_repr::datetime_str(naive, offset.as_ref()),
        }
    }

    /// Python `repr(value)`.
    ///
    /// See [`Value::py_str`] for a known difference from CPython 3.11 on
    /// very large integers.
    pub fn py_repr(&self) -> String {
        match self {
            Value::None => "None".to_string(),
            Value::Bool(b) => py_bool_str(*b),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => float_repr::py_float_repr(*f),
            Value::Str(s) => string_repr::py_str_repr(s),
            Value::Bytes(b) => string_repr::py_bytes_repr(b),
            Value::List(items) => {
                let inner = items
                    .iter()
                    .map(Value::py_repr)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[{inner}]")
            }
            Value::Dict(entries) => {
                let inner = entries
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k.py_repr(), v.py_repr()))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{{inner}}}")
            }
            Value::Date(d) => datetime_repr::date_repr(d),
            Value::DateTime(naive, offset) => datetime_repr::datetime_repr(naive, offset.as_ref()),
        }
    }
}

fn py_bool_str(b: bool) -> String {
    if b { "True" } else { "False" }.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(n: i64) -> Value {
        Value::Int(IntValue::Small(n))
    }

    fn hash_of(v: &Value) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        let mut h = DefaultHasher::new();
        v.hash(&mut h);
        h.finish()
    }

    #[test]
    fn numeric_tower_equality_and_hash() {
        let t = Value::Bool(true);
        let one = int(1);
        let one_f = Value::Float(1.0);
        assert!(t.py_eq(&one));
        assert!(one.py_eq(&one_f));
        assert!(t.py_eq(&one_f));
        assert_eq!(hash_of(&t), hash_of(&one));
        assert_eq!(hash_of(&one), hash_of(&one_f));

        let zero = int(0);
        let f = Value::Bool(false);
        let zero_f = Value::Float(-0.0);
        assert!(f.py_eq(&zero));
        assert!(zero.py_eq(&zero_f));
        assert_eq!(hash_of(&f), hash_of(&zero_f));
    }

    #[test]
    fn nan_is_not_equal_to_itself() {
        let nan = Value::Float(f64::NAN);
        assert!(!nan.py_eq(&nan));
    }

    #[test]
    fn str_and_bytes_never_equal() {
        assert!(!Value::Str("a".into()).py_eq(&Value::Bytes(b"a".to_vec())));
    }

    #[test]
    fn dict_can_use_value_as_key() {
        let mut d: Dict = Dict::new();
        d.insert(int(1), Value::Str("one".into()));
        assert_eq!(d.get(&Value::Bool(true)).unwrap().as_str(), Some("one"));
        assert_eq!(d.get(&Value::Float(1.0)).unwrap().as_str(), Some("one"));
    }

    #[test]
    fn exact_big_int_vs_float_equality() {
        let huge: BigInt = "100000000000000000000".parse().unwrap();
        let huge_val = Value::from(huge.clone());
        assert!(huge_val.py_eq(&Value::Float(1e20)));
        assert_eq!(hash_of(&huge_val), hash_of(&Value::Float(1e20)));
        let huge_plus_one = Value::from(huge + BigInt::from(1));
        assert!(!huge_plus_one.py_eq(&Value::Float(1e20)));
    }

    #[test]
    fn dicts_with_different_insertion_order_are_equal_and_hash_equal() {
        let mut a: Dict = Dict::new();
        a.insert(int(1), Value::Str("one".into()));
        a.insert(Value::Str("two".into()), int(2));

        let mut b: Dict = Dict::new();
        b.insert(Value::Str("two".into()), int(2));
        b.insert(int(1), Value::Str("one".into()));

        let a = Value::Dict(a);
        let b = Value::Dict(b);
        assert!(a.py_eq(&b));
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn naive_and_aware_datetime_never_equal() {
        use chrono::NaiveDate;
        let naive = NaiveDate::from_ymd_opt(2020, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let a = Value::DateTime(naive, None);
        let b = Value::DateTime(naive, Some(FixedOffset::east_opt(0).unwrap()));
        assert!(!a.py_eq(&b));
        assert!(matches!(
            a.py_partial_cmp(&b),
            Err(CompareError::NaiveAwareMismatch)
        ));
    }

    #[test]
    fn date_vs_datetime_is_a_dedicated_compare_error() {
        use chrono::NaiveDate;
        let date = Value::Date(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap());
        let naive = NaiveDate::from_ymd_opt(2020, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let datetime = Value::DateTime(naive, None);
        assert_eq!(
            date.py_partial_cmp(&datetime),
            Err(CompareError::DateDateTimeMismatch)
        );
        assert_eq!(
            datetime.py_partial_cmp(&date),
            Err(CompareError::DateDateTimeMismatch)
        );
        assert_eq!(
            CompareError::DateDateTimeMismatch.to_string(),
            "can't compare datetime.datetime to datetime.date"
        );
    }

    #[test]
    fn aware_datetimes_at_the_same_instant_are_equal() {
        use chrono::NaiveDate;
        let utc_noon = NaiveDate::from_ymd_opt(2020, 1, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let plus_one = NaiveDate::from_ymd_opt(2020, 1, 1)
            .unwrap()
            .and_hms_opt(13, 0, 0)
            .unwrap();
        let a = Value::DateTime(utc_noon, Some(FixedOffset::east_opt(0).unwrap()));
        let b = Value::DateTime(plus_one, Some(FixedOffset::east_opt(3600).unwrap()));
        assert!(a.py_eq(&b));
        assert_eq!(a.py_partial_cmp(&b).unwrap(), Some(Ordering::Equal));
    }

    #[test]
    fn ordering_raises_on_incomparable_types() {
        let err = Value::None.py_partial_cmp(&Value::None).unwrap_err();
        assert!(matches!(err, CompareError::IncomparableTypes { .. }));

        let err = Value::Str("a".into()).py_partial_cmp(&int(1)).unwrap_err();
        assert!(matches!(err, CompareError::IncomparableTypes { .. }));

        let mut d1 = Dict::new();
        d1.insert(int(1), int(2));
        let d2 = d1.clone();
        let err = Value::Dict(d1)
            .py_partial_cmp(&Value::Dict(d2))
            .unwrap_err();
        assert!(matches!(err, CompareError::IncomparableTypes { .. }));
    }

    #[test]
    fn nan_ordering_is_unordered_not_an_error() {
        let nan = Value::Float(f64::NAN);
        assert_eq!(nan.py_partial_cmp(&int(1)).unwrap(), None);
        assert!(!nan.py_lt(&int(1)).unwrap());
        assert!(!nan.py_ge(&int(1)).unwrap());
    }

    #[test]
    fn list_ordering_is_lexicographic() {
        let a = Value::List(vec![int(1), int(2)]);
        let b = Value::List(vec![int(1), int(3)]);
        assert_eq!(a.py_partial_cmp(&b).unwrap(), Some(Ordering::Less));
        let shorter = Value::List(vec![int(1)]);
        assert_eq!(shorter.py_partial_cmp(&a).unwrap(), Some(Ordering::Less));
    }

    #[test]
    fn py_str_and_repr_basics() {
        assert_eq!(Value::None.py_str(), "None");
        assert_eq!(Value::Bool(true).py_str(), "True");
        assert_eq!(int(42).py_str(), "42");
        assert_eq!(Value::Str("hi".into()).py_str(), "hi");
        assert_eq!(Value::Str("hi".into()).py_repr(), "'hi'");
        let list = Value::List(vec![int(1), Value::Str("a".into())]);
        assert_eq!(list.py_str(), "[1, 'a']");
        assert_eq!(list.py_repr(), "[1, 'a']");
        let mut d = Dict::new();
        d.insert(Value::Str("a".into()), int(1));
        assert_eq!(Value::Dict(d).py_str(), "{'a': 1}");
    }

    #[test]
    fn type_names() {
        assert_eq!(Value::None.type_name(), "NoneType");
        assert_eq!(int(1).type_name(), "int");
        assert_eq!(Value::Bytes(vec![]).type_name(), "bytes");
    }
}
