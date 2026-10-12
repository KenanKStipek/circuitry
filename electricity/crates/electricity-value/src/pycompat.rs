//! Small CPython-compatibility helpers shared by several M1 lanes
//! (issue #449's gate lane, item 10) -- each crate that needs one of
//! these today re-implements its own narrow copy (`electricity-tools::
//! json`'s own `python_int`, `electricity-compiler::pipeline`'s own
//! ASCII-only `is_python_strip_whitespace`); a shared, more complete
//! version here lets lanes C/D1/D2/E/F2/G/K1 converge on one
//! implementation instead of each growing its own.
//!
//! [`py_int`] and [`py_strip`] are real, final implementations --
//! small enough, and specified precisely enough, that lane A can just
//! finish them. [`shlex_split`]/[`shlex_quote`] (lane C) and
//! [`urlencode`] (lane E) are signature-only stubs: a not-implemented
//! error, never a fake result.

use crate::Value;
use std::fmt;

/// A Python exception class name -- the one canonical copy every
/// crate that needs to report one builds on (issue #449's gate lane;
/// review finding 1 on PR #454 named `electricity-tools::PyExc` and
/// `electricity-vm::adapter::PyExc` as two copies of the same thing,
/// kept apart by a dependency-direction rule that doesn't actually
/// hold -- `electricity-vm` already depends on `electricity-tools`).
/// Not exhaustive of every exception class in Circuitry's own plugin
/// catalog -- a later lane widens this enum (a new variant, never a
/// change to an existing one's spelling) the first time a ported
/// plugin/adapter actually needs to raise one not listed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyExc {
    ValueError,
    /// `core/templates.py::TemplateError` (a `ValueError` subclass) --
    /// lane P (#448, #487): a run-time `{{> name}}` expansion/render
    /// failure under `on_error: fail`. Inserted directly after
    /// `ValueError`, never at the enum's own tail, so a concurrent
    /// lane appending its own variant there (lane E, #485) can never
    /// collide with this one on the same or an adjacent line.
    TemplateError,
    /// `core/answers.py::AnswerParseError` (lane G, issue #498) -- an
    /// unreadable `boolean`/`number` reply. Inserted directly after
    /// `TemplateError`, never at the enum's own tail (where lane E
    /// appended its own variants), so a concurrent lane appending there
    /// can never collide with this one.
    AnswerParseError,
    /// `core/prompt.py::SchemaValidationError` (lane G, issue #498) -- a
    /// prompt's decoded output failed its declared JSON schema.
    SchemaValidationError,
    /// `allowlist_gate.py::AllowlistError` (lane G, issue #498) -- a
    /// `provider:`/fallback outside the installed adapter allowlist.
    AllowlistError,
    RuntimeError,
    TypeError,
    KeyError,
    LookupError,
    PermissionError,
    FileNotFoundError,
    IsADirectoryError,
    NotADirectoryError,
    OSError,
    ConnectionError,
    TimeoutError,
    UnicodeDecodeError,
    /// `int(float("inf"))`/`int(float("nan"))`'s own exception class
    /// (review finding 5 on PR #454) -- CPython raises this, not
    /// `ValueError`, for a non-finite float.
    OverflowError,
    /// `core/use.py`'s own cycle-detection raise (M1 lane I, issue
    /// #469): a `use` effect whose child identity is already on the
    /// VM's own use-call-stack (`electricity_vm::Frame::
    /// use_call_stack`) raises this, not `ValueError` -- still not
    /// retried, the same as `ValueError`.
    RecursionError,
    /// `str.encode(encoding)`'s own strict-mode failure (lane D1's
    /// `fs`/`hash` tools: appended here, never a change to an
    /// existing variant's spelling, per this enum's own doc comment).
    UnicodeEncodeError,
    /// `IndexError` (lane D2, the `regex` tool's own `sub` template
    /// parser): CPython's `re.sub` raises this, not `ValueError`, for
    /// an unknown `\g<name>` group name -- distinct from
    /// [`PyExc::LookupError`], which is the *class name* `re.error`
    /// itself subclasses, never the one #442's multi-failure tree text
    /// needs to print for this specific case.
    IndexError,
    /// `re.error` (lane D2, the `regex` tool): CPython 3.11/3.12's own
    /// name for every pattern-compile and replacement-template parse
    /// failure (`issue #479`'s own "`re.error`'s class" note; 3.13
    /// renames it to `PatternError`, tracked only as a documented
    /// divergence, never matched here).
    ReError,
    /// `adapters/_retry.py::AdapterCallError` (lane F1, issue #478) --
    /// an adapter call failure carrying its own `RetryInfo`, raised by
    /// the scripted adapter's error replies. `type(e).__name__` for
    /// this class is used verbatim in #442's own multi-failure tree
    /// text.
    AdapterCallError,
    /// `adapters/scripted.py::ScriptedRepliesError` (lane F1, issue
    /// #478) -- the scripted adapter's own replies-file-is-malformed
    /// exception, a `RuntimeError` subclass Python code never confuses
    /// with a plain one (`type(e).__name__` differs).
    ScriptedRepliesError,
    /// `http.client.RemoteDisconnected` (lane E, issue #485) -- a
    /// `ConnectionResetError` subclass `urllib.request.urlopen` never
    /// wraps as a `URLError`: the server closed the connection before
    /// sending a status line at all.
    RemoteDisconnected,
    /// `http.client.IncompleteRead` (lane E, issue #485) -- a short
    /// body against a `Content-Length` the server itself declared;
    /// not retried (`cause: ErrorCause::None`), unlike every other
    /// raised `http` failure.
    IncompleteRead,
    /// `http.client.InvalidURL` (lane E, issue #485) -- a request
    /// target carrying a space or control character.
    InvalidURL,
}

