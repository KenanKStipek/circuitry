//! `schema::walk_schema` recurses once per nesting level, the same
//! shape as `electricity-yaml`'s composer and `electricity-schema`'s
//! own recursive walks -- confirmed the same way those crates do
//! (`electricity-schema/tests/ref_chain_stack_safety.rs`): a 2 MiB
//! thread stack, deliberately smaller than any real caller's, must
//! still survive the deepest schema the loaders this workspace feeds
//! `score()` from would ever produce (`electricity-value::MAX_DEPTH`,
//! 512).

use electricity_complexity::{ScoreInput, score};
use electricity_value::{Dict, Value};

fn deep_schema(depth: usize) -> Value {
    let mut node = Value::Dict(Dict::new());
    for _ in 0..depth {
        let mut properties = Dict::new();
        properties.insert(Value::Str("a".to_string()), node);
        let mut wrapper = Dict::new();
        wrapper.insert(
            Value::Str("properties".to_string()),
            Value::Dict(properties),
        );
        node = Value::Dict(wrapper);
    }
    node
}

#[test]
fn a_deep_schema_does_not_overflow_a_2mib_stack() {
    let result = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let schema = deep_schema(500);
            let mut definition = Dict::new();
            definition.insert(Value::Str("template".to_string()), Value::from("x"));
            definition.insert(Value::Str("schema".to_string()), schema);
            let definition = Value::Dict(definition);
            let input = ScoreInput {
                definition: Some(&definition),
                ..Default::default()
            };
            score(&input).expect("a deep but within-MAX_DEPTH schema does not raise")
        })
        .expect("spawn a 2 MiB-stack thread")
        .join()
        .expect("the spawned thread must not panic/overflow its stack");

    let signal = result
        .signal("output_schema")
        .expect("output_schema signal present");
    assert_eq!(signal.raw, 500.0); // one `properties` field per nesting level.
}
