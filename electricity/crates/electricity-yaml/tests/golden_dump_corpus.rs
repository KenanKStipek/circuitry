//! Replays `tests/golden/dump_corpus.json`
//! (`scripts/generate_yaml_dump_corpus.py`) -- real PyYAML's own
//! `yaml.dump(value, default_flow_style=False, sort_keys=...)` output
//! for every case, compared byte for byte against this crate's own
//! [`dump`] (issue #506's lane L1).

use electricity_value::{Dict, IntValue, Value};
use electricity_yaml::{DumpOptions, dump};

fn value_from_json(j: &serde_json::Value) -> Value {
    let obj = j.as_object().expect("every corpus input is a JSON object");
    match obj["t"].as_str().expect("every corpus input has a 't' tag") {
        "none" => Value::None,
        "bool" => Value::Bool(obj["v"].as_bool().unwrap()),
        "int" => Value::Int(IntValue::parse_decimal(obj["v"].as_str().unwrap()).unwrap()),
        "float" => {
            let s = obj["v"].as_str().unwrap();
            Value::Float(match s {
                "nan" => f64::NAN,
                "inf" => f64::INFINITY,
                "-inf" => f64::NEG_INFINITY,
                other => other.parse().unwrap(),
            })
        }
        "str" => Value::Str(obj["v"].as_str().unwrap().to_string()),
        "bytes" => Value::Bytes(
            obj["v"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_u64().unwrap() as u8)
                .collect(),
        ),
        "list" => Value::List(
            obj["v"]
                .as_array()
                .unwrap()
                .iter()
                .map(value_from_json)
                .collect(),
        ),
        "dict" => {
            let mut d = Dict::new();
            for pair in obj["v"].as_array().unwrap() {
                let pair = pair.as_array().unwrap();
                d.insert(value_from_json(&pair[0]), value_from_json(&pair[1]));
            }
            Value::Dict(d)
        }
        "date" => {
            let s = obj["v"].as_str().unwrap();
            Value::Date(chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap())
        }
        "datetime" => {
            let naive = chrono::NaiveDateTime::parse_from_str(
                obj["naive"].as_str().unwrap(),
                "%Y-%m-%dT%H:%M:%S%.f",
            )
            .unwrap();
            let offset = obj["offset_sec"]
                .as_i64()
                .map(|s| chrono::FixedOffset::east_opt(s as i32).unwrap());
            Value::DateTime(naive, offset)
        }
        other => panic!("golden_dump_corpus: unknown input kind {other:?}"),
    }
}

#[test]
fn matches_pyyaml_dump_corpus() {
    let data = include_str!("golden/dump_corpus.json");
    let cases: Vec<serde_json::Value> = serde_json::from_str(data).unwrap();
    assert!(
        cases.len() > 100,
        "corpus should hold every documented category"
    );

    let mut failures = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        // The one documented, low-priority divergence (`Value::Bytes`,
        // never produced by a parsed orchestration plan) -- `dump.rs`'s
        // own module doc comment.
        if case["input"]["t"] == "bytes" {
            continue;
        }
        let value = value_from_json(&case["input"]);
        let sort_keys = case["sort_keys"].as_bool().unwrap();
        let expected = case["expected"].as_str().unwrap();
        match dump(&value, &DumpOptions { sort_keys }) {
            Ok(text) if text == expected => {}
            Ok(text) => failures.push(format!(
                "case {i}: mismatch\n  expected: {expected:?}\n  got:      {text:?}"
            )),
            Err(e) => failures.push(format!(
                "case {i}: unexpected error {e} (expected {expected:?})"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases failed:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n\n")
    );
}
