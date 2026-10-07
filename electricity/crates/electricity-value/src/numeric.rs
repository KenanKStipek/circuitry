//! Exact (no precision loss) comparison between an arbitrary-precision
//! integer and an `f64`, needed because Python's numeric tower compares
//! `int` and `float` *exactly* rather than by converting the int to a
//! (possibly lossy) float first — `10**20 == float(10**20)` and
//! `10**20 + 1 != float(10**20)` both hold, even though the two integers
//! round to the same `f64`.

use num_bigint::BigInt;
use std::cmp::Ordering;

/// Decomposes a finite, nonzero `f64` into `(sign, mantissa, exponent)`
/// such that the value equals `sign * mantissa * 2^exponent` exactly,
/// `mantissa` having at most 53 significant bits.
fn decompose(f: f64) -> (i8, u64, i64) {
    let bits = f.to_bits();
    let sign: i8 = if bits >> 63 == 1 { -1 } else { 1 };
    let biased_exp = ((bits >> 52) & 0x7ff) as i64;
    let mantissa_bits = bits & 0x000f_ffff_ffff_ffff;
    if biased_exp == 0 {
        (sign, mantissa_bits, -1074) // subnormal
    } else {
        (sign, mantissa_bits | (1u64 << 52), biased_exp - 1075)
    }
}

/// The exact integer value of `f`, or `None` if `f` is non-finite or has a
/// nonzero fractional part.
pub fn exact_integer_value(f: f64) -> Option<BigInt> {
    if f == 0.0 {
        return Some(BigInt::from(0));
    }
    if !f.is_finite() {
        return None;
    }
    let (sign, mantissa, exponent) = decompose(f);
    let magnitude = if exponent >= 0 {
        BigInt::from(mantissa) << exponent
    } else {
        let shift = (-exponent) as u32;
        if (mantissa.trailing_zeros() as i64) < -exponent {
            return None; // fractional
        }
        BigInt::from(mantissa >> shift)
    };
    Some(if sign < 0 { -magnitude } else { magnitude })
}

/// Compares an arbitrary-precision integer to an `f64` with no precision
/// loss. Returns `None` for `NaN` (IEEE-754 unordered, not an error — the
/// caller renders this as "every relational operator is false", matching
/// Python's own `float('nan') < 1` etc.).
pub fn bigint_cmp_f64(i: &BigInt, f: f64) -> Option<Ordering> {
    if f.is_nan() {
        return None;
    }
    if f.is_infinite() {
        return Some(if f > 0.0 {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    if f == 0.0 {
        return Some(i.cmp(&BigInt::from(0)));
    }
    let (sign, mantissa, exponent) = decompose(f);
    let mantissa = BigInt::from(mantissa);
    let signed_mantissa = if sign < 0 { -mantissa } else { mantissa };
    if exponent >= 0 {
        Some(i.cmp(&(signed_mantissa << exponent)))
    } else {
        let scaled_i = i << (-exponent) as u32;
        Some(scaled_i.cmp(&signed_mantissa))
    }
}

/// `true` iff `i == f` using exact (not float-conversion) comparison.
pub fn bigint_eq_f64(i: &BigInt, f: f64) -> bool {
    bigint_cmp_f64(i, f) == Some(Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_equality_beyond_f64_precision() {
        let huge: BigInt = "100000000000000000000".parse().unwrap(); // 10**20
        let huge_plus_one: BigInt = "100000000000000000001".parse().unwrap();
        let f = 1e20_f64;
        assert!(bigint_eq_f64(&huge, f));
        assert!(!bigint_eq_f64(&huge_plus_one, f));
    }

    #[test]
    fn simple_cases() {
        assert_eq!(bigint_cmp_f64(&BigInt::from(1), 1.0), Some(Ordering::Equal));
        assert_eq!(bigint_cmp_f64(&BigInt::from(1), 1.5), Some(Ordering::Less));
        assert_eq!(
            bigint_cmp_f64(&BigInt::from(2), 1.5),
            Some(Ordering::Greater)
        );
        assert_eq!(
            bigint_cmp_f64(&BigInt::from(0), -0.0),
            Some(Ordering::Equal)
        );
    }

    #[test]
    fn nan_and_infinite() {
        assert_eq!(bigint_cmp_f64(&BigInt::from(1), f64::NAN), None);
        assert_eq!(
            bigint_cmp_f64(&BigInt::from(1), f64::INFINITY),
            Some(Ordering::Less)
        );
        assert_eq!(
            bigint_cmp_f64(&BigInt::from(1), f64::NEG_INFINITY),
            Some(Ordering::Greater)
        );
    }

    #[test]
    fn negative_values() {
        assert_eq!(
            bigint_cmp_f64(&BigInt::from(-1), -1.0),
            Some(Ordering::Equal)
        );
        assert!(bigint_eq_f64(&(-BigInt::from(1)), -1.0));
    }
}