impl PyExc {
    /// The bare class name, e.g. `"ValueError"` -- #442's own `{type}`.
    pub fn name(self) -> &'static str {
        match self {
            PyExc::ValueError => "ValueError",
            PyExc::TemplateError => "TemplateError",
            PyExc::AnswerParseError => "AnswerParseError",
            PyExc::SchemaValidationError => "SchemaValidationError",
            PyExc::AllowlistError => "AllowlistError",
            PyExc::RuntimeError => "RuntimeError",
            PyExc::TypeError => "TypeError",
            PyExc::KeyError => "KeyError",
            PyExc::LookupError => "LookupError",
            PyExc::PermissionError => "PermissionError",
            PyExc::FileNotFoundError => "FileNotFoundError",
            PyExc::IsADirectoryError => "IsADirectoryError",
            PyExc::NotADirectoryError => "NotADirectoryError",
            PyExc::OSError => "OSError",
            PyExc::ConnectionError => "ConnectionError",
            PyExc::TimeoutError => "TimeoutError",
            PyExc::UnicodeDecodeError => "UnicodeDecodeError",
            PyExc::OverflowError => "OverflowError",
            PyExc::RecursionError => "RecursionError",
            PyExc::UnicodeEncodeError => "UnicodeEncodeError",
            PyExc::IndexError => "IndexError",
            PyExc::ReError => "error",
            PyExc::AdapterCallError => "AdapterCallError",
            PyExc::ScriptedRepliesError => "ScriptedRepliesError",
            PyExc::RemoteDisconnected => "RemoteDisconnected",
            PyExc::IncompleteRead => "IncompleteRead",
            PyExc::InvalidURL => "InvalidURL",
        }
    }
}

impl fmt::Display for PyExc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// A failed [`py_int`] coercion -- the Python exception class it would
/// have raised (review finding 5 on PR #454: a caller that only kept
/// the message text couldn't tell a `ValueError` from the
/// `OverflowError` a non-finite float raises) plus CPython's own
/// message text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyIntError {
    pub py_class: PyExc,
    pub message: String,
}

impl fmt::Display for PyIntError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

fn value_error(message: impl Into<String>) -> PyIntError {
    PyIntError {
        py_class: PyExc::ValueError,
        message: message.into(),
    }
}

/// `%.200R`'s own truncation (`Objects/longobject.c`'s own invalid-
/// literal message, review P2-9 on PR #454's second review) --
/// *value*'s own [`Value::py_repr`], truncated to its first 200
/// *characters* (not bytes: a multi-byte codepoint is never split),
/// with no ellipsis added -- CPython's own `%R` precision truncation
/// (`PyUnicode_Substring`) is a plain substring, never a "..." marker.
fn py_repr_truncated_200(value: &Value) -> String {
    let repr = value.py_repr();
    match repr.char_indices().nth(200) {
        Some((byte_index, _)) => repr[..byte_index].to_string(),
        None => repr,
    }
}

/// Python's `int(value)` coercion, with CPython's own error wording on
/// failure: `bool`/`int`/`float` convert the way the `int()` builtin
/// does (`int(True) == 1`, truncating a float toward zero); a
/// numeric-looking `str` parses the same way (surrounding whitespace
/// tolerated via [`py_strip`]); anything else is a `TypeError`-shaped
/// message naming the value's own Python type. A non-finite float
/// raises `OverflowError`/`ValueError` exactly as `int(float("inf"))`/
/// `int(float("nan"))` do, rather than truncating silently.
///
/// Known divergence (shared by every caller): Python's `int()` is
/// arbitrary-precision and tolerates `_` digit grouping (`"1_0"` ==
/// `10`); this returns `i64`, so a numeral outside that range, or one
/// written with underscores, fails to parse here where CPython would
/// succeed. The same gap applies to the `Value::Float` arm below
/// (review P2-9 on PR #454's second review): `f.trunc() as i64` is
/// Rust's own saturating float-to-int cast, silently clamping to
/// `i64::MAX`/`MIN` for a float whose truncated value doesn't fit --
/// CPython raises `OverflowError` instead (`int(1e30)`), the same
/// class [`py_int`] already raises for a non-finite float, just not
/// yet for a finite one too large to represent.
pub fn py_int(value: &Value) -> Result<i64, PyIntError> {
    match value {
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::Int(i) => i
            .to_string()
            .parse::<i64>()
            .map_err(|_| value_error(format!("int too large to convert: {i}"))),
        Value::Float(f) if f.is_nan() => Err(PyIntError {
            py_class: PyExc::ValueError,
            message: "cannot convert float NaN to integer".to_string(),
        }),
        Value::Float(f) if f.is_infinite() => Err(PyIntError {
            py_class: PyExc::OverflowError,
            message: "cannot convert float infinity to integer".to_string(),
        }),
        Value::Float(f) => Ok(f.trunc() as i64),
        Value::Str(s) => py_strip(s).parse::<i64>().map_err(|_| {
            value_error(format!(
                "invalid literal for int() with base 10: {}",
                py_repr_truncated_200(&Value::Str(s.clone()))
            ))
        }),
        other => Err(PyIntError {
            py_class: PyExc::TypeError,
            message: format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                other.type_name()
            ),
        }),
    }
}

/// `True` for every codepoint Python's `str.isspace()` (and so a
/// bare `str.strip()`) treats as whitespace: every Unicode codepoint
/// Rust's own `char::is_whitespace` already agrees is whitespace,
/// **plus** the four C0 control characters `\x1c`-`\x1f` (FS/GS/RS/US)
/// CPython also counts but Rust does not (`electricity-template`'s own
/// crate docs name this same gap for chevron's standalone-tag check).
pub fn is_py_whitespace(ch: char) -> bool {
    ch.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&ch)
}

/// `str.strip()` with no arguments -- [`is_py_whitespace`]'s own set,
/// trimmed from both ends.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_whitespace)
}

