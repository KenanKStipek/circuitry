//! A dedicated `Value` → text writer matching `json.dumps` exactly
//! (DESIGN.md §3.4, rust-ecosystem.md item 22), rather than bending
//! `serde_json`'s `ryu`-based formatter and ASCII-only string encoder to
//! Python's rules.
//!
//! Known difference from CPython 3.11, inherited from `electricity-value`
//! (see its `py_str` doc): an integer longer than 4300 decimal digits
//! writes its full digits here, where CPython's `json.dumps` raises
//! `ValueError` converting it to a string (`sys.set_int_max_str_digits`).

use electricity_value::{Dict, Value};
use std::cmp::Ordering;
use std::fmt;

/// How a plain `json.dumps` call is parametrized: `indent=None` vs.
/// `indent=2`, `sort_keys`, `ensure_ascii`. Deliberately independent
/// knobs (DESIGN.md §3.4.1): a future `--out` variant can combine them
/// without electricity-json inferring one from another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteMode {
    pub indent: Option<u8>,
    pub sort_keys: bool,
    pub ensure_ascii: bool,
}

impl WriteMode {
    /// Plain `--out`: `json.dumps(saved)` (runtime-semantics.md §8.3).
    pub const COMPACT: WriteMode = WriteMode {
        indent: None,
        sort_keys: false,
        ensure_ascii: true,
    };

    /// `--out --pretty`: `json.dumps(saved, indent=2, sort_keys=True)`.
    pub const PRETTY: WriteMode = WriteMode {
        indent: Some(2),
        sort_keys: true,
        ensure_ascii: true,
    };
}

/// Why a `Value` tree can't be written as JSON — every variant mirrors a
/// `TypeError` CPython's `json.dumps` itself raises for the same input.
/// This text comes from CPython's `json` module, not Circuitry's own code,
/// so (DESIGN.md §1/§12) it only has to be a non-empty message naming the
/// right type, not match CPython byte for byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteError {
    /// A `Date`/`DateTime`/`Bytes` value with no `default=` to fall back
    /// to (`Object of type <x> is not JSON serializable`).
    Unserializable { type_name: &'static str },
    /// A `Date`/`DateTime`/`Bytes` *dict key* — `json.dumps` stringifies
    /// `str`/`int`/`float`/`bool`/`None` keys but raises on anything else.
    KeyNotStrIntFloatBoolNone { type_name: &'static str },
    /// A `List`/`Dict` dict key: unhashable in Python, so a dict literal
    /// containing one could never have been built in the first place.
    /// Every upstream caller is expected to maintain that invariant; this
    /// variant exists so the writer stays total instead of panicking if
    /// one ever slips through.
    UnhashableKeyType { type_name: &'static str },
    /// `sort_keys=True` tried to compare two original keys of incomparable
    /// types.
    IncomparableKeys {
        left: &'static str,
        right: &'static str,
    },
    /// Nesting beyond [`crate::MAX_DEPTH`] levels deep. Distinct from the
    /// other variants above (all of which mirror a CPython `TypeError`)
    /// for the same reason [`crate::ReadError::Depth`] is distinct from
    /// [`crate::ReadError::Syntax`] — see the crate's module docs.
    Depth,
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::Unserializable { type_name } => {
                write!(f, "Object of type {type_name} is not JSON serializable")
            }
            WriteError::KeyNotStrIntFloatBoolNone { type_name } => {
                write!(
                    f,
                    "keys must be str, int, float, bool or None, not {type_name}"
                )
            }
            WriteError::UnhashableKeyType { type_name } => {
                write!(
                    f,
                    "cannot use '{type_name}' as a dict key (unhashable type: '{type_name}')"
                )
            }
            WriteError::IncomparableKeys { left, right } => {
                write!(
                    f,
                    "'<' not supported between instances of '{left}' and '{right}'"
                )
            }
            WriteError::Depth => {
                write!(
                    f,
                    "exceeded the maximum nesting depth of {}",
                    crate::MAX_DEPTH
                )
            }
        }
    }
}

impl std::error::Error for WriteError {}

