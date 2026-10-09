//! Small helpers shared by this crate's modules -- dict lookup by a
//! plain `&str` key (every config/document mapping this crate reads is
//! keyed by `Value::Str` in practice, since both come from JSON/YAML,
//! but `Dict`'s key type is `Value`, not `String`) and Python's `:g`
//! float formatting, used by a handful of `complexity`'s own
//! validation messages (`cli/complexity_config.py`'s `f"{x:g}"`
//! interpolations).

use electricity_value::{Dict, Value};

/// `dict.get(key)` for a plain string key.
pub(crate) fn get<'a>(dict: &'a Dict, key: &str) -> Option<&'a Value> {
    dict.get(&Value::Str(key.to_string()))
}

/// `value.as_dict()`, or `None` for anything else (including `None`
/// itself) -- shorthand for the "is this an object" checks scattered
/// through the config/complexity/persistence validation ports.
pub(crate) fn as_dict(value: &Value) -> Option<&Dict> {
    value.as_dict()
}

/// Python's `type(value)`-keyed type name used throughout
/// `cli/complexity_config.py`'s own validation messages
/// (`_type_name`): `None` -> `"null"`, `bool` -> `"a boolean"`, an
/// `int`/`float` -> `"a number"`, `str` -> `"a string"`, a mapping ->
/// `"an object"`, a list -> `"an array"`, anything else (unreachable
/// from JSON) -> `"a <type>"`.
pub(crate) fn type_name(value: &Value) -> String {
    match value {
        Value::None => "null".to_string(),
        Value::Bool(_) => "a boolean".to_string(),
        Value::Int(_) | Value::Float(_) => "a number".to_string(),
        Value::Str(_) => "a string".to_string(),
        Value::Dict(_) => "an object".to_string(),
        Value::List(_) => "an array".to_string(),
        Value::Bytes(_) => "a bytes".to_string(),
        Value::Date(_) => "a date".to_string(),
        Value::DateTime(..) => "a datetime".to_string(),
    }
}

/// Python's `f"{x:g}"`: the shortest fixed-notation rendering with 6
/// significant digits, falling back to scientific notation
/// (`1e+10`-style, always-signed two-digit-minimum exponent) outside
/// it -- CPython's `float.__format__` with the `g` presentation type at
/// its default precision (6). Every call site in this crate formats a
/// config-supplied score/threshold value in a validation message
/// (`cli/complexity_config.py`), never a value this crate itself
/// computes, so exactness here is about matching Circuitry's own
/// wording for a value the *user* supplied, not an internal
/// computation of ours.
pub(crate) fn py_format_g(x: f64) -> String {
    if x == 0.0 {
        return if x.is_sign_negative() {
            "-0".to_string()
        } else {
            "0".to_string()
        };
    }
    let negative = x < 0.0;
    let ax = x.abs();
    let precision: i32 = 6;
    let exp = ax.log10().floor() as i32;
    // `log10().floor()` can be off by one right at a power-of-ten
    // boundary due to float rounding (e.g. `100.0f64.log10()` landing
    // a hair under `2.0`) -- nudge it back using the fixed-notation
    // rendering at that exponent, exactly as CPython's own `_Py_dg_dtoa`
    // based formatter would place the decimal point.
    let exp = adjust_exponent(ax, exp);
    if (-4..precision).contains(&exp) {
        let decimals = (precision - 1 - exp).max(0) as usize;
        let mut s = format!("{ax:.decimals$}");
        strip_trailing_zeros(&mut s);
        if negative { format!("-{s}") } else { s }
    } else {
        let mantissa = ax / 10f64.powi(exp);
        let decimals = (precision - 1).max(0) as usize;
        let mut mant_s = format!("{mantissa:.decimals$}");
        strip_trailing_zeros(&mut mant_s);
        let sign = if exp >= 0 { "+" } else { "-" };
        format!(
            "{}{mant_s}e{sign}{:02}",
            if negative { "-" } else { "" },
            exp.abs()
        )
    }
}

fn adjust_exponent(ax: f64, exp: i32) -> i32 {
    if ax >= 10f64.powi(exp + 1) {
        exp + 1
    } else if ax < 10f64.powi(exp) {
        exp - 1
    } else {
        exp
    }
}

fn strip_trailing_zeros(s: &mut String) {
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_name_matches_pythons_own_labels() {
        assert_eq!(type_name(&Value::None), "null");
        assert_eq!(type_name(&Value::Bool(true)), "a boolean");
        assert_eq!(type_name(&Value::from(1i64)), "a number");
        assert_eq!(type_name(&Value::from(1.5f64)), "a number");
        assert_eq!(type_name(&Value::from("x")), "a string");
        assert_eq!(type_name(&Value::Dict(Dict::new())), "an object");
        assert_eq!(type_name(&Value::List(vec![])), "an array");
    }

    #[test]
    fn formats_small_integers_without_a_decimal_point() {
        assert_eq!(py_format_g(0.0), "0");
        assert_eq!(py_format_g(80.0), "80");
        assert_eq!(py_format_g(100.0), "100");
        assert_eq!(py_format_g(-5.0), "-5");
    }

    #[test]
    fn formats_fractional_values() {
        assert_eq!(py_format_g(0.5), "0.5");
        assert_eq!(py_format_g(99.9), "99.9");
    }

    #[test]
    fn rounds_to_six_significant_digits() {
        assert_eq!(py_format_g(123456.789), "123457");
    }

    #[test]
    fn falls_back_to_scientific_notation_outside_the_fixed_range() {
        assert_eq!(py_format_g(1e10), "1e+10");
        assert_eq!(py_format_g(1e-10), "1e-10");
    }
}