/// `True` for every codepoint Python's `str.splitlines()` treats as a
/// line boundary -- a strict superset of `\n` alone: `\r` (and `\r\n`
/// as one boundary, never two empty lines), the four C0/C1 "other
/// line/paragraph separator" controls `\x0b`/`\x0c`/`\x1c`-`\x1e`, and
/// the two Unicode line/paragraph separators `\x85`/`\u2028`/`\u2029`
/// (CPython's own `Objects/unicodeobject.c::STRINGLIB(splitlines)`/
/// `unicodectype.c`'s own line-break table) -- used by [`py_splitlines`]
/// and shared with lane L2's own `strip_fences` (issue #506's "Hazards"
/// section: model output can carry any of these, which Rust's own
/// `str::lines()` doesn't split on).
fn is_py_line_boundary(ch: char) -> bool {
    matches!(
        ch,
        '\n' | '\r'
            | '\u{0b}'
            | '\u{0c}'
            | '\u{1c}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// `str.splitlines()` with no arguments -- every boundary
/// [`is_py_line_boundary`] recognizes, `\r\n` consumed as one boundary
/// (never an extra empty line between them), the terminator itself
/// dropped from every returned line (`keepends` defaults to `False`),
/// and no trailing empty line for text that ends in a boundary (`"a\n"
/// .splitlines() == ["a"]`, unlike `"a\n".split("\n") == ["a", ""]`).
pub fn py_splitlines(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < n {
        if is_py_line_boundary(chars[i]) {
            lines.push(chars[start..i].iter().collect());
            i += if chars[i] == '\r' && i + 1 < n && chars[i + 1] == '\n' {
                2
            } else {
                1
            };
            start = i;
        } else {
            i += 1;
        }
    }
    if start < n {
        lines.push(chars[start..n].iter().collect());
    }
    lines
}

/// `round(x, 6)`: correctly rounded on the *exact* binary value of
/// *x*, with half-even tie-breaking, exactly like CPython's `round()`
/// builtin for a float (`float.__round__`, `Objects/floatobject.c::
/// double_round` -- formats to a fixed 6 digits after the decimal
/// point via a correctly-rounded decimal conversion, then parses that
/// decimal string back to the nearest `f64`).
///
/// Rust's own fixed-precision float formatting (`format!("{:.6}", x)`)
/// is already a correctly-rounded decimal conversion of *x*'s exact
/// binary value with the same half-to-even tie-break Rust's `core`
/// float-to-decimal algorithm (`core::num::flt2dec`) uses throughout --
/// so formatting to 6 fractional digits and parsing the result back is
/// the same two-step CPython itself performs, not an approximation of
/// it. `NaN`/`+-inf` pass straight through unformatted (CPython's own
/// `round()` raises `OverflowError`/returns `nan` for these in ways a
/// 6-digit decimal round trip can't represent either way; this crate's
/// callers already special-case a non-finite value before ever
/// reaching this function in every known use).
pub fn round6(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.6}").parse::<f64>().unwrap_or(x)
}

/// `shlex.split(s)` states, as far as this crate's own `posix=True,
/// whitespace_split=True, comments=False, punctuation_chars=False`
/// configuration (`shlex.split`'s own defaults) ever reaches -- a bare
/// backslash escape outside quotes, or one inside a double-quoted
/// string (only `\\`/`\"` are consumed there; any other `\x` keeps
/// its own backslash), never inside a single-quoted string (single
/// quotes have no escape at all in POSIX shells, matched here too).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ShlexState {
    Space,
    Word,
    Quote(char),
    /// `escapedstate` in CPython's own `shlex.py` -- where this escape
    /// resumes once it resolves: back to a bare word, or back inside
    /// the double-quoted string that opened it.
    Escape(EscapeCtx),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EscapeCtx {
    Bare,
    DoubleQuote,
}

fn is_shlex_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

/// `shlex.split(s)` (POSIX mode, `comments=False`, `whitespace_split=
/// True`) -- a direct port of `shlex.py`'s own `read_token` state
/// machine, narrowed to the one configuration `shlex.split` itself ever
/// builds (`wordchars` never matters once `whitespace_split` is set: it
/// only widens, never narrows, which characters a bare word accepts).
/// `Err` carries CPython's own `ValueError` text (`"No closing
/// quotation"`/`"No escaped character"`) verbatim.
pub fn shlex_split(s: &str) -> Result<Vec<String>, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut pos = 0usize;
    let mut tokens = Vec::new();

    'outer: loop {
        let mut token = String::new();
        let mut state = ShlexState::Space;

        loop {
            let c = chars.get(pos).copied();
            if c.is_some() {
                pos += 1;
            }
            match (state, c) {
                (ShlexState::Space, None) => break 'outer,
                (ShlexState::Space, Some(ch)) if is_shlex_whitespace(ch) => continue,
                (ShlexState::Space, Some('\\')) => state = ShlexState::Escape(EscapeCtx::Bare),
                (ShlexState::Space, Some(ch)) if ch == '\'' || ch == '"' => {
                    state = ShlexState::Quote(ch);
                }
                (ShlexState::Space, Some(ch)) => {
                    token.push(ch);
                    state = ShlexState::Word;
                }

                (ShlexState::Word, None) => break,
                (ShlexState::Word, Some(ch)) if is_shlex_whitespace(ch) => break,
                (ShlexState::Word, Some(ch)) if ch == '\'' || ch == '"' => {
                    state = ShlexState::Quote(ch);
                }
                (ShlexState::Word, Some('\\')) => state = ShlexState::Escape(EscapeCtx::Bare),
                (ShlexState::Word, Some(ch)) => token.push(ch),

                (ShlexState::Quote(_), None) => return Err("No closing quotation".to_string()),
                (ShlexState::Quote(qc), Some(ch)) if ch == qc => state = ShlexState::Word,
                (ShlexState::Quote('"'), Some('\\')) => {
                    state = ShlexState::Escape(EscapeCtx::DoubleQuote);
                }
                (ShlexState::Quote(_), Some(ch)) => token.push(ch),

                (ShlexState::Escape(_), None) => return Err("No escaped character".to_string()),
                (ShlexState::Escape(EscapeCtx::Bare), Some(ch)) => {
                    token.push(ch);
                    state = ShlexState::Word;
                }
                (ShlexState::Escape(EscapeCtx::DoubleQuote), Some(ch)) => {
                    if ch != '\\' && ch != '"' {
                        token.push('\\');
                    }
                    token.push(ch);
                    state = ShlexState::Quote('"');
                }
            }
        }
        tokens.push(token);
    }
    Ok(tokens)
}

/// `shlex.quote(s)` -- `_find_unsafe = re.compile(r'[^\w@%+=:,./-]',
/// re.ASCII).search`'s own safe set (`\w` under `re.ASCII` is exactly
/// `[a-zA-Z0-9_]`) plus the listed punctuation; anything outside it
/// (including every non-ASCII character, which `\w` under `re.ASCII`
/// never matches) is single-quoted, with an embedded `'` closed,
/// literal-quoted, and reopened (`'"'"'`). Always `Ok` -- `shlex.quote`
/// itself never raises.
pub fn shlex_quote(s: &str) -> Result<String, String> {
    if s.is_empty() {
        return Ok("''".to_string());
    }
    let is_safe = |c: char| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c);
    if s.chars().all(is_safe) {
        return Ok(s.to_string());
    }
    Ok(format!("'{}'", s.replace('\'', "'\"'\"'")))
}

/// `urllib.parse.quote_plus(s, safe='')`'s own byte-level rule: the
/// RFC 3986 unreserved set (letters, digits, `_.-~`) plus *safe* pass
/// through unescaped; a space becomes `+`; everything else is
/// percent-encoded one byte at a time (a multi-byte UTF-8 character
/// becomes one `%XX` per byte -- CPython's own `quote`/`quote_plus`
/// UTF-8-encode a `str` input before this same per-byte rule). Never
/// fails -- `quote_plus` itself never raises.
fn quote_plus_bytes(bytes: &[u8], safe: &str) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b < 0x80 {
            let c = b as char;
            if c.is_ascii_alphanumeric() || "_.-~".contains(c) || safe.contains(c) {
                out.push(c);
                continue;
            }
            if b == b' ' {
                out.push('+');
                continue;
            }
        }
        out.push_str(&format!("%{b:02X}"));
    }
    out
}