/// What happens to a `Value` variant `json.dumps` itself can't encode
/// (`Date`/`DateTime`/`Bytes`): raise, or stringify it with
/// [`Value::py_str`] the way `json.dumps(x, default=str)` does (used by
/// `core/tool.py`'s redacted-`raw` size-cap path `_capped_raw`, its
/// `__str__` methods, the json plugin, and the persistence plugins —
/// `default=str` itself is generic, not one specific caller's quirk;
/// runtime-semantics.md §8.2/DESIGN.md §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnUnsupported {
    Raise,
    DefaultStr,
}

/// `json.dumps(value)`/`json.dumps(value, indent=2, sort_keys=True)` with
/// no `default=`: raises on `Date`/`DateTime`/`Bytes` exactly where Python
/// would. Never appends a trailing newline (the module doc on [`crate`]).
pub fn dumps(value: &Value, mode: WriteMode) -> Result<String, WriteError> {
    let mut out = String::new();
    write_value(value, mode, 0, OnUnsupported::Raise, &mut out)?;
    Ok(out)
}

/// `json.dumps(value, default=str, ...)`: a `Date`/`DateTime`/`Bytes`
/// *value* is stringified via [`Value::py_str`] instead of raising. An
/// unsupported dict *key* still raises — `default=` is never consulted for
/// keys, in Python either.
pub fn dumps_default_str(value: &Value, mode: WriteMode) -> Result<String, WriteError> {
    let mut out = String::new();
    write_value(value, mode, 0, OnUnsupported::DefaultStr, &mut out)?;
    Ok(out)
}

fn write_value(
    value: &Value,
    mode: WriteMode,
    level: usize,
    on_unsupported: OnUnsupported,
    out: &mut String,
) -> Result<(), WriteError> {
    match value {
        Value::None => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(f) => out.push_str(&float_to_json(*f)),
        Value::Str(s) => write_json_string(s, mode.ensure_ascii, out),
        Value::List(items) => write_list(items, mode, level, on_unsupported, out)?,
        Value::Dict(dict) => write_dict(dict, mode, level, on_unsupported, out)?,
        Value::Bytes(_) | Value::Date(_) | Value::DateTime(..) => match on_unsupported {
            OnUnsupported::Raise => {
                return Err(WriteError::Unserializable {
                    type_name: short_type_name(value),
                });
            }
            OnUnsupported::DefaultStr => write_json_string(&value.py_str(), mode.ensure_ascii, out),
        },
    }
    Ok(())
}

/// The class name `json.dumps`'s own `TypeError` names for an
/// unserializable *value* (`"bytes"`/`"date"`/`"datetime"` — the Python
/// `__class__.__name__`, not [`Value::type_name`]'s fully qualified
/// `"datetime.date"`/`"datetime.datetime"`, which is what the *key*-side
/// `TypeError` uses instead; see [`WriteError::KeyNotStrIntFloatBoolNone`]).
fn short_type_name(value: &Value) -> &'static str {
    match value {
        Value::Bytes(_) => "bytes",
        Value::Date(_) => "date",
        Value::DateTime(..) => "datetime",
        _ => unreachable!("short_type_name called on a JSON-representable Value"),
    }
}

fn write_list(
    items: &[Value],
    mode: WriteMode,
    level: usize,
    on_unsupported: OnUnsupported,
    out: &mut String,
) -> Result<(), WriteError> {
    if level >= crate::MAX_DEPTH {
        return Err(WriteError::Depth);
    }
    out.push('[');
    if !items.is_empty() {
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                out.push_str(item_separator(mode));
            }
            newline_indent(mode, level + 1, out);
            write_value(item, mode, level + 1, on_unsupported, out)?;
        }
        newline_indent(mode, level, out);
    }
    out.push(']');
    Ok(())
}

