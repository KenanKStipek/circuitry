//! A schema that is itself recursive (a `$ref` cycle back to an
//! ancestor) describes a real, finite-instance-terminating shape -- a
//! tree or a linked list -- that Python validates up to roughly 200-240
//! levels before its own `RecursionError` (PR #502's round-4 fix,
//! finding F1). Before that fix, `schema_is_fully_supported` had no
//! visited set and walked a cyclic schema's own structure forever
//! (bounded only by a plain depth budget), so *every* instance of *any*
//! recursive schema -- even a depth-1 one -- was wrongly reported as
//! Python's own uncaught `RecursionError` before `evaluate` ever ran.
//!
//! These exercise both shapes `probe-depth`/`probe-schema6` record
//! against real `cof run`: a tree (`$defs` + a named `$ref`) and a
//! linked list (`$ref: "#"`, the root referring to itself).

use electricity_schema::{PromptSchemaCheck, validate_prompt_schema, value_from_json};
use serde_json::json;

fn tree_ref_schema() -> serde_json::Value {
    json!({
        "$defs": {
            "node": {
                "type": "object",
                "properties": {
                    "v": {"type": "integer"},
                    "c": {"$ref": "#/$defs/node"},
                },
            },
        },
        "$ref": "#/$defs/node",
    })
}

fn self_ref_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "v": {"type": "integer"},
            "c": {"$ref": "#"},
        },
    })
}

/// A chain *depth* levels deep, each node `{"v": <leaf_v on the last
/// level, 1 otherwise>, "c": <next>}`, the last level carrying no `"c"`
/// at all (matching `probe-depth`'s own fixtures exactly).
fn chain(depth: usize, leaf_v: serde_json::Value) -> serde_json::Value {
    assert!(depth >= 1);
    let mut node = json!({"v": leaf_v});
    for _ in 0..depth - 1 {
        node = json!({"v": 1, "c": node});
    }
    node
}

fn check(schema: &serde_json::Value, instance: &serde_json::Value) -> PromptSchemaCheck {
    validate_prompt_schema(&value_from_json(schema), &value_from_json(instance))
}

#[test]
fn tree_shaped_recursive_schema_accepts_depth_1() {
    let schema = tree_ref_schema();
    let instance = chain(1, json!(1));
    assert_eq!(check(&schema, &instance), PromptSchemaCheck::Valid);
}

#[test]
fn tree_shaped_recursive_schema_accepts_depth_25() {
    let schema = tree_ref_schema();
    let instance = chain(25, json!(1));
    assert_eq!(check(&schema, &instance), PromptSchemaCheck::Valid);
}

#[test]
fn tree_shaped_recursive_schema_accepts_depth_200() {
    let schema = tree_ref_schema();
    let instance = chain(200, json!(1));
    assert_eq!(check(&schema, &instance), PromptSchemaCheck::Valid);
}

#[test]
fn tree_shaped_recursive_schema_reports_an_invalid_leaf_200_levels_down() {
    let schema = tree_ref_schema();
    let instance = chain(200, json!("bad"));
    assert_eq!(
        check(&schema, &instance),
        PromptSchemaCheck::Invalid("'bad' is not of type 'integer'".to_string())
    );
}

#[test]
fn linked_list_shaped_recursive_schema_accepts_depth_1() {
    let schema = self_ref_schema();
    let instance = chain(1, json!(1));
    assert_eq!(check(&schema, &instance), PromptSchemaCheck::Valid);
}

#[test]
fn linked_list_shaped_recursive_schema_accepts_depth_25() {
    let schema = self_ref_schema();
    let instance = chain(25, json!(1));
    assert_eq!(check(&schema, &instance), PromptSchemaCheck::Valid);
}

#[test]
fn linked_list_shaped_recursive_schema_accepts_depth_200() {
    let schema = self_ref_schema();
    let instance = chain(200, json!(1));
    assert_eq!(check(&schema, &instance), PromptSchemaCheck::Valid);
}

#[test]
fn linked_list_shaped_recursive_schema_reports_an_invalid_leaf_200_levels_down() {
    let schema = self_ref_schema();
    let instance = chain(200, json!("bad"));
    assert_eq!(
        check(&schema, &instance),
        PromptSchemaCheck::Invalid("'bad' is not of type 'integer'".to_string())
    );
}

/// A pure `$ref` cycle (the schema resolving to literally itself, with
/// no instance progress at all) is the one case this engine must still
/// report as Python's own uncaught `RecursionError` would -- a generic,
/// non-retryable `SchemaError`, never a crash and never `Valid`/
/// `Invalid`. `probe-selfref`'s own fixture.
#[test]
fn a_pure_ref_cycle_reports_the_recursion_error_text_not_a_crash() {
    let schema = json!({"$ref": "#"});
    let instance = json!({"anything": "at all"});
    assert_eq!(
        check(&schema, &instance),
        PromptSchemaCheck::SchemaError(
            "maximum recursion depth exceeded while calling a Python object".to_string()
        )
    );
}

/// Pins the exact native-thread-stack safety margin this engine's own
/// cycle detection relies on (`MAX_RECURSION_DEPTH`'s own module docs,
/// `electricity-schema/src/engine.rs`): a decoder-max-depth instance
/// (`electricity_value::MAX_DEPTH`, the JSON decoder's own ceiling -- no
/// real document's instance can ever nest deeper than this) against the
/// tree-shaped recursive schema, run on an unoptimized debug build's
/// *default* main-thread stack (no `std::thread::Builder::stack_size`
/// override at all, unlike every other depth-safety test in this
/// workspace -- a real `cof run` validates a prompt's schema on
/// electricity's own main thread, never a thread this test's own 2MiB
/// convention would reproduce).
#[test]
fn decoder_max_depth_instance_does_not_overflow_the_main_threads_own_stack() {
    let schema = tree_ref_schema();
    let instance = chain(electricity_value::MAX_DEPTH, json!(1));
    assert_eq!(check(&schema, &instance), PromptSchemaCheck::Valid);
}
