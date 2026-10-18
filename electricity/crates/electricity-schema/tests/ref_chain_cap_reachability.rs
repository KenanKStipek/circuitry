//! `engine::ref_chain_depth_exceeds_cap`'s own walk, measuring a
//! schema's longest `$ref` chain before handing it to the `jsonschema`
//! crate, used to conflate ordinary structural nesting with `$ref`
//! hops (counting every edge, of either kind, against the same cap a
//! pure hop chain uses) and to measure chains no real validator ever
//! resolves (an unreferenced `$defs` entry, or a `"$ref"`-looking key
//! sitting inside literal `const`/`enum`/`examples`/`default` data).
//! Both are fixed here: only an actual `$ref` hop counts toward
//! [`MAX_RECURSION_DEPTH`] (structural nesting gets its own, much
//! larger budget), and a chain is only ever found by actually
//! resolving a `$ref`, never by blindly walking `$defs`/`definitions`
//! or a literal-data keyword's own value.

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

/// `unevaluatedProperties` at the root routes this schema to the crate
/// path (the only path [`engine::ref_chain_depth_exceeds_cap`] ever
/// runs on) -- the same convention `crate_path_long_ref_chain.rs`
/// uses. The chain itself has `n + 1` total `$ref` hops: the schema's
/// own top-level `$ref` into `$defs.L0`, plus `n` more hops from
/// [`defs_chain_json`]'s own `L0 -> L1 -> ... -> Ln` -- `n = 4094`
/// is exactly [`engine::MAX_RECURSION_DEPTH`]'s own last accepted
/// total (4095 hops), `n = 4095` its first rejected one (4096).
fn crate_path_chain(n: usize) -> (Value, Value) {
    let schema = format!(
        "{{\"unevaluatedProperties\": false, \"$ref\": \"#/$defs/L0\", \"$defs\": {}}}",
        defs_chain_json(n)
    );
    (
        electricity_json::loads(&schema).expect("valid JSON"),
        electricity_json::loads("\"x\"").expect("valid JSON"),
    )
}

#[test]
fn a_4095_total_hop_chain_is_accepted_on_the_crate_path() {
    let (schema, instance) = crate_path_chain(4094);
    assert_eq!(
        validate_prompt_schema(&schema, &instance),
        PromptSchemaCheck::Valid
    );
}

#[test]
fn a_4096_total_hop_chain_reports_the_recursion_cap_on_the_crate_path() {
    let (schema, instance) = crate_path_chain(4095);
    assert_eq!(
        validate_prompt_schema(&schema, &instance),
        PromptSchemaCheck::SchemaError(RECURSION_ERROR_TEXT.to_string())
    );
}

/// The same total hop count as the accepted case above (4095), but
/// every hop now sits three structural levels deep
/// (`properties.x.$ref`) rather than at the top of each `$defs`
/// entry -- active-path-length-based accounting used to count
/// structural nesting against the same cap a `$ref` hop uses, tripping
/// at around 1,365 hops here (`4096 / 3`) long before the real hop
/// count ever did.
#[test]
fn a_ref_chain_nested_under_properties_x_is_not_capped_early() {
    let n = 4094usize;
    let mut schema = String::from("{\"unevaluatedProperties\": false, \"$ref\": \"#/L0\", ");
    for i in 0..n {
        schema.push_str(&format!(
            "\"L{i}\": {{\"properties\": {{\"x\": {{\"$ref\": \"#/L{}\"}}}}}}, ",
            i + 1
        ));
    }
    schema.push_str(&format!("\"L{n}\": {{\"type\": \"string\"}}}}"));
    let schema: Value = electricity_json::loads(&schema).expect("valid JSON");
    let instance = electricity_json::loads("\"x\"").expect("valid JSON");
    assert_eq!(
        validate_prompt_schema(&schema, &instance),
        PromptSchemaCheck::Valid
    );
}

/// A 100,000-hop `$defs` chain nothing ever references (no `$ref`
/// anywhere in the schema points into it) must never be measured at
/// all -- it is dead weight no real validator, Python's, the crate's,
/// or this engine's own, ever resolves, so this schema stays `Valid`
/// rather than wrongly reporting the recursion cap.
#[test]
fn an_unreferenced_defs_chain_is_never_measured() {
    let schema = format!(
        "{{\"unevaluatedProperties\": false, \"$defs\": {}}}",
        defs_chain_json(100_000)
    );
    let schema: Value = electricity_json::loads(&schema).expect("valid JSON");
    let instance = electricity_json::loads("{}").expect("valid JSON");
    assert_eq!(
        validate_prompt_schema(&schema, &instance),
        PromptSchemaCheck::Valid
    );
}

/// A `"$ref"` key sitting inside `examples` (instance-shaped example
/// data, never schema content) that happens to point at a real,
/// 100,000-hop `$defs` chain must never be followed as an actual hop
/// -- Python, and the crate, both treat `examples` as opaque literal
/// data, never a place a `$ref` keyword can live.
#[test]
fn a_ref_looking_key_inside_examples_is_never_followed() {
    let schema = format!(
        "{{\"unevaluatedProperties\": false, \"examples\": [{{\"$ref\": \"#/$defs/L0\"}}], \"$defs\": {}}}",
        defs_chain_json(100_000)
    );
    let schema: Value = electricity_json::loads(&schema).expect("valid JSON");
    let instance = electricity_json::loads("{}").expect("valid JSON");
    assert_eq!(
        validate_prompt_schema(&schema, &instance),
        PromptSchemaCheck::Valid
    );
}