fn quote_plus_str(s: &str, safe: &str) -> String {
    quote_plus_bytes(s.as_bytes(), safe)
}

/// A `doseq=True` element's own wire text: `quote_via(elt, safe)` for
/// `bytes` (quoted directly, never stringified -- `str(b"x")` would
/// give the `b'x'` repr form instead), `quote_via(str(elt), safe, ...)`
/// for everything else, matching [`Value::py_str`]'s own `str()`.
fn quote_plus_scalar(value: &Value) -> String {
    match value {
        Value::Bytes(b) => quote_plus_bytes(b, ""),
        other => quote_plus_str(&other.py_str(), ""),
    }
}

/// `urllib.parse.urlencode(query, doseq=...)`, `safe=''` (the only form
/// `plugins/http.py` ever calls) -- *params* is already the `(key,
/// value)` pairs in a dict's own insertion order, each key already
/// run through Python's own `str()` by the caller: `Value`'s `Dict`
/// keys are themselves `Value`s, not `String`s, so stringifying a key
/// happens once at the call site rather than being re-derived here.
///
/// Without `doseq`, every value is `str()`-ed and quoted (CPython's
/// own non-`doseq` branch). With `doseq`:
/// - a `str` value is quoted as-is;
/// - a `bytes` value is quoted directly, not stringified;
/// - anything `len()`-able in CPython -- here, [`Value::List`] and
///   [`Value::Dict`] -- emits one `key=value` pair per *element* (a
///   list's own items; a dict's own **keys**, exactly as iterating a
///   dict does in Python), an empty collection emitting nothing for
///   that key;
/// - every other value (CPython's own `TypeError` from a failed
///   `len()`: `bool`/`int`/`float`/`None`/a `Value::Date`/`DateTime`)
///   falls back to the non-`doseq` scalar form.
///
/// Always `Ok` -- nothing in this path can fail once *params* is
/// already a list of `(String, Value)` pairs; kept `Result` so a
/// future caller that still holds an unkeyed params value has one
/// seam to extend into without a signature change.
pub fn urlencode(params: &[(String, Value)], doseq: bool) -> Result<String, String> {
    let mut pairs: Vec<String> = Vec::new();
    for (key, value) in params {
        let quoted_key = quote_plus_str(key, "");
        if !doseq {
            pairs.push(format!("{quoted_key}={}", quote_plus_scalar(value)));
            continue;
        }
        match value {
            Value::Str(s) => pairs.push(format!("{quoted_key}={}", quote_plus_str(s, ""))),
            Value::Bytes(b) => pairs.push(format!("{quoted_key}={}", quote_plus_bytes(b, ""))),
            Value::List(items) => {
                for item in items {
                    pairs.push(format!("{quoted_key}={}", quote_plus_scalar(item)));
                }
            }
            Value::Dict(entries) => {
                for (inner_key, _) in entries {
                    pairs.push(format!("{quoted_key}={}", quote_plus_scalar(inner_key)));
                }
            }
            other => pairs.push(format!("{quoted_key}={}", quote_plus_scalar(other))),
        }
    }
    Ok(pairs.join("&"))
}

/// Why [`py_str_format`] fell back to the raw template -- every
/// variant is a documented divergence from CPython's own `str.format`
/// (issue #506's lane L1, the "Settled questions" section): a
/// positional field, an unknown name, attribute/index access, an
/// unknown conversion or a malformed format spec all raise a real
/// Python exception there, but this port never reproduces the exact
/// exception text -- callers (the reflector prime) only ever log a
/// fixed warning and fall back to the unformatted template, never this
/// error's own text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyFormatError {
    /// A lone `{` or `}` with no matching partner.
    LoneBrace,
    /// `{}`/`{0}` -- auto-numbering/explicit positional fields, never
    /// supported (the prime only ever has named fields).
    PositionalField,
    /// A field name not in *fields*.
    UnknownField,
    /// `{goal.upper}`/`{goal[0]}` -- attribute/index access.
    AttributeOrIndex,
    /// `{goal!x}` -- a conversion other than `s`/`r`/`a`.
    UnknownConversion,
    /// A format spec this subset doesn't parse, or applies to a type
    /// it doesn't support (a sign/`#`/`0`/grouping flag on a `str`
    /// value, a `.precision` on an `int`, a `type` char that isn't
    /// valid for the value's own type).
    BadFormatSpec,
}

enum ConvertedValue {
    Raw(Value),
    Str(String),
}

/// Python `ascii(value)`: [`crate::string_repr::py_str_repr`] (which
/// keeps printable non-ASCII literal, matching `repr()`) with every
/// remaining non-ASCII character backslash-escaped the same way
/// CPython's own `ascii()` does -- `\xHH`/`\uHHHH`/`\UHHHHHHHH` by
/// code-point size.
fn py_ascii_repr(s: &str) -> String {
    let repr = crate::string_repr::py_str_repr(s);
    let mut out = String::with_capacity(repr.len());
    for ch in repr.chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            let cp = ch as u32;
            if cp <= 0xFF {
                out.push_str(&format!("\\x{cp:02X}"));
            } else if cp <= 0xFFFF {
                out.push_str(&format!("\\u{cp:04X}"));
            } else {
                out.push_str(&format!("\\U{cp:08X}"));
            }
        }
    }
    out
}

/// `value.__repr__()`/`value.__str__()` for exactly the types
/// [`py_str_format`]'s own callers ever hand it (`str`/`int` --
/// [`Value::py_repr`]/[`Value::py_str`] already match CPython for
/// both).
fn converted_text(value: &Value, conv: char) -> String {
    match conv {
        's' => value.py_str(),
        'r' => value.py_repr(),
        'a' => py_ascii_repr(&value.py_repr()),
        _ => unreachable!("converted_text is only ever called with a validated conversion"),
    }
}

/// One `{field_spec}` replacement field's own parsed shape --
/// `field_name[!conversion][:format_spec]`, Python's own "Format
/// String Syntax" grammar narrowed to a bare name (no `.attr`/
/// `[index]`, [`PyFormatError::AttributeOrIndex`] instead).
struct ParsedField<'a> {
    name: &'a str,
    conversion: Option<char>,
    format_spec: &'a str,
}

