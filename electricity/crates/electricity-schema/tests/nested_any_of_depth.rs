//! `anyOf` validated through a recursive `$ref` (a tree/list-shaped
//! schema's own natural encoding of "either a leaf or a nested
//! container") used to double this engine's own work at every level a
//! branch *failed* -- `schema_accepts` (a bare validity check) was
//! always immediately followed by a second, separate call re-evaluating
//! the identical branch/instance pair just to collect its violations --
//! recursively exponential in nesting depth once a document fails deep
//! enough (PR #502's round-4 fix; probe-schema7 c18/c19,
//! 12/16 levels). This pins a much deeper, still-failing instance (30
//! levels) staying well under a second in an unoptimized debug build.

use electricity_schema::{PromptSchemaCheck, validate_prompt_schema, value_from_json};
use serde_json::json;
use std::time::{Duration, Instant};

#[test]
fn deeply_nested_any_of_through_a_recursive_ref_stays_linear() {
    let schema = json!({
        "$defs": {
            "n": {
                "anyOf": [
                    {"type": "string"},
                    {"type": "array", "items": {"$ref": "#/$defs/n"}},
                ],
            },
        },
        "$ref": "#/$defs/n",
    });
    let schema = value_from_json(&schema);

    let depth = 30;
    let mut instance = json!(1);
    for _ in 0..depth {
        instance = json!([instance]);
    }
    let instance = value_from_json(&instance);

    let start = Instant::now();
    let result = validate_prompt_schema(&schema, &instance);
    let elapsed = start.elapsed();

    assert!(
        matches!(result, PromptSchemaCheck::Invalid(_)),
        "a bare int at the bottom of {depth} nested arrays matches neither anyOf branch: {result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "{depth} nested arrays through a recursive anyOf took {elapsed:?}"
    );
}
