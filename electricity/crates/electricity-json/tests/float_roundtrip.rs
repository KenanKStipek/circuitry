//! Property test (issue #377): every finite float, written and read back
//! through `electricity_json`, round-trips bit for bit. `NaN` can't (JSON
//! has exactly one `NaN` literal, not one per payload — a format
//! limitation, not a bug), so it's checked separately with `is_nan()`.
//!
//! A small inline splitmix64 generator keeps this self-contained rather
//! than adding a `rand`/`proptest` dependency for one test file.

use electricity_json::{WriteMode, dumps, loads};
use electricity_value::Value;

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
}

fn round_trip(f: f64, mode: WriteMode) -> f64 {
    let text = dumps(&Value::Float(f), mode).expect("a float always writes");
    match loads(&text).expect("writer output is always valid JSON") {
        Value::Float(got) => got,
        other => panic!("expected a float back, got {other:?}"),
    }
}

#[test]
fn every_float_bit_pattern_round_trips() {
    let mut rng = SplitMix64(0x05EE_D373_70D3_7377_u64);
    for mode in [WriteMode::COMPACT, WriteMode::PRETTY] {
        for _ in 0..20_000 {
            let bits = rng.next();
            let f = f64::from_bits(bits);
            if f.is_nan() {
                assert!(round_trip(f, mode).is_nan());
            } else {
                assert_eq!(round_trip(f, mode).to_bits(), f.to_bits(), "f = {f:?}");
            }
        }
    }
}

#[test]
fn non_finite_literals_round_trip() {
    for mode in [WriteMode::COMPACT, WriteMode::PRETTY] {
        assert_eq!(
            round_trip(f64::INFINITY, mode).to_bits(),
            f64::INFINITY.to_bits()
        );
        assert_eq!(
            round_trip(f64::NEG_INFINITY, mode).to_bits(),
            f64::NEG_INFINITY.to_bits()
        );
        assert!(round_trip(f64::NAN, mode).is_nan());
    }
}

/// Fixed values alongside the random sample above: the sign of zero, the
/// smallest and largest finite magnitudes, and the `1e16`/`1e-4` digit-
/// count boundaries `electricity-value`'s float repr switches format at.
#[test]
fn float_repr_edge_values_round_trip() {
    let edge_values = [
        0.0,
        -0.0,
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
        5e-324, // smallest subnormal
        -5e-324,
        f64::MAX,
        f64::MIN,
        1e16,
        9.999999999999998e15,
        1e-4,
        9.999999999999999e-5,
    ];
    for mode in [WriteMode::COMPACT, WriteMode::PRETTY] {
        for f in edge_values {
            assert_eq!(round_trip(f, mode).to_bits(), f.to_bits(), "f = {f:?}");
        }
    }
}