fn parse_field(field_spec: &str) -> Result<ParsedField<'_>, PyFormatError> {
    let (before_spec, format_spec) = match field_spec.find(':') {
        Some(idx) => (&field_spec[..idx], &field_spec[idx + 1..]),
        None => (field_spec, ""),
    };
    let (name, conversion) = match before_spec.find('!') {
        Some(idx) => {
            let rest = &before_spec[idx + 1..];
            let mut chars = rest.chars();
            let conv = chars.next().ok_or(PyFormatError::UnknownConversion)?;
            if chars.next().is_some() || !matches!(conv, 's' | 'r' | 'a') {
                return Err(PyFormatError::UnknownConversion);
            }
            (&before_spec[..idx], Some(conv))
        }
        None => (before_spec, None),
    };
    if name.is_empty() || name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Err(PyFormatError::PositionalField);
    }
    if name.contains('.') || name.contains('[') {
        return Err(PyFormatError::AttributeOrIndex);
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(PyFormatError::UnknownField);
    }
    Ok(ParsedField {
        name,
        conversion,
        format_spec,
    })
}

/// Python's own "Format Specification Mini-Language":
/// `[[fill]align][sign][#][0][width][,_][.precision][type]`, parsed
/// once and applied by [`apply_str_spec`]/[`apply_int_spec`] against
/// whichever of the two types actually reached them.
#[derive(Default)]
struct FormatSpec {
    fill: char,
    align: Option<char>,
    sign: Option<char>,
    alternate: bool,
    zero: bool,
    width: Option<usize>,
    grouping: Option<char>,
    precision: Option<usize>,
    kind: Option<char>,
}

fn parse_format_spec(spec: &str) -> Result<FormatSpec, PyFormatError> {
    let chars: Vec<char> = spec.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    let mut out = FormatSpec {
        fill: ' ',
        ..Default::default()
    };

    if n >= 2 && matches!(chars[1], '<' | '>' | '^' | '=') {
        out.fill = chars[0];
        out.align = Some(chars[1]);
        i = 2;
    } else if n >= 1 && matches!(chars[0], '<' | '>' | '^' | '=') {
        out.align = Some(chars[0]);
        i = 1;
    }
    if i < n && matches!(chars[i], '+' | '-' | ' ') {
        out.sign = Some(chars[i]);
        i += 1;
    }
    if i < n && chars[i] == '#' {
        out.alternate = true;
        i += 1;
    }
    if i < n && chars[i] == '0' {
        out.zero = true;
        if out.align.is_none() {
            out.align = Some('=');
            out.fill = '0';
        }
        i += 1;
    }
    let width_start = i;
    while i < n && chars[i].is_ascii_digit() {
        i += 1;
    }
    if i > width_start {
        let width_text: String = chars[width_start..i].iter().collect();
        out.width = Some(
            width_text
                .parse()
                .map_err(|_| PyFormatError::BadFormatSpec)?,
        );
    }
    if i < n && matches!(chars[i], ',' | '_') {
        out.grouping = Some(chars[i]);
        i += 1;
    }
    if i < n && chars[i] == '.' {
        i += 1;
        let prec_start = i;
        while i < n && chars[i].is_ascii_digit() {
            i += 1;
        }
        if i == prec_start {
            return Err(PyFormatError::BadFormatSpec);
        }
        let prec_text: String = chars[prec_start..i].iter().collect();
        out.precision = Some(
            prec_text
                .parse()
                .map_err(|_| PyFormatError::BadFormatSpec)?,
        );
    }
    if i < n {
        out.kind = Some(chars[i]);
        i += 1;
    }
    if i != n {
        return Err(PyFormatError::BadFormatSpec);
    }
    Ok(out)
}

fn pad(text: &str, spec: &FormatSpec, default_align: char) -> String {
    let width = spec.width.unwrap_or(0);
    let len = text.chars().count();
    if len >= width {
        return text.to_string();
    }
    let pad_len = width - len;
    let align = spec.align.unwrap_or(default_align);
    match align {
        '<' => format!("{text}{}", spec.fill.to_string().repeat(pad_len)),
        '>' => format!("{}{text}", spec.fill.to_string().repeat(pad_len)),
        '^' => {
            let left = pad_len / 2;
            let right = pad_len - left;
            format!(
                "{}{text}{}",
                spec.fill.to_string().repeat(left),
                spec.fill.to_string().repeat(right)
            )
        }
        '=' => format!("{}{text}", spec.fill.to_string().repeat(pad_len)),
        _ => text.to_string(),
    }
}

/// `format(s, spec)` for a `str` -- CPython raises `ValueError` for a
/// `sign`/`#`/`0`/grouping flag or any `type` but `s` on a string;
/// this subset falls back on all of those rather than reproducing the
/// exact message.
fn apply_str_spec(text: &str, spec: &FormatSpec) -> Result<String, PyFormatError> {
    if spec.sign.is_some() || spec.alternate || spec.zero || spec.grouping.is_some() {
        return Err(PyFormatError::BadFormatSpec);
    }
    if let Some(kind) = spec.kind {
        if kind != 's' {
            return Err(PyFormatError::BadFormatSpec);
        }
    }
    let truncated: String = match spec.precision {
        Some(p) => text.chars().take(p).collect(),
        None => text.to_string(),
    };
    Ok(pad(&truncated, spec, '<'))
}

/// `format(i, spec)` for an `int` -- `b`/`o`/`d`/`x`/`X`/`n`/no type
/// (decimal); `#` prepends the base prefix; `0` zero-pads between the
/// sign and the digits (the `=` alignment [`pad`] already implements,
/// since `0`-fill defaults `align` to `=`); CPython raises
/// `ValueError` for a `.precision` on an int, matched here by falling
/// back instead.
fn apply_int_spec(value: i64, spec: &FormatSpec) -> Result<String, PyFormatError> {
    if spec.precision.is_some() {
        return Err(PyFormatError::BadFormatSpec);
    }
    let negative = value < 0;
    let magnitude = value.unsigned_abs();
    let kind = spec.kind.unwrap_or('d');
    let (digits, prefix) = match kind {
        'd' | 'n' => (magnitude.to_string(), ""),
        'b' => (format!("{magnitude:b}"), "0b"),
        'o' => (format!("{magnitude:o}"), "0o"),
        'x' => (format!("{magnitude:x}"), "0x"),
        'X' => (format!("{magnitude:X}"), "0X"),
        'c' => return Err(PyFormatError::BadFormatSpec),
        _ => return Err(PyFormatError::BadFormatSpec),
    };
    let sign = if negative {
        "-"
    } else {
        match spec.sign {
            Some('+') => "+",
            Some(' ') => " ",
            _ => "",
        }
    };
    let base_prefix = if spec.alternate { prefix } else { "" };
    let body = digits;
    if spec.align == Some('=') {
        let width = spec.width.unwrap_or(0);
        let core_len = sign.chars().count() + base_prefix.chars().count() + body.chars().count();
        let fill_count = width.saturating_sub(core_len);
        return Ok(format!(
            "{sign}{base_prefix}{}{body}",
            spec.fill.to_string().repeat(fill_count)
        ));
    }
    let combined = format!("{sign}{base_prefix}{body}");
    Ok(pad(&combined, spec, '>'))
}

