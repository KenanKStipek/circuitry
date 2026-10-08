//! A dedicated `Value` → text writer matching `json.dumps` exactly
//! (DESIGN.md §3.4, rust-ecosystem.md item 22), rather than bending
//! `serde_json`'s `ryu`-based formatter and ASCII-only string encoder to
//! Python's rules.

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
    /// `sort_keys=True` tried to compare two original keys (or, when keys
    /// are equal, two values for those keys — CPython's `sorted(dct.items())`
    /// sorts the full `(key, value)` tuples) of incomparable types.
    IncomparableKeys {
        left: &'static str,
        right: &'static str,
    },
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
        }
    }
}

impl std::error::Error for WriteError {}

/// What happens to a `Value` variant `json.dumps` itself can't encode
/// (`Date`/`DateTime`/`Bytes`): raise (every write site except one), or
/// stringify it with [`Value::py_str`] the way `json.dumps(x, default=str)`
/// does (the redacted-`raw` size-cap path, `cli/tool.py`'s `_capped_raw`,
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

/// `sorted(dct.items())`: sorts the **original** `(key, value)` tuples —
/// comparing keys first and only falling through to values when two keys
/// are themselves equal (`1`/`1.0`/`True` collide as dict keys, but need
/// not collide as *values*) — before any key is stringified, so e.g.
/// `{1: ..., 2: ..., 10: ...}` sorts numerically, never lexicographically
/// (DESIGN.md §3.4). Raises [`WriteError::IncomparableKeys`] the same way
/// CPython's `sorted()` raises `TypeError` for an incomparable pair;
/// exactly which pair a comparison-sort happens to compare first when
/// several incomparable pairs exist is an implementation detail this
/// doesn't try to match (the message text doesn't have to, either —
/// DESIGN.md §1/§12).
fn sorted_items(dict: &Dict) -> Result<Vec<(&Value, &Value)>, WriteError> {
    let mut items: Vec<(&Value, &Value)> = dict.iter().collect();
    let mut error: Option<WriteError> = None;
    items.sort_by(|a, b| {
        if error.is_some() {
            return Ordering::Equal;
        }
        match tuple_py_cmp(a, b) {
            Ok(ordering) => ordering,
            Err(e) => {
                error = Some(e);
                Ordering::Equal
            }
        }
    });
    match error {
        Some(e) => Err(e),
        None => Ok(items),
    }
}

fn tuple_py_cmp(a: &(&Value, &Value), b: &(&Value, &Value)) -> Result<Ordering, WriteError> {
    let key_ordering = py_cmp(a.0, b.0)?;
    if key_ordering != Ordering::Equal {
        return Ok(key_ordering);
    }
    py_cmp(a.1, b.1)
}

/// A total order over `Value` for sort purposes: `Ok(None)` (one side is
/// `NaN`, numerically unordered) is folded to `Ordering::Equal` so the
/// sort stays well-defined — Python's own sort order for a `NaN` key is
/// itself not a meaningful contract to reproduce exactly.
fn py_cmp(a: &Value, b: &Value) -> Result<Ordering, WriteError> {
    match a.py_partial_cmp(b) {
        Ok(Some(ordering)) => Ok(ordering),
        Ok(None) => Ok(Ordering::Equal),
        Err(_) => Err(WriteError::IncomparableKeys {
            left: a.type_name(),
            right: b.type_name(),
        }),
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
