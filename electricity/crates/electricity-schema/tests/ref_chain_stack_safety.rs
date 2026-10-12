//! Round-7 regression: a single-hop-per-level `$defs` chain of `$ref`s
//! (`L0 -> L1 -> ... -> LN`) used to overflow the native stack --
//! `thread 'main' has overflowed its stack`, an immediate process abort
//! with no state written at all -- well under `engine::MAX_RECURSION_
//! DEPTH` (4096), in both debug and release builds (`electricity-schema`
//! builds at `opt-level = 2` even in dev -- workspace `Cargo.toml`'s own
//! override -- specifically so this margin holds in both). `validate_
//! prompt_schema`'s own dedicated, large-stack validation thread
//! (`lib.rs`) is the fix.
//!
//! Every trial here additionally runs on its own 2 MiB caller thread --
//! the convention this workspace's other depth tests use (e.g.
//! `electricity-json/tests/depth_limit.rs`) -- so a regression that
//! only shows up when the *caller's* own stack is small (rather than
//! `cargo test`'s own, generally larger, default worker-thread stack)
//! would still fail loudly here, not just abort the whole test process
//! silently on some platforms and pass by accident on others.
//!
//! A pure 1000-hop chain is Python's own accepted case (`cof` itself
//! accepts it; Python's `RecursionError` starts somewhere between 1000
//! and 2000 hops -- a documented threshold difference, `DESIGN.md`
//! §13) and must stay `Valid` here too. 3000/4096/4097/10000 must never
//! crash regardless of verdict.

use electricity_schema::{PromptSchemaCheck, validate_prompt_schema, value_from_json};
use serde_json::json;

const RECURSION_ERROR_TEXT: &str = "maximum recursion depth exceeded while calling a Python object";

fn ref_chain(n: usize) -> (serde_json::Value, serde_json::Value) {
    let mut defs = serde_json::Map::new();
    for i in 0..n {
        defs.insert(
            format!("L{i}"),
            json!({"$ref": format!("#/$defs/L{}", i + 1)}),
        );
    }
    defs.insert(format!("L{n}"), json!({"type": "object"}));
    (json!({"$defs": defs, "$ref": "#/$defs/L0"}), json!({}))
}

/// `Hop1 -> Hop2 -> Hop3 -> Node`, cyclic back to `Node` via a sibling
/// `properties.child`, so every level of *instance* nesting costs four
/// `evaluate` frames, not one -- distinct from [`ref_chain`]'s own
/// one-hop-per-level shape.
fn combined_deep_shape(levels: usize) -> (serde_json::Value, serde_json::Value) {
    let defs = json!({
        "Node": {"type": "object", "properties": {"child": {"$ref": "#/$defs/Hop1"}}},
        "Hop1": {"$ref": "#/$defs/Hop2"},
        "Hop2": {"$ref": "#/$defs/Hop3"},
        "Hop3": {"$ref": "#/$defs/Node"},
    });
    let schema = json!({"$defs": defs, "$ref": "#/$defs/Node"});
    let mut instance = json!({});
    for _ in 0..levels {
        instance = json!({"child": instance});
    }
    (schema, instance)
}

/// Same chain shape as [`ref_chain`], but the terminal node uses
/// `unevaluatedProperties` -- a keyword `engine::schema_is_fully_
/// supported` never implements, so the gate routes the whole document
/// to the `jsonschema` crate instead, which resolves every `$ref` in
/// the chain eagerly at its own schema-build step: the crate path this
/// stack fix has to cover too, not just `engine::evaluate`'s own.
fn ref_chain_ending_in_crate_only_construct(n: usize) -> (serde_json::Value, serde_json::Value) {
    let mut defs = serde_json::Map::new();
    for i in 0..n {
        defs.insert(
            format!("L{i}"),
            json!({"$ref": format!("#/$defs/L{}", i + 1)}),
        );
    }
    defs.insert(
        format!("L{n}"),
        json!({"type": "object", "unevaluatedProperties": false}),
    );
    (
        json!({"$defs": defs, "$ref": "#/$defs/L0"}),
        json!({"extra": 1}),
    )
}

fn check_on_a_2mib_caller_stack(
    schema: serde_json::Value,
    instance: serde_json::Value,
) -> PromptSchemaCheck {
    let schema = value_from_json(&schema);
    let instance = value_from_json(&instance);
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || validate_prompt_schema(&schema, &instance))
        .expect("spawn a 2 MiB-stack thread")
        .join()
        .expect("the spawned thread must not panic")
}

#[test]
fn a_1000_hop_ref_chain_is_accepted_on_a_2mib_caller_stack() {
    let (schema, instance) = ref_chain(1000);
    assert_eq!(
        check_on_a_2mib_caller_stack(schema, instance),
        PromptSchemaCheck::Valid
    );
}

#[test]
fn a_3000_hop_ref_chain_never_overflows_on_a_2mib_caller_stack() {
    let (schema, instance) = ref_chain(3000);
    match check_on_a_2mib_caller_stack(schema, instance) {
        PromptSchemaCheck::Valid | PromptSchemaCheck::SchemaError(_) => {}
        other => panic!("expected Valid or a SchemaError, got {other:?}"),
    }
}

#[test]
fn a_4096_hop_ref_chain_reports_the_recursion_cap_not_a_crash() {
    let (schema, instance) = ref_chain(4096);
    assert_eq!(
        check_on_a_2mib_caller_stack(schema, instance),
        PromptSchemaCheck::SchemaError(RECURSION_ERROR_TEXT.to_string())
    );
}

#[test]
fn a_4097_hop_ref_chain_reports_the_recursion_cap_not_a_crash() {
    let (schema, instance) = ref_chain(4097);
    assert_eq!(
        check_on_a_2mib_caller_stack(schema, instance),
        PromptSchemaCheck::SchemaError(RECURSION_ERROR_TEXT.to_string())
    );
}

#[test]
fn a_10000_hop_ref_chain_reports_the_recursion_cap_not_a_crash() {
    let (schema, instance) = ref_chain(10000);
    assert_eq!(
        check_on_a_2mib_caller_stack(schema, instance),
        PromptSchemaCheck::SchemaError(RECURSION_ERROR_TEXT.to_string())
    );
}

#[test]
fn a_combined_deep_shape_with_several_ref_hops_per_level_is_accepted() {
    // 700, not a round 500: measured directly against afaceeb, this
    // shape's own per-level cost on a 2 MiB caller stack already
    // overflows somewhere between 600 and 650 levels -- 500 wouldn't
    // have made this a convincing regression check. The upper bound is
    // `engine::MAX_RECURSION_DEPTH` (4096) itself: this shape's own
    // frames-per-level cost caps out around 800 levels (measured
    // directly against the fix), past which the cap reports cleanly
    // rather than `Valid`.
    let (schema, instance) = combined_deep_shape(700);
    assert_eq!(
        check_on_a_2mib_caller_stack(schema, instance),
        PromptSchemaCheck::Valid
    );
}

#[test]
fn a_ref_chain_ending_in_a_crate_only_construct_rejects_cleanly() {
    let (schema, instance) = ref_chain_ending_in_crate_only_construct(1000);
    match check_on_a_2mib_caller_stack(schema, instance) {
        PromptSchemaCheck::Invalid(_) => {}
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn a_ref_chain_ending_in_a_crate_only_construct_accepts_a_clean_instance() {
    let (schema, _) = ref_chain_ending_in_crate_only_construct(1000);
    assert_eq!(
        check_on_a_2mib_caller_stack(schema, json!({})),
        PromptSchemaCheck::Valid
    );
}