/// `str.format(**fields)`, narrowed to exactly the subset the
/// reflector prime needs (issue #506's lane L1, "Hazards"): `{{`/`}}`
/// literal braces, named fields only (no positional/auto-numbering),
/// `!s`/`!r`/`!a` conversions, and the Format Specification Mini-
/// Language for `str`/`int` values. *fields* is searched linearly (a
/// handful of entries -- `goal`/`context`/`max_effects` -- never worth
/// a map). Any field this subset can't handle -- a positional field,
/// an unknown name, `.attr`/`[index]` access, an unknown conversion, a
/// malformed or type-mismatched format spec, or a lone unescaped
/// `{`/`}` -- is `Err(_)`; the caller's own fallback (raw template
/// text) is what actually reproduces CPython's behaviour of raising
/// and being caught, not this error's own value (see
/// [`PyFormatError`]'s own doc comment).
pub fn py_str_format(template: &str, fields: &[(&str, Value)]) -> Result<String, PyFormatError> {
    let chars: Vec<char> = template.chars().collect();
    let n = chars.len();
    let mut out = String::new();
    let mut i = 0usize;
    while i < n {
        match chars[i] {
            '{' if i + 1 < n && chars[i + 1] == '{' => {
                out.push('{');
                i += 2;
            }
            '}' if i + 1 < n && chars[i + 1] == '}' => {
                out.push('}');
                i += 2;
            }
            '{' => {
                let start = i + 1;
                let mut j = start;
                while j < n && chars[j] != '}' {
                    if chars[j] == '{' {
                        return Err(PyFormatError::LoneBrace);
                    }
                    j += 1;
                }
                if j >= n {
                    return Err(PyFormatError::LoneBrace);
                }
                let field_spec: String = chars[start..j].iter().collect();
                let parsed = parse_field(&field_spec)?;
                let value = fields
                    .iter()
                    .find(|(name, _)| *name == parsed.name)
                    .map(|(_, value)| value)
                    .ok_or(PyFormatError::UnknownField)?;
                let converted = match parsed.conversion {
                    Some(conv) => ConvertedValue::Str(converted_text(value, conv)),
                    None => ConvertedValue::Raw(value.clone()),
                };
                let spec = parse_format_spec(parsed.format_spec)?;
                let rendered = match &converted {
                    ConvertedValue::Str(s) => apply_str_spec(s, &spec)?,
                    ConvertedValue::Raw(Value::Str(s)) => apply_str_spec(s, &spec)?,
                    ConvertedValue::Raw(Value::Int(int_value)) => {
                        let as_i64 = py_int(&Value::Int(int_value.clone()))
                            .map_err(|_| PyFormatError::BadFormatSpec)?;
                        apply_int_spec(as_i64, &spec)?
                    }
                    ConvertedValue::Raw(_) => return Err(PyFormatError::BadFormatSpec),
                };
                out.push_str(&rendered);
                i = j + 1;
            }
            '}' => return Err(PyFormatError::LoneBrace),
            ch => {
                out.push(ch);
                i += 1;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod py_str_format_tests {
    use super::*;

    fn fields(pairs: &[(&'static str, Value)]) -> Vec<(&'static str, Value)> {
        pairs.to_vec()
    }

    #[test]
    fn substitutes_named_fields() {
        let f = fields(&[
            ("goal", Value::Str("g".to_string())),
            ("context", Value::Str("c".to_string())),
        ]);
        assert_eq!(
            py_str_format("{goal} and {context}", &f).unwrap(),
            "g and c"
        );
    }

    #[test]
    fn literal_double_braces_survive() {
        let f = fields(&[("goal", Value::Str("g".to_string()))]);
        assert_eq!(
            py_str_format("{{literal}} {goal}", &f).unwrap(),
            "{literal} g"
        );
    }

    #[test]
    fn zero_padded_int_width() {
        let f = fields(&[("max_effects", Value::from(8i64))]);
        assert_eq!(py_str_format("{max_effects:03d}", &f).unwrap(), "008");
    }

    #[test]
    fn plain_int_width_and_alignment() {
        let f = fields(&[("max_effects", Value::from(8i64))]);
        assert_eq!(py_str_format("{max_effects:5d}", &f).unwrap(), "    8");
        assert_eq!(py_str_format("{max_effects:<5d}|", &f).unwrap(), "8    |");
        assert_eq!(py_str_format("{max_effects:^5d}|", &f).unwrap(), "  8  |");
    }

    #[test]
    fn str_alignment_and_precision() {
        let f = fields(&[("goal", Value::Str("hi".to_string()))]);
        assert_eq!(py_str_format("{goal:>10s}|", &f).unwrap(), "        hi|");
        let f2 = fields(&[("goal", Value::Str("hello".to_string()))]);
        assert_eq!(py_str_format("{goal:.2s}|", &f2).unwrap(), "he|");
    }

    #[test]
    fn repr_conversion_quotes_like_cpython() {
        let f = fields(&[("context", Value::Str("c'text".to_string()))]);
        assert_eq!(py_str_format("{context!r}", &f).unwrap(), "\"c'text\"");
    }

    #[test]
    fn str_conversion_is_a_no_op_for_a_string() {
        let f = fields(&[("goal", Value::Str("g".to_string()))]);
        assert_eq!(py_str_format("{goal!s}", &f).unwrap(), "g");
    }

    #[test]
    fn unknown_field_falls_back() {
        let f = fields(&[("goal", Value::Str("g".to_string()))]);
        assert_eq!(
            py_str_format("{unknown}", &f),
            Err(PyFormatError::UnknownField)
        );
    }

    #[test]
    fn positional_field_falls_back() {
        assert_eq!(
            py_str_format("{0}", &[]),
            Err(PyFormatError::PositionalField)
        );
        assert_eq!(
            py_str_format("{}", &[]),
            Err(PyFormatError::PositionalField)
        );
    }

    #[test]
    fn attribute_and_index_access_fall_back() {
        let f = fields(&[("goal", Value::Str("g".to_string()))]);
        assert_eq!(
            py_str_format("{goal.upper}", &f),
            Err(PyFormatError::AttributeOrIndex)
        );
        assert_eq!(
            py_str_format("{goal[0]}", &f),
            Err(PyFormatError::AttributeOrIndex)
        );
    }

    #[test]
    fn lone_braces_fall_back() {
        assert_eq!(py_str_format("{", &[]), Err(PyFormatError::LoneBrace));
        assert_eq!(py_str_format("}", &[]), Err(PyFormatError::LoneBrace));
    }

    #[test]
    fn unknown_conversion_falls_back() {
        let f = fields(&[("goal", Value::Str("g".to_string()))]);
        assert_eq!(
            py_str_format("{goal!x}", &f),
            Err(PyFormatError::UnknownConversion)
        );
    }

    #[test]
    fn mustache_text_survives_the_fallback_raw_template() {
        // The prime's own fallback path (`_render_reflector_prime`)
        // hands the *raw* template to the caller on `Err`, not this
        // function's own partial output -- `{{input.x}}` inside a
        // custom `prime_template` therefore reaches Mustache rendering
        // later exactly as written, never half-substituted here. This
        // test only pins that `py_str_format` itself never panics or
        // partially mutates anything observable on an error path (it
        // returns `Err` before `out` is ever handed back).
        let f = fields(&[("goal", Value::Str("g".to_string()))]);
        assert!(py_str_format("{{input.x}} {unknown}", &f).is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn py_int_coerces_bool_int_float_and_numeric_strings() {
        assert_eq!(py_int(&Value::Bool(true)), Ok(1));
        assert_eq!(py_int(&Value::Bool(false)), Ok(0));
        assert_eq!(py_int(&Value::Float(4.9)), Ok(4));
        assert_eq!(py_int(&Value::Float(-4.9)), Ok(-4));
        assert_eq!(py_int(&Value::Str(" 42 ".to_string())), Ok(42));
    }

    #[test]
    fn py_int_reports_cpythons_own_text_for_a_bad_string() {
        let err = py_int(&Value::Str("x".to_string())).unwrap_err();
        // CPython's own `repr('x')` is single-quoted, not Rust's
        // double-quoted `{:?}` (review finding 5 on PR #454).
        assert_eq!(err.message, "invalid literal for int() with base 10: 'x'");
        assert_eq!(err.py_class, PyExc::ValueError);
    }

    #[test]
    fn py_int_truncates_a_long_bad_string_to_200_chars_like_percent_200_r() {
        // Review P2-9 on PR #454's second review: CPython's own
        // invalid-literal message formats the repr with `%.200R`, a
        // plain substring truncation to 200 characters, no ellipsis.
        let long = "x".repeat(250);
        let err = py_int(&Value::Str(long)).unwrap_err();
        let expected_repr = format!("'{}", "x".repeat(199));
        assert_eq!(
            err.message,
            format!("invalid literal for int() with base 10: {expected_repr}")
        );
        // The repr itself (`'` + 250 x's + `'`) is 252 chars; truncated
        // to the first 200 leaves the opening quote plus 199 x's, no
        // closing quote -- CPython's own truncation cuts the repr's
        // own text wherever the 200th character falls, quote or not.
        assert_eq!(expected_repr.chars().count(), 200);
    }

    #[test]
    fn py_int_reports_cpythons_own_text_for_an_unsupported_type() {
        let err = py_int(&Value::List(Vec::new())).unwrap_err();
        assert_eq!(
            err.message,
            "int() argument must be a string, a bytes-like object or a real number, not 'list'"
        );
        assert_eq!(err.py_class, PyExc::TypeError);
    }

    #[test]
    fn py_int_raises_value_error_for_nan() {
        let err = py_int(&Value::Float(f64::NAN)).unwrap_err();
        assert_eq!(err.message, "cannot convert float NaN to integer");
        assert_eq!(err.py_class, PyExc::ValueError);
    }

    #[test]
    fn py_int_raises_overflow_error_for_infinity() {
        let err = py_int(&Value::Float(f64::INFINITY)).unwrap_err();
        assert_eq!(err.message, "cannot convert float infinity to integer");
        assert_eq!(err.py_class, PyExc::OverflowError);
        let err = py_int(&Value::Float(f64::NEG_INFINITY)).unwrap_err();
        assert_eq!(err.message, "cannot convert float infinity to integer");
        assert_eq!(err.py_class, PyExc::OverflowError);
    }

    #[test]
    fn py_strip_trims_c0_control_whitespace_python_counts_but_rust_does_not() {
        assert_eq!(py_strip("\u{1c}\u{1d}hi\u{1e}\u{1f}"), "hi");
        assert_eq!(py_strip("  hi  "), "hi");
        assert_eq!(py_strip(""), "");
    }

    #[test]
    fn round6_rounds_half_to_even_on_a_well_known_binary_value() {
        // 2.675 is not exactly representable; its nearest `f64` is
        // slightly *below* 2.675, so correctly-rounded `round(x, 2)`
        // (not this function's own 6-digit rounding, just the same
        // underlying mechanism) famously gives 2.67, not 2.68 -- the
        // textbook example of "round on the exact binary value, not
        // the decimal you typed." At 6 digits the same value is exact
        // enough that the stored binary value's own extra digits are
        // still visible.
        assert_eq!(round6(2.675), 2.675);
    }

    #[test]
    fn round6_is_idempotent_on_an_already_short_value() {
        assert_eq!(round6(0.5), 0.5);
        assert_eq!(round6(1.0), 1.0);
        assert_eq!(round6(-0.000001), -0.000001);
    }

    #[test]
    fn round6_rounds_odd_multiples_of_one_over_128_half_to_even() {
        // Review finding 7 on PR #454: a 6-digit tie only happens at an
        // odd multiple of 1/128 -- `generate_round6_corpus.py`'s own
        // random draws essentially never hit one, so the half-even
        // claim went untested without cases picked specifically to land
        // on one.
        assert_eq!(round6(0.0078125), 0.007812);
        assert_eq!(round6(-0.0078125), -0.007812);
        assert_eq!(round6(0.0234375), 0.023438);
        assert_eq!(round6(-0.0234375), -0.023438);
        assert_eq!(round6(1.0078125), 1.007812);
        assert_eq!(round6(123.0078125), 123.007812);
        assert_eq!(round6(9999.9921875), 9999.992188);
    }

    #[test]
    fn round6_passes_non_finite_values_through_unchanged() {
        assert!(round6(f64::NAN).is_nan());
        assert_eq!(round6(f64::INFINITY), f64::INFINITY);
        assert_eq!(round6(f64::NEG_INFINITY), f64::NEG_INFINITY);
    }

    #[test]
    fn shlex_split_splits_on_whitespace_and_strips_quotes() {
        assert_eq!(
            shlex_split("ffmpeg -i in.mp4 out.mp4"),
            Ok(vec!["ffmpeg", "-i", "in.mp4", "out.mp4"]
                .into_iter()
                .map(String::from)
                .collect())
        );
        assert_eq!(
            shlex_split("a 'b c' \"d e\""),
            Ok(vec!["a", "b c", "d e"]
                .into_iter()
                .map(String::from)
                .collect())
        );
    }

    #[test]
    fn shlex_split_handles_empty_and_whitespace_only_input() {
        assert_eq!(shlex_split(""), Ok(Vec::new()));
        assert_eq!(shlex_split("   \t "), Ok(Vec::new()));
    }

    #[test]
    fn shlex_split_preserves_an_empty_quoted_argument() {
        assert_eq!(
            shlex_split("a '' b"),
            Ok(vec!["a", "", "b"].into_iter().map(String::from).collect())
        );
    }

    #[test]
    fn shlex_split_concatenates_adjacent_quoted_and_bare_text() {
        assert_eq!(shlex_split("'abc'def"), Ok(vec!["abcdef".to_string()]));
        assert_eq!(shlex_split("'it'\"'\"'s'"), Ok(vec!["it's".to_string()]));
    }

    #[test]
    fn shlex_split_escapes_a_space_outside_quotes() {
        assert_eq!(shlex_split("a\\ b"), Ok(vec!["a b".to_string()]));
    }

    #[test]
    fn shlex_split_double_quote_escape_only_consumes_backslash_and_quote() {
        assert_eq!(shlex_split(r#""a\$b""#), Ok(vec!["a\\$b".to_string()]));
        assert_eq!(shlex_split(r#""a\"b""#), Ok(vec!["a\"b".to_string()]));
        assert_eq!(shlex_split(r#""a\\b""#), Ok(vec!["a\\b".to_string()]));
    }

    #[test]
    fn shlex_split_reports_cpythons_own_text_for_an_unterminated_quote() {
        assert_eq!(
            shlex_split("'unterminated"),
            Err("No closing quotation".to_string())
        );
    }

    #[test]
    fn shlex_split_reports_cpythons_own_text_for_a_trailing_backslash() {
        assert_eq!(
            shlex_split("trailing\\"),
            Err("No escaped character".to_string())
        );
    }

    #[test]
    fn shlex_quote_wraps_an_empty_string() {
        assert_eq!(shlex_quote(""), Ok("''".to_string()));
    }

    #[test]
    fn shlex_quote_leaves_a_plain_word_unchanged() {
        assert_eq!(
            shlex_quote("plain-word_1.txt"),
            Ok("plain-word_1.txt".to_string())
        );
    }

    #[test]
    fn shlex_quote_single_quotes_a_string_with_a_space() {
        assert_eq!(shlex_quote("a b"), Ok("'a b'".to_string()));
    }

    #[test]
    fn shlex_quote_escapes_an_embedded_single_quote() {
        assert_eq!(shlex_quote("it's"), Ok("'it'\"'\"'s'".to_string()));
    }

    #[test]
    fn shlex_quote_quotes_any_non_ascii_character() {
        assert_eq!(shlex_quote("café"), Ok("'café'".to_string()));
    }
}

#[cfg(test)]
mod urlencode_tests {
    use super::*;
    use crate::Dict;

    fn pairs(entries: Vec<(&str, Value)>) -> Vec<(String, Value)> {
        entries
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
    }

    #[test]
    fn doseq_scalars_match_pythons_str() {
        let params = pairs(vec![
            ("a", Value::from(1i64)),
            ("b", Value::Bool(true)),
            ("c", Value::None),
            ("d", Value::Float(1.5)),
        ]);
        assert_eq!(urlencode(&params, true).unwrap(), "a=1&b=True&c=None&d=1.5");
    }

    #[test]
    fn doseq_list_emits_one_pair_per_element() {
        let params = pairs(vec![(
            "a",
            Value::List(vec![
                Value::from(1i64),
                Value::from(2i64),
                Value::from(3i64),
            ]),
        )]);
        assert_eq!(urlencode(&params, true).unwrap(), "a=1&a=2&a=3");
    }

    #[test]
    fn doseq_empty_list_emits_nothing() {
        let params = pairs(vec![("a", Value::List(vec![]))]);
        assert_eq!(urlencode(&params, true).unwrap(), "");
    }

    #[test]
    fn doseq_dict_value_iterates_its_keys() {
        let mut dict = Dict::new();
        dict.insert(Value::Str("x".to_string()), Value::from(1i64));
        dict.insert(Value::Str("y".to_string()), Value::from(2i64));
        let params = pairs(vec![("a", Value::Dict(dict))]);
        assert_eq!(urlencode(&params, true).unwrap(), "a=x&a=y");
    }

    #[test]
    fn reserved_and_non_ascii_characters_are_percent_encoded() {
        let params = pairs(vec![(
            "q",
            Value::Str("hello world/slash~tilde".to_string()),
        )]);
        assert_eq!(
            urlencode(&params, true).unwrap(),
            "q=hello+world%2Fslash~tilde"
        );
        let params = pairs(vec![("q", Value::Str("héllo".to_string()))]);
        assert_eq!(urlencode(&params, true).unwrap(), "q=h%C3%A9llo");
    }

    #[test]
    fn non_doseq_matches_doseq_for_plain_scalars() {
        let params = pairs(vec![
            ("a", Value::from(1i64)),
            ("b", Value::Bool(true)),
            ("c", Value::None),
            ("d", Value::Float(1.5)),
        ]);
        assert_eq!(
            urlencode(&params, false).unwrap(),
            "a=1&b=True&c=None&d=1.5"
        );
    }

    #[test]
    fn bytes_values_are_quoted_directly_not_stringified() {
        let params = pairs(vec![("a", Value::Bytes(b"bytes val".to_vec()))]);
        assert_eq!(urlencode(&params, true).unwrap(), "a=bytes+val");
        assert_eq!(urlencode(&params, false).unwrap(), "a=bytes+val");
    }
}
