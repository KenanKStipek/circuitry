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

/// Like [`bigint_cmp_f64`], but for an `i64` rather than a [`BigInt`]: the
/// fast path for `Bool`/`Int` comparisons against a `Float`, done entirely
/// in `i128` so it never allocates. `f`'s magnitude is checked against
/// `i64`'s range first — once it's outside, no `i64` could possibly equal
/// it, so the exact shift-and-compare below only has to handle magnitudes
/// that actually fit.
pub fn i64_cmp_f64(i: i64, f: f64) -> Option<Ordering> {
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
        return Some(i.cmp(&0));
    }
    let (sign, mantissa, exponent) = decompose(f);
    if exponent >= 12 {
        // The mantissa's implicit leading bit means magnitude >= 2^(52 +
        // exponent) >= 2^64, already outside any `i64` (including
        // `i64::MIN`'s magnitude of exactly 2^63).
        return Some(if sign > 0 {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    if exponent <= -53 {
        // `mantissa` has at most 53 significant bits, so magnitude <
        // 2^(53 + exponent) <= 1: strictly between 0 and 1. No nonzero
        // `i64` can tie with that, so the comparison reduces to signs
        // (computing it the way the branch below does would otherwise
        // need a shift of `i` by more than `i64` has bits to spare).
        return Some(if i != 0 {
            if i > 0 {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        } else if sign > 0 {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    let signed_mantissa = if sign < 0 {
        -(mantissa as i128)
    } else {
        mantissa as i128
    };
    // `exponent` is now in `-52..12`, so shifting either side by its
    // magnitude stays well within `i128`.
    let (lhs, rhs): (i128, i128) = if exponent >= 0 {
        (i as i128, signed_mantissa << exponent)
    } else {
        ((i as i128) << (-exponent) as u32, signed_mantissa)
    };
    Some(lhs.cmp(&rhs))
}

/// `true` iff `i == f` using exact (not float-conversion) comparison.
pub fn i64_eq_f64(i: i64, f: f64) -> bool {
    i64_cmp_f64(i, f) == Some(Ordering::Equal)
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

    #[test]
    fn i64_matches_bigint_for_simple_cases() {
        assert_eq!(i64_cmp_f64(1, 1.0), Some(Ordering::Equal));
        assert_eq!(i64_cmp_f64(1, 1.5), Some(Ordering::Less));
        assert_eq!(i64_cmp_f64(2, 1.5), Some(Ordering::Greater));
        assert_eq!(i64_cmp_f64(0, -0.0), Some(Ordering::Equal));
        assert_eq!(i64_cmp_f64(-1, -1.0), Some(Ordering::Equal));
        assert!(i64_eq_f64(-1, -1.0));
    }

    #[test]
    fn i64_nan_and_infinite() {
        assert_eq!(i64_cmp_f64(1, f64::NAN), None);
        assert_eq!(i64_cmp_f64(1, f64::INFINITY), Some(Ordering::Less));
        assert_eq!(i64_cmp_f64(1, f64::NEG_INFINITY), Some(Ordering::Greater));
    }

    #[test]
    fn i64_cmp_f64_agrees_with_bigint_cmp_f64() {
        let cases: &[(i64, f64)] = &[
            (i64::MAX, i64::MAX as f64),
            (i64::MIN, i64::MIN as f64),
            (i64::MIN, i64::MIN as f64 + 2048.0), // f still exactly == i64::MIN at this spacing
            (i64::MAX, 1e300),
            (i64::MIN, -1e300),
            (0, 0.1),
            (0, -0.1),
            (-5, -5.5),
            (1_000_000_000_000, 1e12),
            (1_000_000_000_000, 1e12 + 1.0),
            (0, 1.0),
            (1, 0.9999999999999999),
            (-1, -0.9999999999999999),
            (1i64 << 62, (1i64 << 62) as f64),
            (-(1i64 << 62), -((1i64 << 62) as f64)),
        ];
        for &(i, f) in cases {
            assert_eq!(
                i64_cmp_f64(i, f),
                bigint_cmp_f64(&BigInt::from(i), f),
                "i={i} f={f}"
            );
        }
    }
}
