//! Python ints are unbounded; [`IntValue`] is an `i64` fast path with a
//! [`BigInt`] fallback for anything that overflows it.

use num_bigint::BigInt;
use num_traits::ToPrimitive;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

/// An arbitrary-precision integer, represented as `i64` whenever the value
/// fits (the common case) and as a [`BigInt`] otherwise.
///
/// Construction always normalizes: a `Big` variant never holds a value that
/// fits in `i64`, so two `IntValue`s that are numerically equal are always
/// represented the same way and compare/hash identically.
#[derive(Debug, Clone)]
pub enum IntValue {
    Small(i64),
    Big(BigInt),
}

impl IntValue {
    /// Builds an `IntValue` from a `BigInt`, normalizing to `Small` when it fits.
    pub fn from_bigint(n: BigInt) -> Self {
        match n.to_i64() {
            Some(small) => IntValue::Small(small),
            None => IntValue::Big(n),
        }
    }

    /// Parses a decimal (optionally `-`-signed) string of digits into an `IntValue`.
    ///
    /// Returns `None` if `s` is not a valid base-10 integer literal.
    pub fn parse_decimal(s: &str) -> Option<Self> {
        if let Ok(small) = s.parse::<i64>() {
            return Some(IntValue::Small(small));
        }
        s.parse::<BigInt>().ok().map(IntValue::from_bigint)
    }

    /// Borrows the value as a [`BigInt`], widening the fast path if needed.
    pub fn to_bigint(&self) -> BigInt {
        match self {
            IntValue::Small(n) => BigInt::from(*n),
            IntValue::Big(n) => n.clone(),
        }
    }

    /// The value as `f64`, following Python's int-to-float conversion
    /// (round-to-nearest-even on precision loss; never panics).
    pub fn to_f64(&self) -> f64 {
        match self {
            IntValue::Small(n) => *n as f64,
            IntValue::Big(n) => bigint_to_f64(n),
        }
    }

    /// `true` iff the value is `0`.
    pub fn is_zero(&self) -> bool {
        match self {
            IntValue::Small(n) => *n == 0,
            IntValue::Big(n) => n.sign() == num_bigint::Sign::NoSign,
        }
    }

    /// `true` iff the value is strictly less than `0`.
    pub fn is_negative(&self) -> bool {
        match self {
            IntValue::Small(n) => *n < 0,
            IntValue::Big(n) => n.sign() == num_bigint::Sign::Minus,
        }
    }
}

/// Converts a `BigInt` to `f64` the way CPython's `int.__float__` does:
/// shift the magnitude down to 53 significant bits with round-to-nearest
/// -even, then scale back up, rather than accumulating digit-by-digit
/// (which would round each digit independently and give the wrong answer
/// for large magnitudes).
fn bigint_to_f64(n: &BigInt) -> f64 {
    use num_bigint::Sign;
    if n.sign() == Sign::NoSign {
        return 0.0;
    }
    let (sign, mag) = n.clone().into_parts();
    let bits = mag.bits();
    let f = if bits <= 53 {
        mag.to_f64().unwrap_or(f64::INFINITY)
    } else {
        let shift = bits - 53;
        let shifted = &mag >> shift;
        let rounded = round_half_to_even_shift(&mag, shift, &shifted);
        let base = rounded.to_f64().unwrap_or(f64::INFINITY);
        base * 2f64.powi(shift as i32)
    };
    match sign {
        Sign::Minus => -f,
        _ => f,
    }
}

/// Rounds `mag >> shift` to the nearest integer, breaking exact ties to even,
/// given the already-computed `shifted = mag >> shift`.
fn round_half_to_even_shift(
    mag: &num_bigint::BigUint,
    shift: u64,
    shifted: &num_bigint::BigUint,
) -> num_bigint::BigUint {
    use num_bigint::BigUint;
    use num_traits::{One, Zero};
    let remainder = mag - (shifted << shift);
    let half = BigUint::one() << (shift - 1);
    match remainder.cmp(&half) {
        Ordering::Less => shifted.clone(),
        Ordering::Greater => shifted + BigUint::one(),
        Ordering::Equal => {
            if (shifted % 2u8).is_zero() {
                shifted.clone()
            } else {
                shifted + BigUint::one()
            }
        }
    }
}

impl PartialEq for IntValue {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for IntValue {}

impl PartialOrd for IntValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IntValue {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (IntValue::Small(a), IntValue::Small(b)) => a.cmp(b),
            _ => self.to_bigint().cmp(&other.to_bigint()),
        }
    }
}

impl Hash for IntValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Always hash through BigInt's own Hash so Small(1) and
        // Big(BigInt::from(1)) (which construction never actually produces,
        // but a future caller might by hand) still collide.
        self.to_bigint().hash(state);
    }
}

impl fmt::Display for IntValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IntValue::Small(n) => write!(f, "{n}"),
            IntValue::Big(n) => write!(f, "{n}"),
        }
    }
}

impl From<i64> for IntValue {
    fn from(n: i64) -> Self {
        IntValue::Small(n)
    }
}

impl From<BigInt> for IntValue {
    fn from(n: BigInt) -> Self {
        IntValue::from_bigint(n)
    }
}
