//! Replays `electricity/scripts/generate_profile_corpus.py`'s golden
//! corpus (issue #488, M1-J1) against `electricity_compiler::profile`
//! and `electricity_config::profile`.
//!
//! The corpus is checked in (`tests/golden/profile_corpus.json`); CI
//! separately regenerates it with Python 3.11 and fails the build if it
//! differs (`.github/workflows/electricity-generated.yml`), so this test
//! only needs to trust the committed file.

use chrono::{FixedOffset, NaiveDate, NaiveDateTime};
use electricity_compiler::profile::{load_profile, parse_profile_document};
use electricity_config::{
    ComplexityBand, ProfileError, RoutingSettings, collect_orchestration_condition_paths,
    collect_orchestration_effect_paths, settings_from_raw, validate_effect_paths,
    validate_profile_routing_pins,
};
use electricity_value::{Dict, IntValue, Value};
use indexmap::IndexMap;
use serde_json::Value as Json;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};

fn corpus() -> Vec<Json> {
    let text = include_str!("golden/profile_corpus.json");
    serde_json::from_str(text).expect("golden/profile_corpus.json is valid JSON")
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "electricity-profile-corpus-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn decode_hex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

/// Decodes the tagged `{"t": ..., ...}` encoding
/// `generate_profile_corpus.py`'s own `encode()` writes.
fn decode_tagged(json: &Json) -> Value {
    let obj = json.as_object().expect("tagged value is a JSON object");
    match obj["t"].as_str().unwrap() {
        "none" => Value::None,
        "bool" => Value::Bool(obj["v"].as_bool().unwrap()),
        "int" => Value::Int(IntValue::parse_decimal(obj["v"].as_str().unwrap()).unwrap()),
        "float" => {
            let bits = u64::from_str_radix(obj["bits"].as_str().unwrap(), 16).unwrap();
            Value::Float(f64::from_bits(bits))
        }
        "str" => Value::Str(obj["v"].as_str().unwrap().to_string()),
        "bytes" => Value::Bytes(decode_hex(obj["v"].as_str().unwrap())),
        "list" => Value::List(
            obj["v"]
                .as_array()
                .unwrap()
                .iter()
                .map(decode_tagged)
                .collect(),
        ),
        "dict" => {
            let mut dict = Dict::new();
            for pair in obj["v"].as_array().unwrap() {
                let pair = pair.as_array().unwrap();
                dict.insert(decode_tagged(&pair[0]), decode_tagged(&pair[1]));
            }
            Value::Dict(dict)
        }
        "date" => Value::Date(
            NaiveDate::parse_from_str(obj["v"].as_str().unwrap(), "%Y-%m-%d")
                .expect("valid ISO date"),
        ),
        "datetime" => {
            let naive = NaiveDateTime::parse_from_str(
                obj["naive"].as_str().unwrap(),
                "%Y-%m-%dT%H:%M:%S%.f",
            )
            .expect("valid ISO naive datetime");
            let offset = obj["offset_seconds"]
                .as_i64()
                .map(|secs| FixedOffset::east_opt(secs as i32).expect("valid UTC offset"));
            Value::DateTime(naive, offset)
        }
        other => panic!("unknown tag {other:?}"),
    }
}

/// Decodes plain JSON (no tagged markers -- every `document` field in
/// the corpus is an ordinary string-keyed JSON tree) into a `Value`.
fn decode_plain(json: &Json) -> Value {
    match json {
        Json::Null => Value::None,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else {
                Value::Float(n.as_f64().unwrap())
            }
        }
        Json::String(s) => Value::Str(s.clone()),
        Json::Array(items) => Value::List(items.iter().map(decode_plain).collect()),
        Json::Object(map) => {
            let mut dict = Dict::new();
            for (k, v) in map {
                dict.insert(Value::Str(k.clone()), decode_plain(v));
            }
            Value::Dict(dict)
        }
    }
}

fn write_case_file(dir: &Path, input: &Json, profile_name: &str) -> PathBuf {
    let path = dir.join(format!("{profile_name}.yml"));
    if input["missing"].as_bool().unwrap_or(false) {
        return path;
    }
    if input
        .get("directory")
        .and_then(Json::as_bool)
        .unwrap_or(false)
    {
        fs::create_dir(&path).unwrap();
        return path;
    }
    if let Some(hex) = input.get("file_bytes_hex").and_then(Json::as_str) {
        fs::write(&path, decode_hex(hex)).unwrap();
        return path;
    }
    let text = input["file_text"].as_str().unwrap_or("");
    fs::write(&path, text).unwrap();
    path
}

