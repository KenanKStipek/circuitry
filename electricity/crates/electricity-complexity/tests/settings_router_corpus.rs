//! Replays the `settings_cases`, `router_cases` and `band_named_cases`
//! families from `electricity/scripts/generate_complexity_corpus.py`'s
//! golden corpus against `electricity_config::resolve_complexity_settings`,
//! `electricity_complexity::route_model` and
//! `electricity_config::RoutingSettings::band_named` -- the three
//! differential case families `tests/corpus.rs` doesn't cover (its own
//! cases only ever call `score()`).

use electricity_complexity::route_model;
use electricity_config::{ComplexityBand, RoutingSettings, resolve_complexity_settings};
use electricity_value::{Dict, Value};

fn dict_get<'a>(dict: &'a Dict, key: &str) -> Option<&'a Value> {
    dict.get(&Value::Str(key.to_string()))
}

fn load_corpus() -> Value {
    let text = include_str!("golden/complexity_corpus.json");
    electricity_json::loads(text).expect("golden/complexity_corpus.json is valid JSON")
}

fn case_list<'a>(root: &'a Dict, key: &str) -> &'a [Value] {
    match dict_get(root, key) {
        Some(Value::List(items)) => items,
        _ => panic!("corpus must have a '{key}' list"),
    }
}

fn description(case: &Dict, index: usize) -> String {
    match dict_get(case, "description") {
        Some(Value::Str(s)) => s.clone(),
        _ => format!("case {index}"),
    }
}

fn as_f64(value: &Value) -> f64 {
    match value {
        Value::Float(f) => *f,
        Value::Int(i) => i.to_f64(),
        other => panic!("expected a number, found {other:?}"),
    }
}

fn number_pairs(dict: &Dict) -> Vec<(String, f64)> {
    dict.iter()
        .map(|(k, v)| {
            let key = match k {
                Value::Str(s) => s.clone(),
                other => panic!("expected a string key, found {other:?}"),
            };
            (key, as_f64(v))
        })
        .collect()
}

// Any two `NaN`s compare equal here regardless of payload/sign bit --
// same reasoning as `tests/corpus.rs`'s own `bit_exact_eq`: an
// architecture-default `NaN` (from an invalid op with no `NaN` input
// operand) has a CPU-defined sign, not one this crate's code or the
// golden corpus's own `NaN` literal ever records.
fn bit_exact_pairs(a: &[(String, f64)], b: &[(String, f64)]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|((ka, va), (kb, vb))| {
            ka == kb && ((va.is_nan() && vb.is_nan()) || va.to_bits() == vb.to_bits())
        })
}

