//! Regression test for a quadratic slowdown the position-tracking fix
//! (`Composer::byte_offset`, `compose.rs`) introduced: converting a
//! `saphyr_parser::Marker` to a byte offset by walking forward from the
//! line's start on every call made a single long line -- a one-line
//! flow mapping, the shape JSON-like plans models often emit -- cost
//! O(column) per event, O(n^2) overall. `byte_offset` now advances from
//! an incremental cursor instead.
//!
//! Compares *growth*, not an absolute wall-clock bound: an absolute
//! bound has to be loose enough to survive `cargo test --workspace`'s
//! debug build (where only this crate itself gets `opt-level = 2` --
//! `saphyr-parser`, `regex` and `electricity-value` build at 0 --
//! sharing the machine with every other test running in parallel), and
//! loose enough to survive that is loose enough to miss a real
//! regression. Going from 10k to 100k keys (10x the input) costs about
//! 10x the time if this is linear, about 100x if it's quadratic again;
//! asserting well below 100x catches the regression with headroom to
//! spare on either side, on any machine, under any load.

use std::time::Instant;

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

fn load_and_time(n: usize) -> std::time::Duration {
    let text = flow_mapping(n);
    let start = Instant::now();
    let value = electricity_yaml::load_yaml(&text).expect("a well-formed flow mapping");
    let elapsed = start.elapsed();
    assert!(!value.py_repr().is_empty());
    elapsed
}

#[test]
fn one_line_flow_mapping_scales_linearly_not_quadratically() {
    // The smallest run's own cost is paid first and discarded: a
    // process's one-time warm-up (allocator growth, code paths touched
    // for the first time) would otherwise inflate the ratio's
    // denominator and hide a real quadratic regression behind a falsely
    // small one.
    load_and_time(1_000);
    let small = load_and_time(10_000);
    let large = load_and_time(100_000);

    let small_secs = small.as_secs_f64().max(1e-6);
    let ratio = large.as_secs_f64() / small_secs;
    assert!(
        ratio < 30.0,
        "100k keys took {large:?} against 10k keys' {small:?} ({ratio:.1}x) -- \
         Composer::byte_offset may have regressed to its old O(column) \
         per-call behaviour (quadratic over the whole line, about 100x \
         for a 10x input instead of linear's about 10x)"
    );
}
