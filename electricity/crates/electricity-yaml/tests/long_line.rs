//! Regression test for a quadratic slowdown the position-tracking fix
//! (`Composer::byte_offset`, `compose.rs`) introduced: converting a
//! `saphyr_parser::Marker` to a byte offset by walking forward from the
//! line's start on every call made a single long line -- a one-line
//! flow mapping, the shape JSON-like plans models often emit -- cost
//! O(column) per event, O(n^2) overall. `byte_offset` now advances from
//! an incremental cursor instead; this test is a timing-bounded check
//! that a regression back to the quadratic form fails loudly rather
//! than just getting slower.

use std::time::{Duration, Instant};

fn flow_mapping(n: usize) -> String {
    let mut text = String::from("{");
    for i in 0..n {
        if i > 0 {
            text.push_str(", ");
        }
        text.push_str(&format!("k{i}: {i}"));
    }
    text.push_str("}\n");
    text
}

#[test]
fn one_line_flow_mapping_with_100k_keys_loads_in_linear_time() {
    let text = flow_mapping(100_000);
    let start = Instant::now();
    let value = electricity_yaml::load_yaml(&text).expect("a well-formed flow mapping");
    let elapsed = start.elapsed();
    assert!(!value.py_repr().is_empty());
    // The quadratic form took tens of seconds (minutes, extrapolating)
    // for a line this long; the linear one takes well under a second on
    // any machine this test runs on. A generous bound well clear of
    // normal machine-load variance, but far below what a reintroduced
    // O(n^2) would take.
    assert!(
        elapsed < Duration::from_secs(5),
        "loading a 100k-key single-line flow mapping took {elapsed:?} -- \
         Composer::byte_offset may have regressed to its old O(column) \
         per-call behaviour (quadratic over the whole line)"
    );
}
