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

use chrono::{Duration, FixedOffset, NaiveDate, NaiveDateTime};
use indexmap::IndexMap;
use num_bigint::BigInt;
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
    None,
    Bool(bool),
    /// Python ints are unbounded; see [`IntValue`].
    Int(IntValue),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Dict(Dict),
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

    pub fn is_none(&self) -> bool {
        matches!(self, Value::None)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<&IntValue> {
        match self {
            Value::Int(i) => Some(i),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_list_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Value::Dict(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_dict_mut(&mut self) -> Option<&mut Dict> {
        match self {
            Value::Dict(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_date(&self) -> Option<&NaiveDate> {
        match self {
            Value::Date(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_datetime(&self) -> Option<(&NaiveDateTime, Option<&FixedOffset>)> {
        match self {
            Value::DateTime(naive, offset) => Some((naive, offset.as_ref())),
            _ => None,
        }
    }

    fn is_numeric_tower(&self) -> bool {
        matches!(self, Value::Bool(_) | Value::Int(_) | Value::Float(_))
    }
}

// ---------------------------------------------------------------------
// Equality (Python dict-key semantics)
// ---------------------------------------------------------------------

/// The numeric tower (`bool`/`int`/`float`) reduced to one of two exact
/// representations for cross-variant comparison.
enum Num {
    Int(BigInt),
    Float(f64),
}

fn to_num(v: &Value) -> Num {
    match v {
        Value::Bool(b) => Num::Int(BigInt::from(if *b { 1 } else { 0 })),
        Value::Int(i) => Num::Int(i.to_bigint()),
        Value::Float(f) => Num::Float(*f),
        _ => unreachable!("to_num called on a non-numeric-tower Value"),
    }
}

fn numeric_eq(a: &Value, b: &Value) -> bool {
    match (to_num(a), to_num(b)) {
        (Num::Int(x), Num::Int(y)) => x == y,
        (Num::Float(x), Num::Float(y)) => x == y,
        (Num::Int(x), Num::Float(y)) | (Num::Float(y), Num::Int(x)) => {
            numeric::bigint_eq_f64(&x, y)
        }
    }
}

fn numeric_partial_cmp(a: &Value, b: &Value) -> Option<Ordering> {
    match (to_num(a), to_num(b)) {
        (Num::Int(x), Num::Int(y)) => Some(x.cmp(&y)),
        (Num::Float(x), Num::Float(y)) => x.partial_cmp(&y),
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
            Value::Bool(b) => hash_numeric_bigint(&BigInt::from(if *b { 1 } else { 0 }), state),
            Value::Int(i) => hash_numeric_bigint(&i.to_bigint(), state),
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
                // Likewise unhashable in Python; kept total and consistent
                // with `py_eq` rather than panicking.
                state.write_u8(4);
                for (k, v) in entries {
                    k.hash(state);
                    v.hash(state);
                }
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
/// identically (required: they are the same dict key).
fn hash_numeric_bigint<H: Hasher>(n: &BigInt, state: &mut H) {
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

    #[test]
    fn numeric_tower_equality_and_hash() {
        use std::collections::hash_map::DefaultHasher;
        fn hash_of(v: &Value) -> u64 {
            let mut h = DefaultHasher::new();
            v.hash(&mut h);
            h.finish()
        }

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
        let huge_plus_one = Value::from(huge + BigInt::from(1));
        assert!(!huge_plus_one.py_eq(&Value::Float(1e20)));
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
