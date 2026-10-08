//! Decision A (the fix-pass orchestrator notes on PR #388): a fixed,
//! documented nesting limit (`electricity_yaml::MAX_DEPTH`, 512), its
//! own error kind, and -- the actual point of the limit -- never a
//! stack overflow, checked by running the composer on a 2 MiB thread,
//! the same stack size a `cof` subprocess gets.
//!
//! Every test here runs on an explicit `std::thread::Builder` thread
//! (never the test harness's own thread, whose stack size is an
//! implementation detail) so a regression that reintroduces unbounded
//! recursion aborts *this* thread's `join`, not silently passes because
//! the main thread happened to have room.

use electricity_yaml::{YamlError, load_yaml};

const SMALL_STACK: usize = 2 * 1024 * 1024;

fn run_on_small_stack(text: String) -> Result<electricity_value::Value, YamlError> {
    std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(move || load_yaml(&text))
        .expect("spawning the probe thread")
        .join()
        .expect("the probe thread must not panic (never a stack overflow)")
}

/// `n` levels of a single-key block mapping (`a:` at increasing
/// one-space indent steps), bottoming out in a scalar -- an `n`-deep
/// mapping's own `depth()` is `n + 1` (the scalar leaf counts as one
/// more, per `Node::depth`'s doc comment), so `n = MAX_DEPTH - 1` is
/// exactly at the limit and `n = MAX_DEPTH` is one past it.
fn nested_block_mapping(n: usize) -> String {
    let mut text = String::new();
    for i in 0..n {
        text.push_str(&" ".repeat(i));
        text.push_str("a:\n");
    }
    text.push_str(&" ".repeat(n));
    text.push_str("1\n");
    text
}

/// `n` levels of a single-item flow sequence (`[...]`), bottoming out in
/// a scalar.
fn nested_flow_sequence(n: usize) -> String {
    format!("{}1{}\n", "[".repeat(n), "]".repeat(n))
}

#[test]
fn exactly_at_the_limit_parses_on_a_2mib_thread() {
    let text = nested_block_mapping(usize::try_from(electricity_yaml::MAX_DEPTH).unwrap() - 1);
    let value = run_on_small_stack(text).expect("exactly MAX_DEPTH levels must still parse");
    // Innermost value is the int 1, wrapped `MAX_DEPTH - 1` times.
    assert!(value.py_repr().starts_with("{'a': {'a':"));
}

#[test]
fn one_past_the_limit_is_a_distinct_nesting_error_not_a_scan_error() {
    let text = nested_block_mapping(usize::try_from(electricity_yaml::MAX_DEPTH).unwrap());
    let err = run_on_small_stack(text).unwrap_err();
    assert!(
        matches!(err, YamlError::NestingTooDeep { .. }),
        "expected NestingTooDeep, got {err:?}"
    );
}

#[test]
fn a_few_thousand_block_mapping_levels_errors_cleanly_on_a_2mib_thread() {
    let text = nested_block_mapping(5_000);
    let err = run_on_small_stack(text).unwrap_err();
    assert!(matches!(err, YamlError::NestingTooDeep { .. }));
}

/// saphyr-parser 0.1.0's scanner counts flow-collection nesting in its
/// own `u8` (`flow_level`), overflowing -- with its own, pre-existing
/// "recursion limit exceeded" `ScanError` -- at 255 levels, *below*
/// `MAX_DEPTH`. For a flow sequence specifically, that upstream guard
/// fires first, so 100,000 nested `[` never reaches this crate's own
/// depth check at all; what this test actually guarantees is the
/// requirement that matters -- no stack overflow -- regardless of which
/// of the two layers' errors comes back.
#[test]
fn one_hundred_thousand_nested_flow_sequences_does_not_crash() {
    let text = nested_flow_sequence(100_000);
    let result = run_on_small_stack(text);
    assert!(result.is_err());
}

#[test]
fn deeply_nested_block_mapping_at_dos_scale_does_not_crash() {
    let text = nested_block_mapping(100_000);
    let err = run_on_small_stack(text).unwrap_err();
    assert!(matches!(err, YamlError::NestingTooDeep { .. }));
}

/// A chain of aliases, each cloning the whole previous (already
/// anchored) subtree: textually one level deeper per entry, but -- once
/// expanded -- `N` levels deep. `N = 600` alone already exceeds
/// `MAX_DEPTH`; the real risk this guards is each `cloned()` node's
/// depth compounding past the limit without ever recursing through
/// `compose_from_event` to get there (`MAX_DEPTH`'s doc comment).
fn alias_chain(n: usize) -> String {
    let mut text = String::from("a0: &a0 [0]\n");
    for i in 1..n {
        text.push_str(&format!("a{i}: &a{i} [*a{}]\n", i - 1));
    }
    text
}

#[test]
fn alias_chain_n_600_is_a_distinct_nesting_error() {
    let text = alias_chain(600);
    let err = run_on_small_stack(text).unwrap_err();
    assert!(matches!(err, YamlError::NestingTooDeep { .. }));
}

#[test]
fn alias_chain_n_100_000_does_not_crash() {
    let text = alias_chain(100_000);
    let err = run_on_small_stack(text).unwrap_err();
    assert!(matches!(err, YamlError::NestingTooDeep { .. }));
}

/// Same as the alias chain above, but merge keys (`<<:`) rather than a
/// plain alias: each mapping merges the previous one, which the
/// composer's `flatten_pairs` expands in place rather than preserving as
/// a reference, so this is the merge-specific version of the same
/// compounding-depth risk.
fn merge_chain(n: usize) -> String {
    let mut text = String::from("b0: &b0 {x: 0}\n");
    for i in 1..n {
        text.push_str(&format!("b{i}: &b{i} {{<<: *b{}}}\n", i - 1));
    }
    text
}

#[test]
fn merge_chain_n_600_is_a_distinct_nesting_error() {
    let text = merge_chain(600);
    let err = run_on_small_stack(text).unwrap_err();
    assert!(matches!(err, YamlError::NestingTooDeep { .. }));
}

#[test]
fn merge_chain_n_100_000_does_not_crash() {
    let text = merge_chain(100_000);
    let err = run_on_small_stack(text).unwrap_err();
    assert!(matches!(err, YamlError::NestingTooDeep { .. }));
}
