//! A `json.loads`-equivalent reader, hand-rolled (not `serde_json::Value`)
//! so it can match CPython's own `json` module exactly: unbounded
//! integers, `NaN`/`Infinity`/`-Infinity` literals, and Circuitry's own
//! `core/json_load.py` `DuplicateKeyError` naming the dotted path of the
//! first repeated key (runtime-semantics.md §1.2).
//!
//! Positions in [`ReadError::Syntax`] are Python `str` indices (Unicode
//! scalar values, matching `JSONDecodeError.pos`) — the reader walks a
//! `Vec<char>`, not raw bytes, specifically so this holds for any input
//! containing non-ASCII text before the error.
//!
//! Syntax-error *messages* here come from mirroring CPython's own
//! `json.decoder`, not from Circuitry's own code, so (DESIGN.md §1/§12)
//! they only need to fail at the same position with a non-empty message;
//! [`ReadError::DuplicateKey`]'s message is Circuitry's own and matches
//! `core/json_load.py` word for word.
//!
//! Known difference from CPython 3.11, inherited from `electricity-value`
//! (see its own module doc): an integer literal longer than 4300 decimal
//! digits parses successfully here, where CPython's `int(...)` raises
//! `ValueError` (`sys.set_int_max_str_digits`).

use electricity_value::{Dict, IntValue, Value};
use std::fmt;

/// Why [`loads`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// Malformed JSON syntax. `pos` is the Python `str`-index CPython's
    /// `json.loads` would report for the same input; `message` is
    /// descriptive but not guaranteed byte-identical to CPython's (that
    /// text is third-party, not Circuitry's own — DESIGN.md §1/§12).
    Syntax { message: String, pos: usize },
    /// A JSON object defines the same key twice — `message` matches
    /// `core/json_load.py`'s `DuplicateKeyError` word for word.
    DuplicateKey { message: String },
    /// Nesting beyond [`crate::MAX_DEPTH`] `{`/`[` levels deep. `pos` is
    /// the index of the opening bracket that would have exceeded the
    /// limit. Distinct from [`ReadError::Syntax`] on purpose — see the
    /// crate's module docs on why a caller must not treat the two the
    /// same way CPython's `RecursionError` and `JSONDecodeError` differ.
    Depth { pos: usize },
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Syntax { message, pos } => write!(f, "{message}: char {pos}"),
            ReadError::DuplicateKey { message } => write!(f, "{message}"),
            ReadError::Depth { pos } => write!(
                f,
                "exceeded the maximum nesting depth of {} at char {pos}",
                crate::MAX_DEPTH
            ),
        }
    }
}

impl std::error::Error for ReadError {}

/// Plain `json.loads(text)` into a [`Value`]: an object literal with a
/// repeated key silently keeps only the last value, exactly like CPython's
/// `json.loads`. Every call site except the orchestration-document loader
/// uses this (`core/tool.py`'s `params_json` and `_capped_raw`,
/// `plugins/json.py`, `core/prompt.py`'s model-reply parsing, `cli/app.py`'s
/// `-e` values and state stores). Use [`load_json`] for `.json`
/// orchestration documents, which need the stricter, path-naming
/// duplicate-key error instead (runtime-semantics.md §1.2).
pub fn loads(text: &str) -> Result<Value, ReadError> {
    let raw = parse(text)?;
    Ok(materialize_last_wins(raw))
}

/// `core/json_load.load_json(text)`: like [`loads`], but raises
/// [`ReadError::DuplicateKey`] naming the dotted path of the first
/// repeated object key instead of silently keeping the last value. Every
/// `.json` orchestration document the runtime loads — a file, a library
/// ref, a `use` child loaded by `path:` — goes through this, never plain
/// [`loads`] (`cli/orchestration_loader.py`).
pub fn load_json(text: &str) -> Result<Value, ReadError> {
    let raw = parse(text)?;
    materialize_duplicate_checked(raw, "")
}