/// How a case's own `error` text compares: see `generate_profile_corpus.
/// py`'s `_error_result` doc comment for what each mode means.
fn assert_error_matches(mode: &str, expected: &str, actual: &ProfileError, path: &Path) {
    let path_text = path.to_string_lossy().into_owned();
    let actual_text = if path_text.is_empty() {
        actual.0.clone()
    } else {
        actual.0.replace(&path_text, "{path}")
    };
    match mode {
        "exact" => assert_eq!(actual_text, expected, "exact error text mismatch"),
        "electricity_own" => { /* electricity's own text, not Circuitry's -- only `ok` matters */ }
        "location" => assert!(
            !actual_text.is_empty(),
            "expected a non-empty error message"
        ),
        "schema" => assert_schema_error_matches(expected, &actual_text),
        other => panic!("unknown comparison mode {other:?}"),
    }
}

/// Schema-failure comparison: the header and each line's own location
/// are exact; the message past the location, and multi-error line
/// order, are third-party `jsonschema` text (compared as a location
/// set only -- see the M1-J1 ruling).
fn assert_schema_error_matches(expected: &str, actual: &str) {
    let (expected_header, expected_rest) = expected.split_once(":\n").unwrap_or((expected, ""));
    let (actual_header, actual_rest) = actual.split_once(":\n").unwrap_or((actual, ""));
    assert_eq!(expected_header, actual_header, "schema error header");

    let locations = |text: &str| -> Vec<String> {
        text.lines()
            .map(|line| {
                let line = line.strip_prefix("  - ").unwrap_or(line);
                line.split_once(": ")
                    .map(|(loc, _)| loc.to_string())
                    .unwrap_or_else(|| line.to_string())
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    assert_eq!(
        locations(expected_rest),
        locations(actual_rest),
        "schema error location set"
    );
}

fn run_parse_profile_document(case: &Json, root: &Path) {
    let input = &case["input"];
    let profile_name = input["profile_name"].as_str().unwrap();
    let path = write_case_file(root, input, profile_name);

    let result = parse_profile_document(&path, profile_name);
    let expect = &case["expect"];
    let ok = expect["ok"].as_bool().unwrap();

    match (result, ok) {
        (Ok(value), true) => {
            let expected_value = decode_tagged(&expect["value"]);
            assert_eq!(
                value, expected_value,
                "case {:?}: parsed value",
                case["name"]
            );
        }
        (Err(err), false) => {
            let mode = expect["mode"].as_str().unwrap_or("exact");
            let expected_text = expect["error"].as_str().unwrap();
            assert_error_matches(mode, expected_text, &err, &path);
        }
        (Ok(_), false) => panic!("case {:?}: expected an error, got Ok", case["name"]),
        (Err(err), true) => panic!("case {:?}: expected Ok, got {err:?}", case["name"]),
    }
}

fn run_collect_effect_paths(case: &Json) {
    let document = decode_plain(&case["input"]["document"]);
    let effect_paths: Vec<String> = collect_orchestration_effect_paths(&document)
        .into_iter()
        .collect();
    let condition_paths: Vec<String> = collect_orchestration_condition_paths(&document)
        .into_iter()
        .collect();

    let expected_effect: Vec<String> = case["expect"]["effect_paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let expected_condition: Vec<String> = case["expect"]["condition_paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();

    assert_eq!(
        effect_paths, expected_effect,
        "case {:?}: effect paths",
        case["name"]
    );
    assert_eq!(
        condition_paths, expected_condition,
        "case {:?}: condition paths",
        case["name"]
    );
}

fn run_validate_effect_paths(case: &Json) {
    let input = &case["input"];
    let effects = decode_tagged(&input["effects"]);
    let document = decode_plain(&input["document"]);
    let profile_name = input["profile_name"].as_str().unwrap();

    let result = validate_effect_paths(&effects, &document, profile_name);
    let expect = &case["expect"];
    let ok = expect["ok"].as_bool().unwrap();
    match (result, ok) {
        (Ok(()), true) => {}
        (Err(err), false) => {
            let mode = expect["mode"].as_str().unwrap_or("exact");
            let expected_text = expect["error"].as_str().unwrap();
            assert_error_matches(mode, expected_text, &err, Path::new(""));
        }
        (Ok(()), false) => panic!("case {:?}: expected an error, got Ok", case["name"]),
        (Err(err), true) => panic!("case {:?}: expected Ok, got {err:?}", case["name"]),
    }
}

fn decode_routing(json: &Json) -> RoutingSettings {
    let bands = json["bands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| ComplexityBand {
            model: b["model"].as_str().unwrap().to_string(),
            max: b["max"].as_f64(),
            name: b["name"].as_str().map(str::to_string),
        })
        .collect();
    RoutingSettings {
        enabled: json["enabled"].as_bool().unwrap(),
        bands,
        respect_explicit: true,
    }
}

fn decode_effects_index_map(json: &Json) -> IndexMap<String, Dict> {
    let decoded = decode_tagged(json);
    let dict = decoded
        .as_dict()
        .expect("effects value is not a dict")
        .clone();
    let mut map = IndexMap::new();
    for (k, v) in dict {
        if let Some(vd) = v.as_dict() {
            map.insert(k.py_str(), vd.clone());
        }
    }
    map
}

fn run_validate_profile_routing_pins(case: &Json) {
    let input = &case["input"];
    let effects = decode_effects_index_map(&input["effects"]);
    let routing = decode_routing(&input["routing"]);
    let profile_name = input["profile_name"].as_str().unwrap();

    let result = validate_profile_routing_pins(&effects, &routing, profile_name);
    let expect = &case["expect"];
    let ok = expect["ok"].as_bool().unwrap();
    match (result, ok) {
        (Ok(()), true) => {}
        (Err(err), false) => {
            let mode = expect["mode"].as_str().unwrap_or("exact");
            let expected_text = expect["error"].as_str().unwrap();
            assert_error_matches(mode, expected_text, &err, Path::new(""));
        }
        (Ok(()), false) => panic!("case {:?}: expected an error, got Ok", case["name"]),
        (Err(err), true) => panic!("case {:?}: expected Ok, got {err:?}", case["name"]),
    }
}

fn run_settings_from_raw(case: &Json) {
    let input = &case["input"];
    let raw = decode_tagged(&input["raw"]);
    let profile_name = input["profile_name"].as_str().unwrap();
    let path = input["path"].as_str().unwrap();

    let settings = settings_from_raw(raw, profile_name, Path::new(path));
    let expect = &case["expect"];

    assert_eq!(settings.name, expect["name"].as_str().unwrap());
    assert_eq!(
        settings.adapter,
        expect["adapter"].as_str().map(str::to_string)
    );
    assert_eq!(settings.model, expect["model"].as_str().map(str::to_string));
    assert_eq!(settings.out, expect["out"].as_str().map(str::to_string));
    assert_eq!(
        Value::Dict(settings.inputs.clone()),
        decode_tagged(&expect["inputs"])
    );

    let expected_effects = decode_effects_index_map(&expect["effects"]);
    assert_eq!(
        settings.effects, expected_effects,
        "case {:?}: effects",
        case["name"]
    );

    match &expect["persistence"] {
        Json::Null => assert_eq!(settings.persistence, None),
        tagged => {
            let decoded = decode_tagged(tagged);
            let expected = decoded
                .as_dict()
                .expect("persistence tagged value is not a dict")
                .clone();
            assert_eq!(settings.persistence, Some(expected));
        }
    }
    assert_eq!(settings.raw, decode_tagged(&expect["raw"]));
}

fn run_load_profile(case: &Json, root: &Path) {
    let input = &case["input"];
    let document = decode_plain(&input["document"]);
    let profile_name = input["profile_name"].as_str().unwrap();
    let path = write_case_file(root, input, profile_name);

    let result = load_profile(&path, &document);
    let expect = &case["expect"];
    let ok = expect["ok"].as_bool().unwrap();

    match (result, ok) {
        (Ok(settings), true) => {
            assert_eq!(settings.name, expect["name"].as_str().unwrap());
            assert_eq!(
                settings.adapter,
                expect["adapter"].as_str().map(str::to_string)
            );
            assert_eq!(settings.model, expect["model"].as_str().map(str::to_string));
            assert_eq!(settings.out, expect["out"].as_str().map(str::to_string));
            assert_eq!(
                Value::Dict(settings.inputs.clone()),
                decode_tagged(&expect["inputs"])
            );
            let expected_effects = decode_effects_index_map(&expect["effects"]);
            assert_eq!(settings.effects, expected_effects);
        }
        (Err(err), false) => {
            let mode = expect["mode"].as_str().unwrap_or("exact");
            let expected_text = expect["error"].as_str().unwrap();
            assert_error_matches(mode, expected_text, &err, &path);
        }
        (Ok(_), false) => panic!("case {:?}: expected an error, got Ok", case["name"]),
        (Err(err), true) => panic!("case {:?}: expected Ok, got {err:?}", case["name"]),
    }
}

#[test]
fn replay_profile_corpus() {
    for case in corpus() {
        let kind = case["kind"].as_str().expect("case has a kind");
        let root = TempDir::new(case["name"].as_str().unwrap_or("case"));
        match kind {
            "parse_profile_document" => run_parse_profile_document(&case, &root),
            "collect_effect_paths" => run_collect_effect_paths(&case),
            "validate_effect_paths" => run_validate_effect_paths(&case),
            "validate_profile_routing_pins" => run_validate_profile_routing_pins(&case),
            "settings_from_raw" => run_settings_from_raw(&case),
            "load_profile" => run_load_profile(&case, &root),
            other => panic!("unknown corpus case kind {other:?}"),
        }
    }
}
