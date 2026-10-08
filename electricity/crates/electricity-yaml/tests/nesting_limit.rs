//! Decision A (the fix-pass orchestrator notes on PR #388): a fixed,
//! documented nesting limit (`electricity_yaml::MAX_DEPTH`, 512), its
//! own error kind, and -- the actual point of the limit -- never a
//! stack overflow, checked by running the composer on a 2 MiB thread, a
//! deliberately small, conservative size (not a measured figure for any
//! specific caller -- `Cargo.toml`'s own profile-override comment).
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
/// mapping's own `depth()` is exactly `n` (a scalar leaf's own depth is
/// 0, per `Node::depth`'s doc comment: it isn't a container, so it
/// doesn't consume one of `MAX_DEPTH`'s own units), so `n = MAX_DEPTH`
/// is exactly at the limit and `n = MAX_DEPTH + 1` is one past it.
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
    let text = nested_block_mapping(electricity_yaml::MAX_DEPTH);
    let value = run_on_small_stack(text).expect("exactly MAX_DEPTH levels must still parse");
    // Innermost value is the int 1, wrapped `MAX_DEPTH` times.
    assert!(value.py_repr().starts_with("{'a': {'a':"));
}

#[test]
fn one_past_the_limit_is_a_distinct_nesting_error_not_a_scan_error() {
    let text = nested_block_mapping(electricity_yaml::MAX_DEPTH + 1);
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

/// Decision B (the fix-pass orchestrator notes on PR #388): a fixed,
/// documented budget on the total number of nodes alias expansion may
/// construct (`electricity_yaml::MAX_NODES`), checked *before* each
/// clone using the anchored node's own already-known size -- never by
/// performing the clone and counting afterward -- with its own error
/// kind, so a billion-laughs-style alias chain fails fast rather than
/// exhausting memory. `a0: &a0 [x x 10]`, `a1: &a1 [*a0 x 10]`, ...,
/// through `a8: &a8 [*a7 x 10]` would clone roughly 10^8 nodes if fully
/// expanded (PyYAML shares one object per anchor instead, so this same
/// document loads cheaply under Circuitry's own loader) -- this must
/// fail quickly, on the same small stack the depth tests above use,
/// without ever allocating that expansion.
fn alias_fan_out(levels: usize, fan_out: usize) -> String {
    let mut text = String::from("a0: &a0 [x,x,x,x,x,x,x,x,x,x]\n");
    for level in 1..levels {
        let prev = level - 1;
        let refs = vec![format!("*a{prev}"); fan_out].join(", ");
        text.push_str(&format!("a{level}: &a{level} [{refs}]\n"));
    }
    text
}

#[test]
fn alias_fan_out_9_levels_of_10_is_a_distinct_node_budget_error() {
    let text = alias_fan_out(9, 10);
    let err = run_on_small_stack(text).unwrap_err();
    assert!(
        matches!(err, YamlError::AliasExpansionTooLarge { .. }),
        "expected AliasExpansionTooLarge, got {err:?}"
    );
}

/// The linear-size version of the same risk: one anchor of linear size
/// (10,000 items), referenced 10,000 times -- about 10^8 nodes if fully
/// expanded, with no single alias use anywhere near that size on its
/// own, so this specifically exercises the *cumulative* running total
/// across many separate clones, not just one clone's own size.
fn linear_anchor_fan_out(item_count: usize, reference_count: usize) -> String {
    let mut text = String::from("a: &a [");
    for i in 0..item_count {
        if i > 0 {
            text.push(',');
        }
        text.push('0');
    }
    text.push_str("]\n");
    for i in 0..reference_count {
        text.push_str(&format!("b{i}: *a\n"));
    }
    text
}

#[test]
fn ten_thousand_references_to_a_ten_thousand_item_anchor_is_a_distinct_node_budget_error() {
    let text = linear_anchor_fan_out(10_000, 10_000);
    let err = run_on_small_stack(text).unwrap_err();
    assert!(
        matches!(err, YamlError::AliasExpansionTooLarge { .. }),
        "expected AliasExpansionTooLarge, got {err:?}"
    );
}

/// `n` levels of a single-item *block* sequence (`- 1`, `- - 1`, ...) --
/// the D3 fix-pass gap: every depth test above used a block mapping or
/// a *flow* sequence; a flow sequence can't reach `MAX_DEPTH` at all
/// (saphyr-parser 0.1.0's own flow-nesting counter is a `u8`, overflowing
/// with its own, unrelated error at 255 levels, well below 512 --
/// `lib.rs`'s "Known divergences"), so this is the one shape that
/// actually exercises the limit for sequences specifically.
fn nested_block_sequence(n: usize) -> String {
    let mut text = String::new();
    for _ in 0..n {
        text.push_str("- ");
    }
    text.push_str("1\n");
    text
}

#[test]
fn block_sequence_exactly_at_the_limit_parses_on_a_2mib_thread() {
    let text = nested_block_sequence(electricity_yaml::MAX_DEPTH);
    let value = run_on_small_stack(text).expect("exactly MAX_DEPTH levels must still parse");
    assert!(value.py_repr().starts_with("[[["));
}

#[test]
fn block_sequence_one_past_the_limit_is_a_distinct_nesting_error() {
    let text = nested_block_sequence(electricity_yaml::MAX_DEPTH + 1);
    let err = run_on_small_stack(text).unwrap_err();
    assert!(matches!(err, YamlError::NestingTooDeep { .. }));
}

/// A deeply nested *complex mapping key* (`? ... : v`) -- the explicit
/// `?` indicator a plain `a: b` mapping never uses, and the other gap
/// in D3's test coverage: every existing depth test nests through an
/// ordinary value position, never a key. `n` is chosen so the whole
/// mapping (`1 +` the key's own depth, `Node::depth_of_pairs`) lands
/// exactly on `MAX_DEPTH`; one level deeper must report
/// `NestingTooDeep` during *composition*, before construction ever gets
/// a chance to fail on the key being unhashable (a list key, same as
/// the nested-sequence key itself is -- unrelated to nesting, confirmed
/// by the exactly-at-the-limit case below also failing that way, not by
/// crashing or misreporting the depth).
fn nested_sequence_key(n: usize) -> String {
    format!("? {}1\n: v\n", "- ".repeat(n))
}

#[test]
fn container_key_exactly_at_the_limit_parses_past_composition_on_a_2mib_thread() {
    let text = nested_sequence_key(electricity_yaml::MAX_DEPTH - 1);
    let err = run_on_small_stack(text).unwrap_err();
    assert!(
        matches!(err, YamlError::UnhashableKey { .. }),
        "expected composition to succeed and only the ordinary \
         unhashable-key check to fail, got {err:?}"
    );
}

#[test]
fn container_key_one_past_the_limit_is_a_distinct_nesting_error() {
    let text = nested_sequence_key(electricity_yaml::MAX_DEPTH);
    let err = run_on_small_stack(text).unwrap_err();
    assert!(matches!(err, YamlError::NestingTooDeep { .. }));
}