fn parse(text: &str) -> Result<Raw, ReadError> {
    let chars: Vec<char> = text.chars().collect();
    let parser = Parser { chars: &chars };
    let start = parser.skip_ws(0);
    let (raw, end) = parser.parse_value(start, 0)?;
    let end = parser.skip_ws(end);
    if end != chars.len() {
        return Err(parser.syntax("Extra data", end));
    }
    Ok(raw)
}

/// One JSON object's or array's contents, shared by both materializers
/// (not yet checked for duplicate keys for [`load_json`]'s sake) — mirrors
/// `core/json_load.py`'s `_RawObject`: built bottom-up by the parser, then
/// walked top-down by [`materialize_duplicate_checked`] so each key's
/// dotted path is known by the time a duplicate is found.
enum Raw {
    Null,
    Bool(bool),
    Int(IntValue),
    Float(f64),
    Str(String),
    List(Vec<Raw>),
    Object(Vec<(String, Raw)>),
}

/// [`loads`]'s materialization: no path tracking, no duplicate check — a
/// repeated key's later value simply overwrites the earlier one in place
/// (`Dict::insert`, an `IndexMap`, keeps the key's first position and
/// updates the value on a repeat insert), exactly matching a Python dict
/// literal with a repeated key.
fn materialize_last_wins(raw: Raw) -> Value {
    match raw {
        Raw::Null => Value::None,
        Raw::Bool(b) => Value::Bool(b),
        Raw::Int(i) => Value::Int(i),
        Raw::Float(f) => Value::Float(f),
        Raw::Str(s) => Value::Str(s),
        Raw::List(items) => Value::List(items.into_iter().map(materialize_last_wins).collect()),
        Raw::Object(pairs) => {
            let mut result: Dict = Dict::new();
            for (key, value) in pairs {
                result.insert(Value::Str(key), materialize_last_wins(value));
            }
            Value::Dict(result)
        }
    }
}

/// [`load_json`]'s materialization: mirrors `core/json_load.py`'s own
/// `_materialize`, walking top-down so each key's dotted path is known by
/// the time a duplicate is found.
fn materialize_duplicate_checked(raw: Raw, path: &str) -> Result<Value, ReadError> {
    match raw {
        Raw::Null => Ok(Value::None),
        Raw::Bool(b) => Ok(Value::Bool(b)),
        Raw::Int(i) => Ok(Value::Int(i)),
        Raw::Float(f) => Ok(Value::Float(f)),
        Raw::Str(s) => Ok(Value::Str(s)),
        Raw::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.into_iter().enumerate() {
                out.push(materialize_duplicate_checked(
                    item,
                    &format!("{path}[{i}]"),
                )?);
            }
            Ok(Value::List(out))
        }
        Raw::Object(pairs) => {
            let mut result: Dict = Dict::new();
            for (key, value) in pairs {
                if result.contains_key(&Value::Str(key.clone())) {
                    let location = if path.is_empty() { "top level" } else { path };
                    let message = format!(
                        "duplicate key {} in {location}; JSON would silently keep only the last one",
                        Value::Str(key).py_repr(),
                    );
                    return Err(ReadError::DuplicateKey { message });
                }
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                let materialized = materialize_duplicate_checked(value, &child_path)?;
                result.insert(Value::Str(key), materialized);
            }
            Ok(Value::Dict(result))
        }
    }
}

struct Parser<'a> {
    chars: &'a [char],
}

