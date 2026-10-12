//! Replays `electricity/scripts/generate_complexity_corpus.py`'s own
//! differential corpus -- Circuitry's real `circuitry.core.complexity.
//! score` and `core/prompt.py::_complexity_meta` -- against
//! [`electricity_complexity::score`].
//!
//! The corpus is checked in (`tests/golden/complexity_corpus.json`); CI
//! separately regenerates it with Python 3.11 and fails the build if it
//! differs (`.github/workflows/electricity-generated.yml`), so this test
//! only needs to trust the committed file. Read with `electricity-json`
//! (never `serde_json`): several cases carry bare `NaN`/`Infinity`
//! literals and arbitrary-precision integers, both of which `json.dumps`'s
//! own `allow_nan=True` default writes verbatim and `serde_json` does not
//! accept.

use electricity_complexity::{score, ScoreError, ScoreInput};
use electricity_value::{Dict, Value};

fn dict_get<'a>(dict: &'a Dict, key: &str) -> Option<&'a Value> {
    dict.get(&Value::Str(key.to_string()))
}

/// Deep structural equality by IEEE-754 *bit pattern* for floats (so
/// `-0.0 != 0.0` and `NaN == NaN`), exact string/key-order equality
/// otherwise -- never [`Value`]'s own `PartialEq`, which is Python's
/// dict-key equality (`NaN != NaN`, the numeric tower), the opposite of
/// what this corpus pins.
fn bit_exact_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::None, Value::None) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x.to_bigint() == y.to_bigint(),
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Bytes(x), Value::Bytes(y)) => x == y,
        (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| bit_exact_eq(a, b))
        }
        (Value::Dict(x), Value::Dict(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|((ka, va), (kb, vb))| bit_exact_eq(ka, kb) && bit_exact_eq(va, vb))
        }
        (Value::Date(x), Value::Date(y)) => x == y,
        (Value::DateTime(x1, o1), Value::DateTime(x2, o2)) => x1 == x2 && o1 == o2,
        _ => false,
    }
}

fn build_input<'a>(input: &'a Value) -> ScoreInput<'a> {
    let Value::Dict(dict) = input else {
        panic!("case 'input' must be an object");
    };
    ScoreInput {
        definition: dict_get(dict, "definition"),
        rendered_prompt: match dict_get(dict, "rendered_prompt") {
            Some(Value::Str(s)) => Some(s.as_str()),
            _ => None,
        },
        weights: dict_get(dict, "weights"),
        keyword_weights: dict_get(dict, "keyword_weights"),
        structure: dict_get(dict, "structure"),
    }
}

fn py_exc_name(err: &ScoreError) -> &'static str {
    err.py_class.name()
}

#[test]
fn golden_corpus_complexity() {
    let text = include_str!("golden/complexity_corpus.json");
    let parsed = electricity_json::loads(text).expect("golden/complexity_corpus.json is valid JSON");
    let Value::Dict(ref root) = parsed else {
        panic!("corpus root must be an object");
    };
    let Some(Value::List(cases)) = dict_get(&root, "score_cases") else {
        panic!("corpus must have a 'score_cases' list");
    };
    assert!(!cases.is_empty());

    let mut failures = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let Value::Dict(case) = case else {
            failures.push(format!("case {i}: not an object"));
            continue;
        };
        let description = match dict_get(case, "description") {
            Some(Value::Str(s)) => s.clone(),
            _ => format!("case {i}"),
        };
        let input_value = dict_get(case, "input").cloned().unwrap_or(Value::Dict(Dict::new()));
        let input = build_input(&input_value);
        let result = score(&input);

        match (dict_get(case, "error"), dict_get(case, "expected")) {
            (Some(Value::Dict(error)), None) => {
                let expected_class = match dict_get(error, "class") {
                    Some(Value::Str(s)) => s.as_str(),
                    _ => panic!("case {description}: error.class must be a string"),
                };
                let expected_message = match dict_get(error, "message") {
                    Some(Value::Str(s)) => s.as_str(),
                    _ => panic!("case {description}: error.message must be a string"),
                };
                match result {
                    Ok(_) => failures.push(format!(
                        "case {description}: expected {expected_class}, got Ok"
                    )),
                    Err(err) => {
                        if py_exc_name(&err) != expected_class || err.message != expected_message {
                            failures.push(format!(
                                "case {description}: expected {expected_class}({expected_message:?}), got {}({:?})",
                                py_exc_name(&err),
                                err.message
                            ));
                        }
                    }
                }
            }
            (None, Some(Value::Dict(expected))) => {
                let expected_meta = dict_get(expected, "meta").expect("expected.meta present");
                let expected_estimated: Vec<String> = match dict_get(expected, "estimated_signals") {
                    Some(Value::List(items)) => items
                        .iter()
                        .map(|v| match v {
                            Value::Str(s) => s.clone(),
                            other => panic!("estimated_signals entries must be strings, got {other:?}"),
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                match result {
                    Err(err) => failures.push(format!(
                        "case {description}: expected Ok, got {}({:?})",
                        py_exc_name(&err),
                        err.message
                    )),
                    Ok(complexity) => {
                        let got_meta = complexity.to_meta();
                        if !bit_exact_eq(&got_meta, expected_meta) {
                            failures.push(format!(
                                "case {description}: meta mismatch\n  expected: {expected_meta:?}\n  got:      {got_meta:?}"
                            ));
                        }
                        let mut got_estimated: Vec<String> = complexity
                            .signals
                            .iter()
                            .filter(|s| s.estimated)
                            .map(|s| s.name.to_string())
                            .collect();
                        got_estimated.sort();
                        if got_estimated != expected_estimated {
                            failures.push(format!(
                                "case {description}: estimated-signal mismatch: expected {expected_estimated:?}, got {got_estimated:?}"
                            ));
                        }
                    }
                }
            }
            _ => panic!("case {description}: must have exactly one of 'error'/'expected'"),
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
