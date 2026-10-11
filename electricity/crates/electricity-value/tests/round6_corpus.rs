//! Replays `electricity/scripts/generate_round6_corpus.py`'s own
//! differential corpus -- CPython's real `round(x, 6)` on a mix of
//! edge cases and random floats (both "ordinary" magnitudes and raw
//! random bit patterns) -- against
//! [`electricity_value::pycompat::round6`].
//!
//! The corpus is checked in (`tests/golden/round6_corpus.json`); CI
//! separately regenerates it with Python 3.11 and fails the build if
//! it differs (`.github/workflows/electricity-generated.yml`), so this
//! test only needs to trust the committed file.

use electricity_value::pycompat::round6;
use serde::Deserialize;

#[derive(Deserialize)]
struct Round6Case {
    x: String,
    expected: String,
}

#[derive(Deserialize)]
struct Corpus {
    round6_cases: Vec<Round6Case>,
}

fn from_hex_bits(hex: &str) -> f64 {
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("valid hex byte"))
        .collect();
    let mut arr = [0u8; 8];
    arr.copy_from_slice(&bytes[..8]);
    f64::from_le_bytes(arr)
}

#[test]
fn golden_corpus_round6() {
    let text = include_str!("golden/round6_corpus.json");
    let corpus: Corpus =
        serde_json::from_str(text).expect("golden/round6_corpus.json is valid JSON");
    assert!(!corpus.round6_cases.is_empty());

    let mut failures = Vec::new();
    for (i, case) in corpus.round6_cases.iter().enumerate() {
        let x = from_hex_bits(&case.x);
        let expected = from_hex_bits(&case.expected);
        let got = round6(x);
        // Bit-identical, not `==`: `-0.0 == 0.0` in IEEE 754, but
        // CPython's own `round(-0.0, 6)` preserves the sign, and a
        // bitwise mismatch there is exactly the kind of divergence
        // this corpus exists to catch.
        if got.to_bits() != expected.to_bits() {
            failures.push(format!(
                "case {i} (x={x:?}): expected round6={expected:?} (bits {:016x}), got {got:?} (bits {:016x})",
                expected.to_bits(),
                got.to_bits()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