#[test]
fn golden_corpus_settings() {
    let parsed = load_corpus();
    let Value::Dict(ref root) = parsed else {
        panic!("corpus root must be an object");
    };
    let cases = case_list(root, "settings_cases");
    assert!(!cases.is_empty());

    let mut failures = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let Value::Dict(case) = case else {
            failures.push(format!("case {i}: not an object"));
            continue;
        };
        let desc = description(case, i);
        let raw = dict_get(case, "raw").cloned().unwrap_or(Value::None);

        let mut runtime = Dict::new();
        runtime.insert(Value::Str("complexity".to_string()), raw);
        let result = resolve_complexity_settings(Some(&Value::Dict(runtime)));

        match (dict_get(case, "error"), dict_get(case, "expected")) {
            (Some(Value::Str(expected_message)), None) => match result {
                Ok(_) => failures.push(format!("case {desc}: expected an error, got Ok")),
                Err(err) => {
                    if &err.0 != expected_message {
                        failures.push(format!(
                            "case {desc}: expected error {expected_message:?}, got {:?}",
                            err.0
                        ));
                    }
                }
            },
            (None, Some(Value::Dict(expected))) => {
                let expected_weights = match dict_get(expected, "weights") {
                    Some(Value::Dict(d)) => number_pairs(d),
                    _ => panic!("case {desc}: expected.weights must be an object"),
                };
                let expected_keywords = match dict_get(expected, "keywords") {
                    Some(Value::Dict(d)) => number_pairs(d),
                    _ => panic!("case {desc}: expected.keywords must be an object"),
                };
                match result {
                    Err(err) => failures.push(format!("case {desc}: expected Ok, got {:?}", err.0)),
                    Ok(settings) => {
                        let got_weights: Vec<(String, f64)> = settings
                            .scoring
                            .weights
                            .iter()
                            .map(|(k, v)| (k.clone(), *v))
                            .collect();
                        let got_keywords: Vec<(String, f64)> = settings
                            .scoring
                            .keywords
                            .iter()
                            .map(|(k, v)| (k.clone(), *v))
                            .collect();
                        if !bit_exact_pairs(&got_weights, &expected_weights) {
                            failures.push(format!(
                                "case {desc}: weights mismatch\n  expected: {expected_weights:?}\n  got:      {got_weights:?}"
                            ));
                        }
                        if !bit_exact_pairs(&got_keywords, &expected_keywords) {
                            failures.push(format!(
                                "case {desc}: keywords mismatch\n  expected: {expected_keywords:?}\n  got:      {got_keywords:?}"
                            ));
                        }
                    }
                }
            }
            _ => panic!("case {desc}: must have exactly one of 'error'/'expected'"),
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

fn band_from_value(value: &Value) -> ComplexityBand {
    let Value::Dict(dict) = value else {
        panic!("band entry must be an object");
    };
    let model = match dict_get(dict, "model") {
        Some(Value::Str(s)) => s.clone(),
        other => panic!("band.model must be a string, found {other:?}"),
    };
    let max = match dict_get(dict, "max") {
        Some(Value::None) | None => None,
        Some(other) => Some(as_f64(other)),
    };
    let name = match dict_get(dict, "name") {
        Some(Value::Str(s)) => Some(s.clone()),
        Some(Value::None) | None => None,
        other => panic!("band.name must be a string or null, found {other:?}"),
    };
    ComplexityBand { model, max, name }
}

fn bands_from_value(value: &Value) -> Vec<ComplexityBand> {
    let Value::List(items) = value else {
        panic!("'bands' must be a list");
    };
    items.iter().map(band_from_value).collect()
}

fn expected_band_from_case(case: &Dict) -> Option<(String, Option<f64>, Option<String>)> {
    match dict_get(case, "expected") {
        Some(Value::None) | None => None,
        Some(Value::Dict(expected)) => {
            let model = match dict_get(expected, "model") {
                Some(Value::Str(s)) => s.clone(),
                other => panic!("expected.model must be a string, found {other:?}"),
            };
            let max = match dict_get(expected, "max") {
                Some(Value::None) | None => None,
                Some(other) => Some(as_f64(other)),
            };
            let name = match dict_get(expected, "name") {
                Some(Value::Str(s)) => Some(s.clone()),
                Some(Value::None) | None => None,
                other => panic!("expected.name must be a string or null, found {other:?}"),
            };
            Some((model, max, name))
        }
        other => panic!("'expected' must be an object or null, found {other:?}"),
    }
}

fn as_bool(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        other => panic!("expected a boolean, found {other:?}"),
    }
}

#[test]
fn golden_corpus_router() {
    let parsed = load_corpus();
    let Value::Dict(ref root) = parsed else {
        panic!("corpus root must be an object");
    };
    let cases = case_list(root, "router_cases");
    assert!(!cases.is_empty());

    let mut failures = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let Value::Dict(case) = case else {
            failures.push(format!("case {i}: not an object"));
            continue;
        };
        let desc = description(case, i);
        let bands = bands_from_value(dict_get(case, "bands").expect("'bands' present"));
        let settings = RoutingSettings {
            enabled: as_bool(dict_get(case, "enabled").expect("'enabled' present")),
            bands,
            respect_explicit: as_bool(
                dict_get(case, "respect_explicit").expect("'respect_explicit' present"),
            ),
        };
        let score = as_f64(dict_get(case, "score").expect("'score' present"));
        let explicit = as_bool(dict_get(case, "explicit").expect("'explicit' present"));
        let expected = expected_band_from_case(case);

        let decision = route_model(score, &settings, explicit);
        let got = decision.map(|d| (d.band.model.clone(), d.band.max, d.band.name.clone()));
        if got != expected {
            failures.push(format!("case {desc}: expected {expected:?}, got {got:?}"));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn golden_corpus_band_named() {
    let parsed = load_corpus();
    let Value::Dict(ref root) = parsed else {
        panic!("corpus root must be an object");
    };
    let cases = case_list(root, "band_named_cases");
    assert!(!cases.is_empty());

    let mut failures = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let Value::Dict(case) = case else {
            failures.push(format!("case {i}: not an object"));
            continue;
        };
        let desc = description(case, i);
        let bands = bands_from_value(dict_get(case, "bands").expect("'bands' present"));
        let name = match dict_get(case, "name") {
            Some(Value::Str(s)) => s.as_str(),
            other => panic!("'name' must be a string, found {other:?}"),
        };
        let settings = RoutingSettings {
            enabled: true,
            bands,
            respect_explicit: true,
        };
        let expected = expected_band_from_case(case);

        let found = settings.band_named(name);
        let got = found.map(|b| (b.model.clone(), b.max, b.name.clone()));
        if got != expected {
            failures.push(format!("case {desc}: expected {expected:?}, got {got:?}"));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
