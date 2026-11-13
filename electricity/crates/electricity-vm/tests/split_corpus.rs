//! Replays `electricity/scripts/generate_split_corpus.py`'s own cases
//! against `electricity_vm::split`'s real `split_list`/`split_mapping`/
//! `split_text`, checking they agree with `core/split.py` byte for byte
//! -- both the resulting parts (in order) and, when a unit doesn't fit,
//! the exact `NoSplitPoint` text (issue #682).

use electricity_value::{Dict, Value};
use electricity_vm::split::{split_list, split_mapping, split_text};
use std::path::Path;

fn corpus() -> Vec<serde_json::Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/split_corpus.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    serde_json::from_str(&text).expect("split_corpus.json is valid JSON")
}

/// The tagged scheme's own decoder (`generate_value_corpus.py`'s own
/// `encode`), restricted to the shapes this corpus contains.
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
        "float" => {
            let bits = u64::from_str_radix(tagged["bits"].as_str().expect("float bits"), 16)
                .expect("float bits is hex");
            Value::Float(f64::from_bits(bits))
        }
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
            let mut dict = Dict::new();
            for pair in tagged["v"].as_array().expect("dict v is an array") {
                let pair = pair.as_array().expect("dict entry is a [k, v] pair");
                dict.insert(decode(&pair[0]), decode(&pair[1]));
            }
            Value::Dict(dict)
        }
        other => panic!("unknown tagged type {other:?}"),
    }
}

fn decode_parts(tagged: &serde_json::Value) -> Vec<Value> {
    tagged
        .as_array()
        .expect("parts is an array")
        .iter()
        .map(decode)
        .collect()
}

#[test]
fn split_matches_core_split_py() {
    for case in corpus() {
        let name = case["name"].as_str().expect("case.name");
        let room = case["room"].as_i64().expect("case.room");
        let expected_error = case["error"].as_str();
        let expected_parts = case["parts"]
            .as_array()
            .map(|_| decode_parts(&case["parts"]));

        match case["kind"].as_str().expect("case.kind") {
            "list" => {
                let decoded = decode(&case["items"]);
                let items = match decoded {
                    Value::List(ref items) => items.clone(),
                    ref other => panic!("{name}: items is not a list: {other:?}"),
                };
                match split_list(&items, room) {
                    Ok(parts) => {
                        let got: Vec<Value> = parts.into_iter().map(Value::List).collect();
                        assert_eq!(Some(got), expected_parts, "{name}");
                        assert_eq!(expected_error, None, "{name}");
                    }
                    Err(err) => {
                        assert_eq!(Some(err.0.as_str()), expected_error, "{name}");
                        assert_eq!(expected_parts, None, "{name}");
                    }
                }
            }
            "mapping" => {
                let decoded = decode(&case["mapping"]);
                let mapping = match decoded {
                    Value::Dict(ref dict) => dict.clone(),
                    ref other => panic!("{name}: mapping is not a dict: {other:?}"),
                };
                match split_mapping(&mapping, room) {
                    Ok(parts) => {
                        let got: Vec<Value> = parts.into_iter().map(Value::Dict).collect();
                        assert_eq!(Some(got), expected_parts, "{name}");
                        assert_eq!(expected_error, None, "{name}");
                    }
                    Err(err) => {
                        assert_eq!(Some(err.0.as_str()), expected_error, "{name}");
                        assert_eq!(expected_parts, None, "{name}");
                    }
                }
            }
            "text" => {
                let text = case["text"].as_str().expect("case.text");
                match split_text(text, room) {
                    Ok(parts) => {
                        let got: Vec<Value> = parts.into_iter().map(Value::Str).collect();
                        assert_eq!(Some(got), expected_parts, "{name}");
                        assert_eq!(expected_error, None, "{name}");
                    }
                    Err(err) => {
                        assert_eq!(Some(err.0.as_str()), expected_error, "{name}");
                        assert_eq!(expected_parts, None, "{name}");
                    }
                }
            }
            other => panic!("{name}: unknown kind {other:?}"),
        }
    }
}