impl Parser<'_> {
    fn syntax(&self, message: &str, pos: usize) -> ReadError {
        ReadError::Syntax {
            message: message.to_string(),
            pos,
        }
    }

    fn skip_ws(&self, mut idx: usize) -> usize {
        while matches!(self.chars.get(idx), Some(' ' | '\t' | '\n' | '\r')) {
            idx += 1;
        }
        idx
    }

    fn starts_with(&self, idx: usize, literal: &str) -> bool {
        let literal: Vec<char> = literal.chars().collect();
        idx + literal.len() <= self.chars.len()
            && self.chars[idx..idx + literal.len()] == literal[..]
    }

    /// `json.scanner._scan_once`: dispatches on the first character, then
    /// the `NUMBER_RE` match, then the `NaN`/`Infinity`/`-Infinity`
    /// literals (outside the JSON spec, but `json.loads` accepts them by
    /// default). `depth` is the current `{`/`[` nesting depth, threaded
    /// through to [`Parser::parse_object`]/[`Parser::parse_array`] so
    /// nesting past [`crate::MAX_DEPTH`] returns [`ReadError::Depth`]
    /// instead of recursing further (DESIGN.md's depth-limit decision).
    fn parse_value(&self, idx: usize, depth: usize) -> Result<(Raw, usize), ReadError> {
        let Some(&c) = self.chars.get(idx) else {
            return Err(self.syntax("Expecting value", idx));
        };
        match c {
            '"' => {
                let (s, end) = self.parse_string(idx + 1)?;
                Ok((Raw::Str(s), end))
            }
            '{' => self.parse_object(idx + 1, depth),
            '[' => self.parse_array(idx + 1, depth),
            'n' if self.starts_with(idx, "null") => Ok((Raw::Null, idx + 4)),
            't' if self.starts_with(idx, "true") => Ok((Raw::Bool(true), idx + 4)),
            'f' if self.starts_with(idx, "false") => Ok((Raw::Bool(false), idx + 5)),
            'N' if self.starts_with(idx, "NaN") => Ok((Raw::Float(f64::NAN), idx + 3)),
            'I' if self.starts_with(idx, "Infinity") => Ok((Raw::Float(f64::INFINITY), idx + 8)),
            '-' if self.starts_with(idx, "-Infinity") => {
                Ok((Raw::Float(f64::NEG_INFINITY), idx + 9))
            }
            _ => {
                if let Some((text, end, has_frac, has_exp)) = self.match_number(idx) {
                    if has_frac || has_exp {
                        let f: f64 = text
                            .parse()
                            .map_err(|_| self.syntax("Expecting value", idx))?;
                        Ok((Raw::Float(f), end))
                    } else {
                        let n = IntValue::parse_decimal(&text)
                            .ok_or_else(|| self.syntax("Expecting value", idx))?;
                        Ok((Raw::Int(n), end))
                    }
                } else {
                    Err(self.syntax("Expecting value", idx))
                }
            }
        }
    }

    /// `json.scanner.NUMBER_RE`:
    /// `(-?(?:0|[1-9][0-9]*))(\.[0-9]+)?([eE][-+]?[0-9]+)?`, matched
    /// (not necessarily to the end of input) at `start`. Returns the
    /// matched text, the end index, and whether a fraction/exponent were
    /// present (which decides `int` vs. `float`, same as `json.scanner`).
    fn match_number(&self, start: usize) -> Option<(String, usize, bool, bool)> {
        let mut idx = start;
        if self.chars.get(idx) == Some(&'-') {
            idx += 1;
        }
        match self.chars.get(idx) {
            Some('0') => idx += 1,
            Some(c) if c.is_ascii_digit() => {
                idx += 1;
                while matches!(self.chars.get(idx), Some(c) if c.is_ascii_digit()) {
                    idx += 1;
                }
            }
            _ => return None,
        }

        let mut has_frac = false;
        if self.chars.get(idx) == Some(&'.') {
            let digit_start = idx + 1;
            let mut j = digit_start;
            while matches!(self.chars.get(j), Some(c) if c.is_ascii_digit()) {
                j += 1;
            }
            if j > digit_start {
                idx = j;
                has_frac = true;
            }
        }

        let mut has_exp = false;
        if matches!(self.chars.get(idx), Some('e' | 'E')) {
            let mut j = idx + 1;
            if matches!(self.chars.get(j), Some('+' | '-')) {
                j += 1;
            }
            let digit_start = j;
            while matches!(self.chars.get(j), Some(c) if c.is_ascii_digit()) {
                j += 1;
            }
            if j > digit_start {
                idx = j;
                has_exp = true;
            }
        }

        Some((
            self.chars[start..idx].iter().collect(),
            idx,
            has_frac,
            has_exp,
        ))
    }

    /// `json.decoder.py_scanstring`'s algorithm, with positions confirmed
    /// against the C-accelerated `json.loads` CPython actually runs by
    /// default (`_json.scanstring`), which isn't always the same position
    /// as the pure-Python fallback's own source would suggest — e.g. an
    /// unrecognized `\x` escape is reported at the backslash, not at `x`,
    /// confirmed directly. `start` is the index right after the opening
    /// quote (mirroring the Python function's own `end` param).
    fn parse_string(&self, start: usize) -> Result<(String, usize), ReadError> {
        let begin = start - 1;
        let mut result = String::new();
        let mut idx = start;
        loop {
            match self.chars.get(idx) {
                None => return Err(self.syntax("Unterminated string starting at", begin)),
                Some('"') => return Ok((result, idx + 1)),
                Some('\\') => {
                    let backslash_idx = idx;
                    idx += 1;
                    let Some(&esc) = self.chars.get(idx) else {
                        return Err(self.syntax("Unterminated string starting at", begin));
                    };
                    if esc == 'u' {
                        let (code, next) = self.decode_u_escape(idx)?;
                        idx = next;
                        if (0xd800..=0xdbff).contains(&code) {
                            if let Some((ch, next2)) = self.try_surrogate_pair(code, idx) {
                                result.push(ch);
                                idx = next2;
                                continue;
                            }
                            // A lone (or unpaired) surrogate: valid in a
                            // Python `str`, not representable in a Rust
                            // `String` (not a Unicode scalar value). See the
                            // crate's module docs for what this diverges on
                            // downstream (every write path Circuitry uses).
                            result.push('\u{fffd}');
                            continue;
                        }
                        match char::from_u32(code) {
                            Some(ch) => result.push(ch),
                            None => result.push('\u{fffd}'),
                        }
                    } else {
                        let mapped = match esc {
                            '"' => '"',
                            '\\' => '\\',
                            '/' => '/',
                            'b' => '\u{8}',
                            'f' => '\u{c}',
                            'n' => '\n',
                            'r' => '\r',
                            't' => '\t',
                            other => {
                                return Err(self.syntax(
                                    &format!("Invalid \\escape: {other:?}"),
                                    backslash_idx,
                                ));
                            }
                        };
                        result.push(mapped);
                        idx += 1;
                    }
                }
                Some(&c) if (c as u32) < 0x20 => {
                    return Err(self.syntax(&format!("Invalid control character {c:?} at"), idx));
                }
                Some(&c) => {
                    result.push(c);
                    idx += 1;
                }
            }
        }
    }

    /// `pos` is the index of the `u` itself (matching
    /// `json.decoder._decode_uXXXX`'s own `pos` argument). Two things
    /// CPython's own `_json.c` scanner checks that are easy to miss
    /// reimplementing this by hand, both confirmed directly against
    /// CPython 3.11:
    /// - the 4 escape characters must be `[0-9a-fA-F]` *only* — unlike
    ///   Rust's own `u32::from_str_radix(_, 16)`, CPython never accepts a
    ///   sign, so e.g. `\u+041` is "Invalid \uXXXX escape", not `A`;
    /// - the check is `start + 4 >= len`, not `> len`: a `\uXXXX` escape
    ///   with nothing at all after its 4th hex digit (not even the
    ///   closing quote) is also "Invalid \uXXXX escape" at `pos`, not
    ///   "Unterminated string" at the opening quote.
    fn decode_u_escape(&self, pos: usize) -> Result<(u32, usize), ReadError> {
        let start = pos + 1;
        if start + 4 >= self.chars.len() {
            return Err(self.syntax("Invalid \\uXXXX escape", pos));
        }
        let hex = &self.chars[start..start + 4];
        if !hex.iter().all(char::is_ascii_hexdigit) {
            return Err(self.syntax("Invalid \\uXXXX escape", pos));
        }
        let hex: String = hex.iter().collect();
        u32::from_str_radix(&hex, 16)
            .map(|v| (v, start + 4))
            .map_err(|_| self.syntax("Invalid \\uXXXX escape", pos))
    }

    /// `idx` is right after a high surrogate's `\uXXXX`. `None` means "no
    /// pair" (a malformed or absent low surrogate still only costs its own
    /// escape, never raises — `py_scanstring` itself never fails on an
    /// unpaired surrogate).
    fn try_surrogate_pair(&self, high: u32, idx: usize) -> Option<(char, usize)> {
        if self.chars.get(idx) != Some(&'\\') || self.chars.get(idx + 1) != Some(&'u') {
            return None;
        }
        let (low, next) = self.decode_u_escape(idx + 1).ok()?;
        if !(0xdc00..=0xdfff).contains(&low) {
            return None;
        }
        let combined = 0x10000 + (((high - 0xd800) << 10) | (low - 0xdc00));
        char::from_u32(combined).map(|ch| (ch, next))
    }

    /// `json.decoder.JSONArray` (CPython 3.11: no "illegal trailing comma"
    /// special case — that was added in a later Python version. A
    /// trailing comma here loops back into [`Parser::parse_value`] at
    /// the `]`, which rejects it as "Expecting value", exactly like
    /// 3.11 itself, confirmed directly). `start` is right after the
    /// opening `[`; `depth` is this array's own nesting depth (0 for a
    /// top-level array), checked against [`crate::MAX_DEPTH`] before any
    /// of its elements are parsed.
    fn parse_array(&self, start: usize, depth: usize) -> Result<(Raw, usize), ReadError> {
        if depth >= crate::MAX_DEPTH {
            return Err(ReadError::Depth { pos: start - 1 });
        }
        let mut idx = self.skip_ws(start);
        if self.chars.get(idx) == Some(&']') {
            return Ok((Raw::List(Vec::new()), idx + 1));
        }
        let mut items = Vec::new();
        loop {
            let (value, next) = self.parse_value(idx, depth + 1)?;
            items.push(value);
            idx = self.skip_ws(next);
            match self.chars.get(idx) {
                Some(']') => return Ok((Raw::List(items), idx + 1)),
                Some(',') => idx = self.skip_ws(idx + 1),
                _ => return Err(self.syntax("Expecting ',' delimiter", idx)),
            }
        }
    }

    /// `json.decoder.JSONObject` (see [`Parser::parse_array`] on 3.11's
    /// lack of a trailing-comma special case: here it surfaces as
    /// "Expecting property name enclosed in double quotes" at the `}`,
    /// confirmed directly). `start` is right after the opening `{`;
    /// `depth` is this object's own nesting depth, checked the same way
    /// as [`Parser::parse_array`]'s.
    fn parse_object(&self, start: usize, depth: usize) -> Result<(Raw, usize), ReadError> {
        if depth >= crate::MAX_DEPTH {
            return Err(ReadError::Depth { pos: start - 1 });
        }
        let mut idx = self.skip_ws(start);
        if self.chars.get(idx) == Some(&'}') {
            return Ok((Raw::Object(Vec::new()), idx + 1));
        }
        if self.chars.get(idx) != Some(&'"') {
            return Err(self.syntax("Expecting property name enclosed in double quotes", idx));
        }
        let mut pairs = Vec::new();
        loop {
            let (key, next) = self.parse_string(idx + 1)?;
            idx = self.skip_ws(next);
            if self.chars.get(idx) != Some(&':') {
                return Err(self.syntax("Expecting ':' delimiter", idx));
            }
            idx = self.skip_ws(idx + 1);
            let (value, next) = self.parse_value(idx, depth + 1)?;
            pairs.push((key, value));
            idx = self.skip_ws(next);
            match self.chars.get(idx) {
                Some('}') => return Ok((Raw::Object(pairs), idx + 1)),
                Some(',') => {
                    idx = self.skip_ws(idx + 1);
                    if self.chars.get(idx) != Some(&'"') {
                        return Err(
                            self.syntax("Expecting property name enclosed in double quotes", idx)
                        );
                    }
                }
                _ => return Err(self.syntax("Expecting ',' delimiter", idx)),
            }
        }
    }
}
