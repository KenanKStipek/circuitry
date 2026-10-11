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
}

impl PyExc {
    /// The bare class name, e.g. `"ValueError"` -- #442's own `{type}`.
    pub fn name(self) -> &'static str {
        match self {
            PyExc::ValueError => "ValueError",
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
/// succeed.
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
                Value::Str(s.clone()).py_repr()
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

/// `shlex.split(s)` (POSIX mode) -- signature only; lane C fills the
/// body (the `ffmpeg`/`shell` plugins' own flag parsing).
pub fn shlex_split(_s: &str) -> Result<Vec<String>, String> {
    Err("electricity_value::pycompat::shlex_split is not implemented yet (lane C)".to_string())
}

/// `shlex.quote(s)` -- signature only; lane C fills the body.
pub fn shlex_quote(_s: &str) -> Result<String, String> {
    Err("electricity_value::pycompat::shlex_quote is not implemented yet (lane C)".to_string())
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
}
