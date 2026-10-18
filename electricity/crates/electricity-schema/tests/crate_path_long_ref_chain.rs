//! Issue #507's own item 0: `engine::schema_is_fully_supported`'s own
//! `dict.iter().all(...)` walk short-circuits at the *first* unsupported
//! keyword it sees -- a root-level `unevaluatedProperties`, or a nested
//! (non-root) `$id` -- so it never measures a long `$defs` chain
//! reachable only through a *later* sibling key, and
//! `recursion_exceeded_peek` reads `false`. The schema used to fall
//! straight through to the `jsonschema` crate's own eager `$ref`
//! resolution, which needs far more native stack per hop than this
//! engine's own bounded walk ever had to -- fine up to about 10,000
//! hops, an uncontrolled process abort (`rc -6`, no state written at
//! all) somewhere past that, well before 30,000. `lib.rs`'s own
//! crate-path guard (`engine::ref_chain_depth_exceeds_cap`) measures the
//! chain itself, structurally, before the crate is ever built, and
//! reports the same `RecursionError` text the engine path already does
//! for an equally long chain.
//!
//! Every schema here is parsed through [`electricity_json::loads`], not
//! `serde_json::json!` -- this crate's own `serde_json::Value::Object`
//! is a `BTreeMap` (alphabetical, `Cargo.toml`'s own dev-dependency
//! comment), which would silently move `"$ref"`/`"$defs"` ahead of the
//! trigger keyword (every ASCII letter sorts after `$`) and never
//! reproduce the short-circuit this regression needs: the whole point
//! is that the *unsupported* keyword comes first in the schema's own
//! key order, exactly as a document author would naturally write it
//! (`unevaluatedProperties`/`allOf` up top, `$defs` at the bottom).
//!
//! Every trial runs on its own 2 MiB caller thread, the same convention
//! `ref_chain_stack_safety.rs` uses, so a regression that only shows up
//! on a small caller stack doesn't pass by accident.

use electricity_schema::{PromptSchemaCheck, validate_prompt_schema};
use electricity_value::Value;

const RECURSION_ERROR_TEXT: &str = "maximum recursion depth exceeded while calling a Python object";

fn defs_chain_json(n: usize) -> String {
    let mut out = String::from("{");
    for i in 0..n {
        out.push_str(&format!("\"L{i}\": {{\"$ref\": \"#/$defs/L{}\"}}, ", i + 1));
    }
    out.push_str(&format!("\"L{n}\": {{\"type\": \"string\"}}}}"));
    out
}

/// `unevaluatedProperties` sits directly at the schema root, written
/// *before* (never inside) the `$ref`/`$defs` chain -- the natural
/// authoring order, and the one that defeats `dict.iter().all(...)`'s
/// own short-circuit.
fn root_unevaluated_properties_with_long_ref_chain(n: usize) -> (Value, Value) {
    let schema = format!(
        "{{\"unevaluatedProperties\": false, \"$ref\": \"#/$defs/L0\", \"$defs\": {}}}",
        defs_chain_json(n)
    );
    (
        electricity_json::loads(&schema).expect("valid JSON"),
        electricity_json::loads("\"x\"").expect("valid JSON"),
    )
}

/// A nested, non-root `$id` (inside `allOf`'s own first branch) sits
/// before the `$ref`/`$defs` keys the same way -- `schema_is_fully_
/// supported_inner`'s own `"$id" => is_root` arm is `false` there,
/// short-circuiting `allOf`'s own `.all()` (and so the outer one)
/// before `"$ref"`/`"$defs"` are ever reached.
fn root_nested_id_with_long_ref_chain(n: usize) -> (Value, Value) {
    let schema = format!(
        "{{\"allOf\": [{{\"$id\": \"https://example.com/sub\"}}], \"$ref\": \"#/$defs/L0\", \"$defs\": {}}}",
        defs_chain_json(n)
    );
    (
        electricity_json::loads(&schema).expect("valid JSON"),
        electricity_json::loads("\"x\"").expect("valid JSON"),
    )
}

fn check_on_a_2mib_caller_stack(schema: Value, instance: Value) -> PromptSchemaCheck {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || validate_prompt_schema(&schema, &instance))
        .expect("spawn a 2 MiB-stack thread")
        .join()
        .expect("the spawned thread must not panic")
}

#[test]
fn root_unevaluated_properties_1000_hops_is_accepted() {
    let (schema, instance) = root_unevaluated_properties_with_long_ref_chain(1000);
    assert_eq!(
        check_on_a_2mib_caller_stack(schema, instance),
        PromptSchemaCheck::Valid
    );
}

#[test]
fn root_unevaluated_properties_10000_hops_reports_the_recursion_cap_not_a_crash() {
    let (schema, instance) = root_unevaluated_properties_with_long_ref_chain(10_000);
    assert_eq!(
        check_on_a_2mib_caller_stack(schema, instance),
        PromptSchemaCheck::SchemaError(RECURSION_ERROR_TEXT.to_string())
    );
}

#[test]
fn root_unevaluated_properties_30000_hops_reports_the_recursion_cap_not_a_crash() {
    let (schema, instance) = root_unevaluated_properties_with_long_ref_chain(30_000);
    assert_eq!(
        check_on_a_2mib_caller_stack(schema, instance),
        PromptSchemaCheck::SchemaError(RECURSION_ERROR_TEXT.to_string())
    );
}

#[test]
fn root_unevaluated_properties_100000_hops_reports_the_recursion_cap_not_a_crash() {
    let (schema, instance) = root_unevaluated_properties_with_long_ref_chain(100_000);
    assert_eq!(
        check_on_a_2mib_caller_stack(schema, instance),
        PromptSchemaCheck::SchemaError(RECURSION_ERROR_TEXT.to_string())
    );
}

#[test]
fn root_nested_id_1000_hops_is_accepted() {
    let (schema, instance) = root_nested_id_with_long_ref_chain(1000);
    assert_eq!(
        check_on_a_2mib_caller_stack(schema, instance),
        PromptSchemaCheck::Valid
    );
}

#[test]
fn root_nested_id_10000_hops_reports_the_recursion_cap_not_a_crash() {
    let (schema, instance) = root_nested_id_with_long_ref_chain(10_000);
    assert_eq!(
        check_on_a_2mib_caller_stack(schema, instance),
        PromptSchemaCheck::SchemaError(RECURSION_ERROR_TEXT.to_string())
    );
}

#[test]
fn root_nested_id_30000_hops_reports_the_recursion_cap_not_a_crash() {
    let (schema, instance) = root_nested_id_with_long_ref_chain(30_000);
    assert_eq!(
        check_on_a_2mib_caller_stack(schema, instance),
        PromptSchemaCheck::SchemaError(RECURSION_ERROR_TEXT.to_string())
    );
}

#[test]
fn root_nested_id_100000_hops_reports_the_recursion_cap_not_a_crash() {
    let (schema, instance) = root_nested_id_with_long_ref_chain(100_000);
    assert_eq!(
        check_on_a_2mib_caller_stack(schema, instance),
        PromptSchemaCheck::SchemaError(RECURSION_ERROR_TEXT.to_string())
    );
}
