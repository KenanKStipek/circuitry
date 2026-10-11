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
}

impl PyExc {
    /// The bare class name, e.g. `"ValueError"` -- #442's own `{type}`.
    pub fn name(self) -> &'static str {
        match self {
            PyExc::ValueError => "ValueError",
            PyExc::TemplateError => "TemplateError",
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

/// `urllib.parse.urlencode(params, doseq=...)` -- signature only; lane
/// E fills the body (the `http` tool's own query-string construction).
pub fn urlencode(_params: &[(String, Value)], _doseq: bool) -> Result<String, String> {
    Err("electricity_value::pycompat::urlencode is not implemented yet (lane E)".to_string())
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
