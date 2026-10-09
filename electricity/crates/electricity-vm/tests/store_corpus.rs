//! Replays `electricity/scripts/generate_store_corpus.py`'s own op
//! sequences against this crate's real `Store`, and checks the
//! materialized result matches Circuitry's own `core.store.store.Store`
//! byte-for-byte -- including key *order*, since `Value`'s own `PartialEq`
//! (Python dict `==` semantics) doesn't check it (`electricity-value`'s
//! crate docs), and a `meta` key's order is part of `--out`'s own contract
//! (issue #431's Lane B section).

use electricity_value::Value;
use electricity_vm::Store;
use std::path::Path;

fn corpus() -> Vec<serde_json::Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/store_corpus.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    serde_json::from_str(&text).expect("store_corpus.json is valid JSON")
}

/// `serde_json::Value` -> `electricity_value::Value`, restricted to the
/// plain JSON-safe shapes this corpus ever contains (no bytes/date/big-int
/// tagging -- that's `generate_value_corpus.py`'s own job).
fn from_json(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::None,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else {
                Value::from(n.as_f64().expect("corpus numbers fit in i64 or f64"))
            }
        }
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Array(items) => Value::List(items.iter().map(from_json).collect()),
        serde_json::Value::Object(entries) => {
            let mut dict = indexmap::IndexMap::with_capacity(entries.len());
            for (key, value) in entries {
                dict.insert(Value::Str(key.clone()), from_json(value));
            }
            Value::Dict(dict)
        }
    }
}

/// Applies one `{"op": "set"|"ensure_dict", "path": ..., "value"?: ...}`
/// corpus op to *parent* -- `set`'s own `path.split(".")`-then-
/// `ensure_dict`-each-segment walk mirrors `core/store/store.py::Store.set`
/// exactly (its own last segment is a plain leaf write, not a further
/// `ensure_dict`).
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
            let value = from_json(&op["value"]);
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
        let expected = from_json(&case["expected_state"]);
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
                for (key, value) in case["initial"]
                    .as_object()
                    .expect("case.initial is an object")
                {
                    store.set_leaf(&store.root, Value::Str(key.clone()), from_json(value));
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
