//! `repr(float)`/`str(float)` parity with CPython 3.11 (the two are
//! identical for floats since Python 3.1).
//!
//! CPython's `repr` uses the shortest decimal digit string that round-trips
//! back to the same `f64`, then lays it out in fixed or exponential form by
//! the decimal exponent alone (`Python/pystrtod.c`'s `format_float_short`,
//! mode `'r'`). Rust's `{:e}` formatter already produces the shortest
//! round-trip digits (`core::num::flt2dec`); this module only re-derives
//! CPython's layout rule on top of it.

/// Formats `f` exactly as CPython 3.11's `repr(f)` (equivalently `str(f)`).
pub fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let neg = f.is_sign_negative();
    let (digits, exp) = shortest_digits_and_exponent(f.abs());
    let body = layout(&digits, exp);
    if neg { format!("-{body}") } else { body }
}

/// Decomposes a finite, positive, nonzero `f64` into its shortest
/// round-trip decimal digit string (no leading/trailing zeros) and the
/// decimal exponent `exp` such that the value equals `0.<digits> *
/// 10^(exp + 1)`, i.e. `digits[0]` is the units digit of `10^exp`.
fn shortest_digits_and_exponent(f: f64) -> (String, i32) {
    let formatted = format!("{f:e}");
    let (mantissa, exp_str) = formatted
        .split_once('e')
        .expect("`{:e}` formatting always contains an 'e'");
    let exp: i32 = exp_str.parse().expect("exponent is always a valid integer");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    (digits, exp)
}

/// Lays out `digits`/`exp` (as produced by [`shortest_digits_and_exponent`])
/// in CPython's `repr` style: exponential when `exp < -4 || exp >= 16`,
/// fixed otherwise, always with at least one digit on each side of `.`.
fn layout(digits: &str, exp: i32) -> String {
    if !(-4..16).contains(&exp) {
        let mut s = String::new();
        s.push(digits.as_bytes()[0] as char);
        if digits.len() > 1 {
            s.push('.');
            s.push_str(&digits[1..]);
        }
        s.push('e');
        s.push(if exp >= 0 { '+' } else { '-' });
        s.push_str(&format!("{:02}", exp.unsigned_abs()));
        s
    } else if exp >= 0 {
        let int_len = (exp + 1) as usize;
        if digits.len() > int_len {
            let (int_part, frac_part) = digits.split_at(int_len);
            format!("{int_part}.{frac_part}")
        } else {
            let zeros = int_len - digits.len();
            format!("{digits}{}.0", "0".repeat(zeros))
        }
    } else {
        let zeros = (-exp - 1) as usize;
        format!("0.{}{digits}", "0".repeat(zeros))
    }
}

#[cfg(test)]
mod tests {
    use super::py_float_repr;

    #[test]
    fn hand_picked_cases_match_cpython_3_11() {
        let cases: &[(f64, &str)] = &[
            (100.0, "100.0"),
            (1.0, "1.0"),
            (-1.0, "-1.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (123456789012345680.0, "1.2345678901234568e+17"),
            (0.1, "0.1"),
            (5e-324, "5e-324"),
            (f64::MAX, "1.7976931348623157e+308"),
            (2.2250738585072014e-308, "2.2250738585072014e-308"),
            (0.5, "0.5"),
            (-0.5, "-0.5"),
        ];
        for (value, expected) in cases {
            assert_eq!(&py_float_repr(*value), expected, "repr({value:?})");
        }
    }

    #[test]
    fn signed_zero() {
        assert_eq!(py_float_repr(0.0), "0.0");
        assert_eq!(py_float_repr(-0.0), "-0.0");
    }

    #[test]
    fn non_finite() {
        assert_eq!(py_float_repr(f64::NAN), "nan");
        assert_eq!(py_float_repr(f64::INFINITY), "inf");
        assert_eq!(py_float_repr(f64::NEG_INFINITY), "-inf");
    }
}
