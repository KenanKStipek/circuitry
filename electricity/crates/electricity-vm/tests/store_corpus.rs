//! Replays `electricity/scripts/generate_store_corpus.py`'s own op
//! sequences against this crate's real `Store`, and checks the
//! materialized result matches Circuitry's own `core.store.store.Store`
//! byte-for-byte -- including key *order*, since `Value`'s own `PartialEq`
//! (Python dict `==` semantics) doesn't check it (`electricity-value`'s
//! crate docs), and a `meta` key's order is part of `--out`'s own contract
//! (issue #431's Lane B section).
//!
//! The corpus itself uses the tagged `{"t": ..., "v": ...}` encoding
//! `generate_value_corpus.py`'s own `encode` uses (`decode` below is its
//! Rust-side counterpart), not a plain JSON object -- a plain JSON object
//! would round-trip through whatever `serde_json::Map` representation
//! this crate's own `[dev-dependencies]` happen to be built with, and
//! turning that into an order-preserving one here (a `preserve_order`
//! feature on `serde_json`) would flip that feature on for *every* crate
//! in the workspace that also depends on `serde_json` (Cargo's own
//! feature unification across one `cargo test --workspace` invocation),
//! changing unrelated code's compiled shape along with it.

use electricity_value::Value;
use electricity_vm::Store;
use std::path::Path;

fn corpus() -> Vec<serde_json::Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/store_corpus.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    serde_json::from_str(&text).expect("store_corpus.json is valid JSON")
}

/// The tagged scheme's own decoder, restricted to the shapes this corpus
/// ever contains (no bytes/float/date -- `generate_store_corpus.py`'s own
/// `encode`).
fn decode(tagged: &serde_json::Value) -> Value {
    let t = tagged["t"].as_str().expect("tagged value has a t");
    match t {
        "none" => Value::None,
        "bool" => Value::Bool(tagged["v"].as_bool().expect("bool v")),
        "int" => Value::from(
            tagged["v"]
                .as_str()
                .expect("int v is a decimal string")
                .parse::<i64>()
                .expect("corpus ints fit in i64"),
        ),
        "str" => Value::Str(tagged["v"].as_str().expect("str v").to_string()),
        "list" => Value::List(
            tagged["v"]
                .as_array()
                .expect("list v is an array")
                .iter()
                .map(decode)
                .collect(),
        ),
        "dict" => {
            let mut dict = indexmap::IndexMap::new();
            for pair in tagged["v"].as_array().expect("dict v is an array") {
                let pair = pair.as_array().expect("dict entry is a [k, v] pair");
                dict.insert(decode(&pair[0]), decode(&pair[1]));
            }
            Value::Dict(dict)
        }
        other => panic!("unknown tagged type {other:?}"),
    }
}

/// Applies one `{"op": "set"|"ensure_dict", "path": ..., "value"?: ...}`
/// corpus op (`value` still tagged) to *parent* -- `set`'s own
/// `path.split(".")`-then-`ensure_dict`-each-segment walk mirrors
/// `core/store/store.py::Store.set` exactly (its own last segment is a
/// plain leaf write, not a further `ensure_dict`).
fn apply_op(store: &Store, parent: &electricity_vm::NodeRef, op: &serde_json::Value) {
    let path = op["path"].as_str().expect("op.path is a string");
    let segments: Vec<&str> = path.split('.').collect();
    match op["op"].as_str().expect("op.op is a string") {
        "set" => {
            let (last, ancestors) = segments.split_last().expect("path is never empty");
            let mut node = parent.clone();
            for segment in ancestors {
                node = store
                    .ensure_dict(&node, Value::Str(segment.to_string()))
                    .unwrap();
            }
            let value = decode(&op["value"]);
            store.set_leaf(&node, Value::Str(last.to_string()), value);
        }
        "ensure_dict" => {
            let mut node = parent.clone();
            for segment in segments {
                node = store
                    .ensure_dict(&node, Value::Str(segment.to_string()))
                    .unwrap();
            }
        }
        other => panic!("unknown corpus op {other:?}"),
    }
}

/// Recursively asserts *actual* equals *expected*, including dict key
/// *order* -- see this module's own doc comment for why a plain `==`
/// isn't enough.
fn assert_matches_in_order(actual: &Value, expected: &Value, path: &str) {
    match (actual, expected) {
        (Value::Dict(a), Value::Dict(e)) => {
            let a_keys: Vec<&Value> = a.keys().collect();
            let e_keys: Vec<&Value> = e.keys().collect();
            assert_eq!(a_keys, e_keys, "key order mismatch at {path:?}");
            for (key, expected_value) in e {
                let actual_value = a.get(key).unwrap_or_else(|| {
                    panic!("missing key {key:?} at {path:?}");
                });
                assert_matches_in_order(
                    actual_value,
                    expected_value,
                    &format!("{path}.{}", key.py_str()),
                );
            }
        }
        (Value::List(a), Value::List(e)) => {
            assert_eq!(a.len(), e.len(), "list length mismatch at {path:?}");
            for (index, (av, ev)) in a.iter().zip(e).enumerate() {
                assert_matches_in_order(av, ev, &format!("{path}[{index}]"));
            }
        }
        _ => assert!(
            actual.py_eq(expected),
            "value mismatch at {path:?}: {actual:?} != {expected:?}"
        ),
    }
}

#[test]
fn store_corpus_matches_circuitrys_own_store() {
    let cases = corpus();
    assert!(!cases.is_empty(), "store_corpus.json has no cases");
    for case in &cases {
        let name = case["name"].as_str().expect("case.name is a string");
        let expected = decode(&case["expected_state"]);
        let actual = match case["kind"].as_str().expect("case.kind is a string") {
            "sequence" => {
                let store = Store::new();
                for op in case["ops"].as_array().expect("case.ops is an array") {
                    apply_op(&store, &store.root, op);
                }
                store.materialize(&store.root)
            }
            "merge" => {
                let store = Store::new();
                let mut decoded_initial = decode(&case["initial"]);
                let Value::Dict(ref mut initial) = decoded_initial else {
                    panic!("case.initial decodes to a dict");
                };
                for (key, value) in std::mem::take(initial) {
                    store.set_leaf(&store.root, key, value);
                }
                let branch_ops = case["branch_ops"]
                    .as_array()
                    .expect("case.branch_ops is an array");
                let branches = store
                    .parallel_branches(&store.root, branch_ops.len())
                    .unwrap();
                for (branch, ops) in branches.iter().zip(branch_ops) {
                    for op in ops.as_array().expect("branch_ops[i] is an array") {
                        apply_op(&store, branch, op);
                    }
                }
                store.merge(&store.root, branches).unwrap();
                store.materialize(&store.root)
            }
            other => panic!("case {name:?} has an unknown kind {other:?}"),
        };
        assert_matches_in_order(&actual, &expected, name);
    }
}
