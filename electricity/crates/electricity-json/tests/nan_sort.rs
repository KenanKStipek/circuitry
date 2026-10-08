//! `sort_keys=True` on a dict with 64 or more keys including a `NaN`
//! float key is a documented divergence (`electricity_json`'s crate
//! docs): a stable sort that puts every `NaN` key after every non-`NaN`
//! key, not CPython's exact order. It must still *never panic* — Rust's
//! `[T]::sort_by` has required a consistent/total comparator since 1.81,
//! and a comparator that treats every `NaN`-involved pair as
//! `Ordering::Equal` to everything isn't one (`-1 == NaN == 0`, yet
//! `-1 < 0`).

use electricity_json::{WriteMode, dumps};
use electricity_value::{Dict, Value};

/// A fixed, deterministic 64-bit PRNG (SplitMix64), so these tests don't
/// need the `rand` crate as a dependency.
struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        SplitMix64(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }
}

/// A hand-traced counterexample (see the PR review this pins): 64 keys
/// in insertion order `0.0, 1.0, 2.0, NaN, -4.0, -3.0, -2.0, -1.0,
/// -100.0`, then 55 more distinct floats (`100.0..155.0`). `count_run`
/// finds an initial run of 8 below the 32-element threshold that would
/// keep this on the small-sort path, so with exactly 64 keys this goes
/// through the 64-or-more fallback — the one `sort_with_nan_as_equal`
/// (its previous name) could panic on, because `sort4_stable` on the
/// first 32 elements produces two runs (`[0, 1, 2, NaN]` and
/// `[-4, -3, -2, -1]`) whose merge draws from both sides.
#[test]
fn sixty_four_key_nan_fallback_does_not_panic() {
    let mut dict: Dict = Dict::new();
    let mut keys: Vec<f64> = vec![0.0, 1.0, 2.0, f64::NAN, -4.0, -3.0, -2.0, -1.0, -100.0];
    keys.extend((0..55).map(|i| 100.0 + i as f64));
    assert_eq!(keys.len(), 64);
    for k in keys {
        dict.insert(Value::Float(k), Value::None);
    }
    let value = Value::Dict(dict);
    dumps(&value, WriteMode::PRETTY).expect("a NaN-keyed 64-entry dict must not panic or error");
}

/// A seeded loop of random 64..200-key dicts with a `NaN` key, in random
/// insertion order, across several sizes and seeds: the fallback must
/// never panic (nor, since every key here is numeric-tower and thus
/// mutually comparable, ever raise).
#[test]
fn large_nan_keyed_dicts_never_panic() {
    for seed in 0u64..20 {
        for size in [64usize, 65, 100, 127, 128, 150, 199, 200] {
            let mut rng = SplitMix64::new(seed * 1000 + size as u64);
            let mut keys: Vec<f64> = (0..size).map(|i| i as f64 - (size as f64 / 2.0)).collect();
            let nan_at = rng.below(keys.len());
            keys[nan_at] = f64::NAN;
            rng.shuffle(&mut keys);

            let mut dict: Dict = Dict::new();
            for k in keys {
                dict.insert(Value::Float(k), Value::None);
            }
            let value = Value::Dict(dict);
            dumps(&value, WriteMode::PRETTY)
                .expect("a random NaN-keyed dict of 64+ comparable keys must not panic or error");
        }
    }
}