fn write_dict(
    dict: &Dict,
    mode: WriteMode,
    level: usize,
    on_unsupported: OnUnsupported,
    out: &mut String,
) -> Result<(), WriteError> {
    if level >= crate::MAX_DEPTH {
        return Err(WriteError::Depth);
    }
    out.push('{');
    let entries: Vec<(&Value, &Value)> = if mode.sort_keys {
        sorted_items(dict)?
    } else {
        dict.iter().collect()
    };
    if !entries.is_empty() {
        for (i, (key, value)) in entries.into_iter().enumerate() {
            if i > 0 {
                out.push_str(item_separator(mode));
            }
            newline_indent(mode, level + 1, out);
            let key_text = stringify_key(key)?;
            write_json_string(&key_text, mode.ensure_ascii, out);
            out.push_str(": ");
            write_value(value, mode, level + 1, on_unsupported, out)?;
        }
        newline_indent(mode, level, out);
    }
    out.push('}');
    Ok(())
}

/// Python's non-`str` dict-key stringification (DESIGN.md §3.4): `True`/
/// `False` → `"true"`/`"false"`, `None` → `"null"`, an int via its decimal
/// digits, a float through the same [`float_to_json`] rule as a value
/// (`NaN`/`Infinity` included — confirmed against CPython directly).
/// `Bytes`/`Date`/`DateTime` and `List`/`Dict` keys raise, matching
/// `json.dumps`'s own `TypeError`s for each.
fn stringify_key(key: &Value) -> Result<String, WriteError> {
    match key {
        Value::Str(s) => Ok(s.clone()),
        Value::Bool(b) => Ok(if *b { "true" } else { "false" }.to_string()),
        Value::None => Ok("null".to_string()),
        Value::Int(i) => Ok(i.to_string()),
        Value::Float(f) => Ok(float_to_json(*f)),
        Value::Bytes(_) | Value::Date(_) | Value::DateTime(..) => {
            Err(WriteError::KeyNotStrIntFloatBoolNone {
                type_name: key.type_name(),
            })
        }
        Value::List(_) | Value::Dict(_) => Err(WriteError::UnhashableKeyType {
            type_name: key.type_name(),
        }),
    }
}

/// `json.dumps`'s float encoding: `NaN`/`Infinity`/`-Infinity` for
/// non-finite values (never serde_json's `null`), otherwise exactly
/// `electricity_value`'s own `repr(float)` layout — confirmed identical to
/// `json.dumps` for every finite float directly against CPython.
fn float_to_json(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_string()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
    } else {
        Value::Float(f).py_str()
    }
}

/// `sorted(dct.items())` compares `(key, value)` tuples, but two distinct
/// entries in a `Dict` are never Python-equal except when both keys are
/// `NaN` (the only way `Dict` ever holds "equal" keys twice — any other
/// pair of equal keys would have collapsed to one entry when the `Dict`
/// was built). Tuple comparison only consults the second element once the
/// first compares equal, and a `NaN` key is never equal even to another
/// `NaN` (`nan == nan` is `False`), so CPython's own tuple comparison for
/// `sorted()` only ever looks at keys here, never values: this sorts by
/// key alone, before any key is stringified, so e.g. `{1: ..., 2: ...,
/// 10: ...}` sorts numerically, never lexicographically (DESIGN.md §3.4).
/// Raises [`WriteError::IncomparableKeys`] the same way CPython's
/// `sorted()` raises `TypeError` for an incomparable pair; exactly which
/// pair a comparison-sort happens to compare first when several
/// incomparable pairs exist is an implementation detail this doesn't try
/// to match (the message text doesn't have to, either — DESIGN.md §1/§12).
///
/// Dispatches on whether any key is a `NaN` float, since that's the only
/// way `py_partial_cmp` ever returns `Ok(None)` ("unordered", not an
/// error) for a `Dict`'s keys, and the only case CPython's own sort
/// doesn't reduce to a plain total order:
/// - no `NaN` key: every key pair has a real order (or raises), so a
///   single stable O(n log n) sort reproduces CPython's output exactly
///   — any correct stable sort over a total order gives the same result.
/// - a `NaN` key, fewer than 64 entries: CPython never promotes such a
///   short list to a full merge sort, so [`cpython_small_sort`] ports its
///   `count_run`/`binarysort` faithfully instead.
/// - a `NaN` key, 64 or more entries: a documented divergence (below).
fn sorted_items(dict: &Dict) -> Result<Vec<(&Value, &Value)>, WriteError> {
    let mut items: Vec<(&Value, &Value)> = dict.iter().collect();
    let has_nan_key = items
        .iter()
        .any(|(key, _)| matches!(key, Value::Float(f) if f.is_nan()));
    if !has_nan_key {
        sort_total_order(&mut items)?;
    } else if items.len() < 64 {
        cpython_small_sort(&mut items)?;
    } else {
        // Known divergence: CPython's full timsort merge for 64+ elements
        // with a NaN key isn't ported here. This produces *some* stable,
        // defined order (every NaN key compares equal to everything, as
        // in the under-64 port's spirit) rather than reproducing CPython's
        // exact key order for this case.
        sort_with_nan_as_equal(&mut items)?;
    }
    Ok(items)
}

/// Python `<` between two dict keys, for sort purposes: `Ok(false)` both
/// when `a` is genuinely not less than `b` and when the pair is `NaN`-
/// unordered (`py_partial_cmp` returning `Ok(None)`) — matching Python's
/// own `<` on a `NaN` operand, which is always `False`, never a raise.
fn py_lt(a: &Value, b: &Value) -> Result<bool, WriteError> {
    match a.py_partial_cmp(b) {
        Ok(ordering) => Ok(ordering == Some(Ordering::Less)),
        Err(_) => Err(WriteError::IncomparableKeys {
            left: a.type_name(),
            right: b.type_name(),
        }),
    }
}

/// A single stable O(n log n) sort by key, for a dict with no `NaN` key:
/// every key pair then has a real order or raises, so any correct stable
/// sort reproduces CPython's own `sorted(dct.items())` key order exactly.
/// `sort_by`'s comparator can't itself return a `Result`, so an
/// incomparable pair is recorded in `error` and all later comparisons
/// return `Ordering::Equal` (making the rest of the sort a no-op) rather
/// than panicking — the final key order is discarded by returning `Err`.
fn sort_total_order(items: &mut [(&Value, &Value)]) -> Result<(), WriteError> {
    let mut error: Option<WriteError> = None;
    items.sort_by(|a, b| {
        if error.is_some() {
            return Ordering::Equal;
        }
        match a.0.py_partial_cmp(b.0) {
            Ok(Some(ordering)) => ordering,
            Ok(None) => unreachable!("sort_total_order called with no NaN key present"),
            Err(_) => {
                error = Some(WriteError::IncomparableKeys {
                    left: a.0.type_name(),
                    right: b.0.type_name(),
                });
                Ordering::Equal
            }
        }
    });
    match error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// A faithful port of CPython 3.11's `listobject.c` `count_run` then
/// `binarysort` — the exact algorithm `list.sort`/`sorted()` use for a
/// list short enough (under 64 elements) to never need a full timsort
/// merge, confirmed directly against CPython for the `NaN`-key
/// counterexample this is written for (DESIGN.md's sort-algorithm
/// decision for #377/#384): keys in insertion order `5.0, 6.0, 7.0, NaN,
/// 8.0, 1.0` sort to `1.0, 5.0, 6.0, 7.0, NaN, 8.0` — `count_run` finds
/// the initial run `[5.0, 6.0, 7.0, NaN, 8.0]` (ascending, since `NaN <
/// x` and `x < NaN` are both always `False`, so neither comparison ever
/// breaks the run), then `binarysort` binary-inserts `1.0` at the front.
/// A plain comparison-sort comparator (as in [`sort_total_order`]) can't
/// reproduce this: it would stop at `1.0 < NaN` being `False` and treat
/// the two as adjacent-equal, landing `1.0` right before `8.0` instead.
fn cpython_small_sort(items: &mut [(&Value, &Value)]) -> Result<(), WriteError> {
    let n = items.len();
    if n < 2 {
        return Ok(());
    }
    let run = count_run(items)?;
    binary_insertion_sort(items, run)
}

/// `count_run`: the length of the initial monotone run starting at index
/// 0 — strictly descending (then reversed in place) or non-decreasing,
/// decided by comparing the first two elements, exactly as CPython's own
/// `count_run` does.
fn count_run(items: &mut [(&Value, &Value)]) -> Result<usize, WriteError> {
    let n = items.len();
    if n < 2 {
        return Ok(n);
    }
    if py_lt(items[1].0, items[0].0)? {
        let mut run = 2;
        while run < n && py_lt(items[run].0, items[run - 1].0)? {
            run += 1;
        }
        items[0..run].reverse();
        Ok(run)
    } else {
        let mut run = 2;
        while run < n && !py_lt(items[run].0, items[run - 1].0)? {
            run += 1;
        }
        Ok(run)
    }
}

/// `binarysort`: given that `items[..ok_as_is]` is already sorted, insert
/// each remaining element with a one-sided binary search for its
/// insertion point (stable: an element lands after every already-placed
/// element it isn't strictly less than), exactly as CPython's own
/// `binarysort` does.
fn binary_insertion_sort(
    items: &mut [(&Value, &Value)],
    ok_as_is: usize,
) -> Result<(), WriteError> {
    let n = items.len();
    for a in ok_as_is..n {
        let pivot = items[a];
        let mut l = 0usize;
        let mut r = a;
        while l < r {
            let p = l + (r - l) / 2;
            if py_lt(pivot.0, items[p].0)? {
                r = p;
            } else {
                l = p + 1;
            }
        }
        items.copy_within(l..a, l + 1);
        items[l] = pivot;
    }
    Ok(())
}

/// Known divergence (see [`sorted_items`]): for a dict with 64 or more
/// keys including a `NaN` key, every `NaN` key sorts as equal to every
/// other key — a stable sort, but not CPython's own order for this case.
fn sort_with_nan_as_equal(items: &mut [(&Value, &Value)]) -> Result<(), WriteError> {
    let mut error: Option<WriteError> = None;
    items.sort_by(|a, b| {
        if error.is_some() {
            return Ordering::Equal;
        }
        match a.0.py_partial_cmp(b.0) {
            Ok(Some(ordering)) => ordering,
            Ok(None) => Ordering::Equal,
            Err(_) => {
                error = Some(WriteError::IncomparableKeys {
                    left: a.0.type_name(),
                    right: b.0.type_name(),
                });
                Ordering::Equal
            }
        }
    });
    match error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn item_separator(mode: WriteMode) -> &'static str {
    if mode.indent.is_some() { "," } else { ", " }
}

fn newline_indent(mode: WriteMode, level: usize, out: &mut String) {
    if let Some(width) = mode.indent {
        out.push('\n');
        out.push_str(&" ".repeat(width as usize * level));
    }
}

/// Escapes exactly as CPython's `json.encoder.py_encode_basestring`
/// (`ensure_ascii=False`) / `py_encode_basestring_ascii`
/// (`ensure_ascii=True`): `\\`/`\"` and the C0 control range `0x00..0x1f`
/// always get a named escape (`\b`/`\f`/`\n`/`\r`/`\t`) or `\u00XX`;
/// `ensure_ascii` additionally escapes every codepoint outside the
/// printable-ASCII range `0x20..=0x7e` as `\uXXXX`, with a UTF-16
/// surrogate pair above `0xffff` — confirmed against CPython's own
/// `ESCAPE`/`ESCAPE_ASCII`/`ESCAPE_DCT` tables directly.
fn write_json_string(s: &str, ensure_ascii: bool, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        if let Some(escape) = named_escape(c) {
            out.push_str(escape);
            continue;
        }
        let cp = c as u32;
        if cp < 0x20 {
            out.push_str(&format!("\\u{cp:04x}"));
        } else if ensure_ascii && cp > 0x7e {
            if cp < 0x10000 {
                out.push_str(&format!("\\u{cp:04x}"));
            } else {
                let n = cp - 0x10000;
                let high = 0xd800 | (n >> 10);
                let low = 0xdc00 | (n & 0x3ff);
                out.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
            }
        } else {
            out.push(c);
        }
    }
    out.push('"');
}

fn named_escape(c: char) -> Option<&'static str> {
    match c {
        '\\' => Some("\\\\"),
        '"' => Some("\\\""),
        '\u{8}' => Some("\\b"),
        '\u{c}' => Some("\\f"),
        '\n' => Some("\\n"),
        '\r' => Some("\\r"),
        '\t' => Some("\\t"),
        _ => None,
    }
}
