//! `cli/profiles.py`'s own pure checks, ported as a library (issue #488,
//! milestone M1-J1): the profile record ([`ProfileSettings`]), the
//! effect/condition-path walk over a raw orchestration document
//! (`_collect_effect_paths`), per-effect-path validation, and the
//! routing-pin check. File access and schema validation stay out of this
//! crate (`electricity-compiler`'s own `profile.rs`, which already
//! reaches `schema_instance`'s `Value` -> schema-instance conversion --
//! this crate cannot depend back on that one without a cycle).
//!
//! `document` throughout this module is always the raw, loaded
//! orchestration [`Value`] (Python's `orch`), never a compiled
//! `electricity_bytecode::Program`.
//!
//! No run behaviour changes here: nothing in electricity's run wiring
//! calls any function in this module yet (lane R wires `--profile`; lane
//! J2 applies a loaded profile's settings onto a compiled program).

use crate::complexity::RoutingSettings;
use crate::util::is_truthy;
use electricity_value::pycompat::py_strip;
use electricity_value::{CompareError, Dict, Value};
use indexmap::IndexMap;
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

/// `cli/profiles.py`'s own `ProfileError` family -- one shape, like
/// [`crate::ConfigError`]: Circuitry raises several distinct exception
/// classes here (`ProfileValidationError`, `TypeError`, a YAML error, a
/// `UnicodeDecodeError`), but none of them reaches a run's recorded
/// state as a class name, only as this one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileError(pub String);

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ProfileError {}

/// `cli/profiles.py::ProfileSettings` -- the validated record a loaded
/// profile resolves to. `effects`/`inputs`/`persistence` are already
/// normalized by [`settings_from_raw`] (only dict-valued `effects`
/// entries kept, `str`-keyed; `inputs` only when the raw value is
/// itself a non-empty dict; `persistence` only when it's a dict).
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileSettings {
    pub name: String,
    pub path: PathBuf,
    pub adapter: Option<String>,
    pub model: Option<String>,
    pub out: Option<String>,
    pub inputs: Dict,
    pub effects: IndexMap<String, Dict>,
    pub persistence: Option<Dict>,
    /// The whole parsed document (`raw.get(...) or {}`'s own result) --
    /// `R`/`J2`'s own `effective_settings.profile.content`.
    pub raw: Value,
}

/// `cli/profiles.py`: the file name with exactly one trailing `.yml` or
/// `.yaml` stripped, case-sensitive -- the string `cof run --profile
/// <name>` would have used for the same file (DESIGN.md §6.11).
pub fn profile_name_from_path(path: &Path) -> String {
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    for suffix in [".yml", ".yaml"] {
        if let Some(stripped) = file_name.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    file_name
}

fn dict_get<'a>(dict: &'a Dict, key: &str) -> Option<&'a Value> {
    dict.get(&Value::Str(key.to_string()))
}

/// `str(effect.get("type") or "").strip().lower()` -- the same
/// truthy-then-stringify-then-normalize idiom
/// `electricity_compiler::compose`'s own `effect_type_of` ports
/// (duplicated here rather than shared across crates, since that
/// module is private to a different lane's file).
fn effect_type_of(dict: &Dict) -> String {
    let value = dict_get(dict, "type");
    let raw = if is_truthy(value) {
        value.map(Value::py_str).unwrap_or_default()
    } else {
        String::new()
    };
    py_strip(&raw).to_lowercase()
}

/// `effect["name"]` if it is a `str`, else `""` -- the name stays
/// *unstripped* (`own_path` is built from it directly); only `has_name`
/// uses the stripped form.
fn effect_name_raw(dict: &Dict) -> &str {
    match dict_get(dict, "name") {
        Some(Value::Str(s)) => s.as_str(),
        _ => "",
    }
}

/// `effect.get(key) or []` -- a falsy value (including a missing key)
/// becomes an empty list; a truthy non-list value is passed through
/// unchanged, so the recursive walk's own `as_list()` check silently
/// skips it (matching Python's `if not isinstance(effects, list):
/// return`), rather than this helper forcing a conversion.
fn or_empty_list<'a>(dict: &'a Dict, key: &str, empty: &'a Value) -> &'a Value {
    match dict_get(dict, key) {
        Some(v) if is_truthy(Some(v)) => v,
        _ => empty,
    }
}

/// `effect.get(a) or effect.get(b) or []`.
fn or_empty_list2<'a>(dict: &'a Dict, a: &str, b: &str, empty: &'a Value) -> &'a Value {
    match dict_get(dict, a) {
        Some(v) if is_truthy(Some(v)) => v,
        _ => or_empty_list(dict, b, empty),
    }
}

/// `cli/profiles.py::_collect_effect_paths`: walks *effects* (a raw
/// `effects:`/`steps:` list) and adds every named effect's dotted path
/// to *paths*; when *conditions* is `Some`, also adds the pseudo-paths
/// that address a named conditional's `if`/loop's `while`. The
/// top-level `finally:` is never reached (callers start from
/// `document.get("effects") or document.get("steps") or []`, which
/// excludes it); `use` children are never entered either (not one of
/// the four types this function recurses into).
fn collect_effect_paths_inner(
    effects: &Value,
    scope: &str,
    paths: &mut BTreeSet<String>,
    mut conditions: Option<&mut BTreeSet<String>>,
) {
    let Some(items) = effects.as_list() else {
        return;
    };
    let empty_list = Value::List(Vec::new());
    for effect in items {
        let Some(dict) = effect.as_dict() else {
            continue;
        };
        let effect_type = effect_type_of(dict);
        let name = effect_name_raw(dict);
        let has_name = !py_strip(name).is_empty();
        let own_path = if !scope.is_empty() && has_name {
            format!("{scope}.{name}")
        } else if has_name {
            name.to_string()
        } else {
            scope.to_string()
        };
        if has_name {
            paths.insert(own_path.clone());
        }

        match effect_type.as_str() {
            "dynamic" | "reflector" => {
                let child_scope = if has_name { own_path.as_str() } else { scope };
                let children = or_empty_list2(dict, "effects", "steps", &empty_list);
                collect_effect_paths_inner(children, child_scope, paths, conditions.as_deref_mut());
            }
            "conditional" | "if" => {
                let branch_scope = if has_name { own_path.as_str() } else { scope };
                if has_name {
                    if let Some(cond) = conditions.as_deref_mut() {
                        cond.insert(format!("{own_path}.if"));
                        cond.insert(format!("{own_path}.condition"));
                    }
                }
                let then_list = or_empty_list(dict, "then", &empty_list);
                collect_effect_paths_inner(
                    then_list,
                    branch_scope,
                    paths,
                    conditions.as_deref_mut(),
                );
                let else_list = or_empty_list(dict, "else", &empty_list);
                collect_effect_paths_inner(
                    else_list,
                    branch_scope,
                    paths,
                    conditions.as_deref_mut(),
                );
            }
            "loop" => {
                let body_scope = if has_name { own_path.as_str() } else { scope };
                if has_name {
                    if let Some(cond) = conditions.as_deref_mut() {
                        cond.insert(format!("{own_path}.while"));
                        cond.insert(format!("{own_path}.condition"));
                    }
                }
                let body_list = or_empty_list(dict, "body", &empty_list);
                collect_effect_paths_inner(body_list, body_scope, paths, conditions.as_deref_mut());
            }
            _ => {}
        }
    }
}

/// The document's own top-level `effects:`/`steps:` list -- the root
/// scope every path/condition walk starts from.
fn top_level_effects(document: &Value) -> Value {
    let Some(dict) = document.as_dict() else {
        return Value::List(Vec::new());
    };
    let empty = Value::List(Vec::new());
    or_empty_list2(dict, "effects", "steps", &empty).clone()
}

/// `collect_orchestration_effect_paths`: every dotted effect path
/// *document* defines.
pub fn collect_orchestration_effect_paths(document: &Value) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    let effects = top_level_effects(document);
    collect_effect_paths_inner(&effects, "", &mut paths, None);
    paths
}

/// `collect_orchestration_condition_paths`: the pseudo-paths that
/// address a named container's own *condition* (a conditional's `if`,
/// a loop's `while`) -- never overridable or disableable on their own.
pub fn collect_orchestration_condition_paths(document: &Value) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    let mut conditions = BTreeSet::new();
    let effects = top_level_effects(document);
    collect_effect_paths_inner(&effects, "", &mut paths, Some(&mut conditions));
    conditions
}

/// `condition_target_message`: the refusal text for a profile override
/// aimed at a container's condition rather than an effect.
///
/// # Panics
///
/// Panics if `targeted` is empty, the same as Python's `ordered[0]`
/// raising `IndexError` there -- every caller only reaches this
/// function with a non-empty `targeted` (`validate_effect_paths`'s own
/// emptiness check gates the call).
pub fn condition_target_message(targeted: &[String], profile_name: &str) -> String {
    let mut ordered: Vec<&str> = targeted.iter().map(String::as_str).collect();
    ordered.sort_unstable();
    let joined = ordered.join(", ");
    let prefix = match ordered[0].rsplit_once('.') {
        Some((before, _)) => before,
        None => ordered[0],
    };
    format!(
        "Profile {} targets condition path(s): {joined}. A conditional's 'if' and a \
         loop's 'while' are conditions, not effects — they cannot be disabled or \
         overridden, because a container with no condition has no defined branch. \
         Disable the whole container instead (e.g. {prefix}: {{enabled: false}}), \
         which disables its entire subtree.",
        name_repr(profile_name),
    )
}

/// `{name!r}` -- Python's `repr()` of the profile name.
fn name_repr(name: &str) -> String {
    Value::Str(name.to_string()).py_repr()
}

/// Sorts *values* the way CPython's `sorted()` would, raising the same
/// `TypeError` text when two incomparable types meet -- a full,
/// general-purpose port of CPython's listsort is out of scope; this is
/// exact for 0/1-element inputs, for inputs where every pair of
/// elements is mutually orderable (same type, or both in the numeric
/// tower), and for exactly two elements of different, unorderable
/// types (CPython's own `count_run`/`binarysort` make exactly one
/// comparison, `values[1] < values[0]`, for a 2-element list, which
/// this reproduces directly). A longer list mixing 3+ incomparable
/// types is not guaranteed to raise with the same operand order CPython
/// would -- `cli/profiles.py`'s own ruling for this port names that gap
/// explicitly: compare such a case by location, not by exact text.
fn py_sort_values(mut values: Vec<Value>) -> Result<Vec<Value>, String> {
    if values.len() < 2 {
        return Ok(values);
    }
    if values.len() == 2 {
        match values[1].py_partial_cmp(&values[0]) {
            Ok(Some(std::cmp::Ordering::Less)) => values.swap(0, 1),
            Ok(_) => {}
            Err(err) => return Err(py_sort_type_error_text(err)),
        }
        return Ok(values);
    }
    // Plain insertion sort for everything else -- correct final order
    // for any mutually-comparable input; see the doc comment above for
    // what it does not claim to reproduce exactly.
    for i in 1..values.len() {
        let mut j = i;
        while j > 0 {
            match values[j].py_partial_cmp(&values[j - 1]) {
                Ok(Some(std::cmp::Ordering::Less)) => {
                    values.swap(j, j - 1);
                    j -= 1;
                }
                Ok(_) => break,
                Err(err) => return Err(py_sort_type_error_text(err)),
            }
        }
    }
    Ok(values)
}

/// The exact `TypeError` text CPython's `sorted()` raises for a
/// [`CompareError`]: the usual `'<' not supported between instances of
/// '{left}' and '{right}'` for two truly unrelated types, but CPython's
/// own distinct text for a naive/aware `datetime.datetime` mismatch or a
/// `datetime.date`/`datetime.datetime` mismatch -- neither of those two
/// is phrased as a `'<'` complaint, so [`CompareError`]'s own `Display`
/// is used unchanged for them.
fn py_sort_type_error_text(err: CompareError) -> String {
    match err {
        CompareError::IncomparableTypes { left, right } => {
            format!("'<' not supported between instances of '{left}' and '{right}'")
        }
        other => other.to_string(),
    }
}

/// `", ".join(unknown)` -- CPython's own `TypeError` text the moment it
/// meets the first non-`str` element: `"sequence item {i}: expected str
/// instance, {type} found"`.
fn py_join_strs(values: &[Value]) -> Result<String, String> {
    let mut parts = Vec::with_capacity(values.len());
    for (i, value) in values.iter().enumerate() {
        match value {
            Value::Str(s) => parts.push(s.clone()),
            other => {
                return Err(format!(
                    "sequence item {i}: expected str instance, {} found",
                    other.type_name()
                ));
            }
        }
    }
    Ok(parts.join(", "))
}

/// `_validate_effect_paths`: `effects` is the profile's raw
/// `effects:` value (`raw.get("effects") or {}`, so it may be anything
/// -- only a non-empty mapping is checked at all); `document` is the
/// raw orchestration.
pub fn validate_effect_paths(
    effects: &Value,
    document: &Value,
    profile_name: &str,
) -> Result<(), ProfileError> {
    let effects_map = match effects.as_dict() {
        Some(d) if !d.is_empty() => d,
        _ => return Ok(()),
    };

    let condition_paths = collect_orchestration_condition_paths(document);
    let mut targeted: Vec<String> = effects_map
        .keys()
        .filter_map(|k| k.as_str())
        .filter(|k| condition_paths.contains(*k))
        .map(str::to_string)
        .collect();
    targeted.sort_unstable();
    if !targeted.is_empty() {
        return Err(ProfileError(condition_target_message(
            &targeted,
            profile_name,
        )));
    }

    let valid_paths = collect_orchestration_effect_paths(document);
    let unknown_values: Vec<Value> = effects_map
        .keys()
        .filter(|k| match k.as_str() {
            Some(s) => !valid_paths.contains(s),
            None => true,
        })
        .cloned()
        .collect();
    if unknown_values.is_empty() {
        return Ok(());
    }

    let sorted_unknown = py_sort_values(unknown_values).map_err(ProfileError)?;
    let joined = py_join_strs(&sorted_unknown).map_err(ProfileError)?;
    let valid_list = if valid_paths.is_empty() {
        "(orchestration defines no named effects)".to_string()
    } else {
        valid_paths.into_iter().collect::<Vec<_>>().join(", ")
    };
    Err(ProfileError(format!(
        "Profile {} references unknown effect path(s): {joined}. Valid effect paths: {valid_list}",
        name_repr(profile_name),
    )))
}

/// `validate_profile_routing_pins`: *effects* is a [`ProfileSettings`]'
/// own, already-normalized `effects` map (every value a real `Dict`),
/// called once effective settings (and so the run's real
/// [`RoutingSettings`]) are resolved -- never at profile-load time.
pub fn validate_profile_routing_pins(
    effects: &IndexMap<String, Dict>,
    routing: &RoutingSettings,
    profile_name: &str,
) -> Result<(), ProfileError> {
    if effects.is_empty() {
        return Ok(());
    }

    let mut unmatched: Vec<(String, String)> = Vec::new();
    for (path, override_) in effects {
        let pin = match dict_get(override_, "routing") {
            Some(Value::Str(s)) => s.clone(),
            _ => continue,
        };
        if routing.band_named(&pin).is_none() {
            unmatched.push((path.clone(), pin));
        }
    }
    if unmatched.is_empty() {
        return Ok(());
    }
    unmatched.sort();

    let mut valid_names: BTreeSet<&str> = BTreeSet::new();
    for band in &routing.bands {
        if let Some(name) = band.name.as_deref() {
            if !name.is_empty() {
                valid_names.insert(name);
            }
        }
    }
    let valid_list = if valid_names.is_empty() {
        "(no named bands configured)".to_string()
    } else {
        valid_names.into_iter().collect::<Vec<_>>().join(", ")
    };

    let pins = unmatched
        .iter()
        .map(|(path, pin)| {
            format!(
                "{} -> {}",
                Value::Str(path.clone()).py_repr(),
                Value::Str(pin.clone()).py_repr()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    Err(ProfileError(format!(
        "Profile {} pins effect(s) to unknown routing band(s): {pins}. Valid band names: {valid_list}",
        name_repr(profile_name),
    )))
}

/// `_settings_from_raw`: builds the validated [`ProfileSettings`] record
/// from *raw* (the whole parsed-and-schema-checked profile document,
/// already defaulted to an empty dict if the file was empty).
pub fn settings_from_raw(raw: Value, name: &str, path: &Path) -> ProfileSettings {
    let dict: Dict = raw.as_dict().cloned().unwrap_or_default();

    let adapter = dict_get(&dict, "adapter")
        .filter(|v| !v.is_none())
        .map(Value::py_str);
    let model = dict_get(&dict, "model")
        .filter(|v| !v.is_none())
        .map(Value::py_str);
    let out = dict_get(&dict, "out")
        .filter(|v| !v.is_none())
        .map(Value::py_str);

    let inputs = match dict_get(&dict, "inputs") {
        Some(Value::Dict(d)) if !d.is_empty() => d.clone(),
        _ => Dict::new(),
    };

    let persistence = match dict_get(&dict, "persistence") {
        Some(Value::Dict(d)) => Some(d.clone()),
        _ => None,
    };

    let mut effects: IndexMap<String, Dict> = IndexMap::new();
    if let Some(Value::Dict(d)) = dict_get(&dict, "effects") {
        if !d.is_empty() {
            for (k, v) in d {
                if let Value::Dict(vd) = v {
                    effects.insert(k.py_str(), vd.clone());
                }
            }
        }
    }

    ProfileSettings {
        name: name.to_string(),
        path: path.to_path_buf(),
        adapter,
        model,
        out,
        inputs,
        effects,
        persistence,
        raw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict_from_pairs(pairs: Vec<(&str, Value)>) -> Dict {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        d
    }

    fn effect(effect_type: &str, name: Option<&str>, extra: Vec<(&str, Value)>) -> Value {
        let mut pairs = vec![("type", Value::Str(effect_type.to_string()))];
        if let Some(n) = name {
            pairs.push(("name", Value::Str(n.to_string())));
        }
        pairs.extend(extra);
        Value::Dict(dict_from_pairs(pairs))
    }

    fn doc_with_effects(effects: Vec<Value>) -> Value {
        Value::Dict(dict_from_pairs(vec![("effects", Value::List(effects))]))
    }

    #[test]
    fn profile_name_from_path_strips_exactly_one_trailing_suffix() {
        assert_eq!(
            profile_name_from_path(Path::new("profiles/a.yml.yml")),
            "a.yml"
        );
        assert_eq!(profile_name_from_path(Path::new("profiles/p.YML")), "p.YML");
        assert_eq!(profile_name_from_path(Path::new("profiles/p")), "p");
        assert_eq!(profile_name_from_path(Path::new("profiles/p.yaml")), "p");
    }

    #[test]
    fn collect_effect_paths_named_dynamic_nests_children() {
        let doc = doc_with_effects(vec![effect(
            "dynamic",
            Some("d"),
            vec![(
                "effects",
                Value::List(vec![effect("tool", Some("t"), vec![])]),
            )],
        )]);
        let paths = collect_orchestration_effect_paths(&doc);
        assert!(paths.contains("d"));
        assert!(paths.contains("d.t"));
    }

    #[test]
    fn collect_effect_paths_unnamed_dynamic_contributes_no_segment() {
        let doc = doc_with_effects(vec![effect(
            "dynamic",
            None,
            vec![(
                "effects",
                Value::List(vec![effect("tool", Some("t"), vec![])]),
            )],
        )]);
        let paths = collect_orchestration_effect_paths(&doc);
        assert!(paths.contains("t"));
        assert_eq!(paths.len(), 1);
    }

    #[test]
    fn collect_condition_paths_for_named_if_and_loop() {
        let doc = doc_with_effects(vec![
            effect(
                "if",
                Some("c"),
                vec![("then", Value::List(vec![effect("tool", Some("t"), vec![])]))],
            ),
            effect(
                "loop",
                Some("l"),
                vec![("body", Value::List(vec![effect("tool", Some("u"), vec![])]))],
            ),
        ]);
        let conditions = collect_orchestration_condition_paths(&doc);
        assert!(conditions.contains("c.if"));
        assert!(conditions.contains("c.condition"));
        assert!(conditions.contains("l.while"));
        assert!(conditions.contains("l.condition"));
    }

    #[test]
    fn finally_at_top_level_is_never_walked() {
        let mut dict = dict_from_pairs(vec![(
            "effects",
            Value::List(vec![effect("tool", Some("t"), vec![])]),
        )]);
        dict.insert(
            Value::Str("finally".to_string()),
            Value::List(vec![effect("tool", Some("f"), vec![])]),
        );
        let doc = Value::Dict(dict);
        let paths = collect_orchestration_effect_paths(&doc);
        assert!(paths.contains("t"));
        assert!(!paths.contains("f"));
    }

    #[test]
    fn validate_effect_paths_rejects_a_condition_target() {
        let doc = doc_with_effects(vec![effect(
            "if",
            Some("c"),
            vec![("then", Value::List(vec![effect("tool", Some("t"), vec![])]))],
        )]);
        let mut effects_dict = Dict::new();
        effects_dict.insert(
            Value::Str("c.if".to_string()),
            Value::Dict(dict_from_pairs(vec![("enabled", Value::Bool(false))])),
        );
        let err = validate_effect_paths(&Value::Dict(effects_dict), &doc, "demo").unwrap_err();
        assert!(err.0.contains("targets condition path(s): c.if"));
        assert!(err.0.contains("c: {enabled: false}"));
    }

    #[test]
    fn validate_effect_paths_rejects_an_unknown_path() {
        let doc = doc_with_effects(vec![effect("tool", Some("t"), vec![])]);
        let mut effects_dict = Dict::new();
        effects_dict.insert(
            Value::Str("bogus".to_string()),
            Value::Dict(dict_from_pairs(vec![("enabled", Value::Bool(false))])),
        );
        let err = validate_effect_paths(&Value::Dict(effects_dict), &doc, "demo").unwrap_err();
        assert_eq!(
            err.0,
            "Profile 'demo' references unknown effect path(s): bogus. Valid effect paths: t"
        );
    }

    #[test]
    fn validate_effect_paths_empty_orchestration_names_no_effects() {
        let doc = doc_with_effects(vec![]);
        let mut effects_dict = Dict::new();
        effects_dict.insert(
            Value::Str("bogus".to_string()),
            Value::Dict(dict_from_pairs(vec![("enabled", Value::Bool(false))])),
        );
        let err = validate_effect_paths(&Value::Dict(effects_dict), &doc, "demo").unwrap_err();
        assert!(err.0.ends_with("(orchestration defines no named effects)"));
    }

    #[test]
    fn validate_effect_paths_accepts_a_known_path() {
        let doc = doc_with_effects(vec![effect("tool", Some("t"), vec![])]);
        let mut effects_dict = Dict::new();
        effects_dict.insert(
            Value::Str("t".to_string()),
            Value::Dict(dict_from_pairs(vec![("enabled", Value::Bool(false))])),
        );
        assert!(validate_effect_paths(&Value::Dict(effects_dict), &doc, "demo").is_ok());
    }

    #[test]
    fn validate_effect_paths_non_string_unknown_key_joins_with_type_error_text() {
        let doc = doc_with_effects(vec![]);
        let mut effects_dict = Dict::new();
        effects_dict.insert(
            Value::Int(1i64.into()),
            Value::Dict(dict_from_pairs(vec![("enabled", Value::Bool(false))])),
        );
        let err = validate_effect_paths(&Value::Dict(effects_dict), &doc, "demo").unwrap_err();
        assert!(
            err.0
                .contains("sequence item 0: expected str instance, int found")
        );
    }

    #[test]
    fn validate_effect_paths_mixed_type_unknown_keys_raise_sort_type_error() {
        let doc = doc_with_effects(vec![]);
        let mut effects_dict = Dict::new();
        effects_dict.insert(
            Value::Str("a".to_string()),
            Value::Dict(dict_from_pairs(vec![("enabled", Value::Bool(false))])),
        );
        effects_dict.insert(
            Value::Int(1i64.into()),
            Value::Dict(dict_from_pairs(vec![("enabled", Value::Bool(false))])),
        );
        let err = validate_effect_paths(&Value::Dict(effects_dict), &doc, "demo").unwrap_err();
        assert_eq!(
            err.0,
            "'<' not supported between instances of 'int' and 'str'"
        );
    }

    #[test]
    fn validate_effect_paths_naive_and_aware_datetime_keys_raise_cpythons_own_text() {
        use chrono::{FixedOffset, NaiveDate, NaiveDateTime};
        let doc = doc_with_effects(vec![]);
        let mut effects_dict = Dict::new();
        let d = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        let naive = NaiveDateTime::new(d, Default::default());
        effects_dict.insert(
            Value::DateTime(naive, None),
            Value::Dict(dict_from_pairs(vec![("enabled", Value::Bool(false))])),
        );
        effects_dict.insert(
            Value::DateTime(naive, Some(FixedOffset::east_opt(0).unwrap())),
            Value::Dict(dict_from_pairs(vec![("enabled", Value::Bool(false))])),
        );
        let err = validate_effect_paths(&Value::Dict(effects_dict), &doc, "demo").unwrap_err();
        assert_eq!(
            err.0,
            "can't compare offset-naive and offset-aware datetimes"
        );
    }

    #[test]
    fn validate_effect_paths_date_and_datetime_keys_raise_cpythons_own_text() {
        use chrono::{NaiveDate, NaiveDateTime};
        let doc = doc_with_effects(vec![]);
        let mut effects_dict = Dict::new();
        let d = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        effects_dict.insert(
            Value::Date(d),
            Value::Dict(dict_from_pairs(vec![("enabled", Value::Bool(false))])),
        );
        effects_dict.insert(
            Value::DateTime(NaiveDateTime::new(d, Default::default()), None),
            Value::Dict(dict_from_pairs(vec![("enabled", Value::Bool(false))])),
        );
        let err = validate_effect_paths(&Value::Dict(effects_dict), &doc, "demo").unwrap_err();
        assert_eq!(err.0, "can't compare datetime.datetime to datetime.date");
    }

    #[test]
    fn settings_from_raw_collapses_stringified_key_collisions_first_position_last_value() {
        let mut effects_dict = Dict::new();
        effects_dict.insert(
            Value::Int(1i64.into()),
            Value::Dict(dict_from_pairs(vec![("model", Value::Str("a".into()))])),
        );
        effects_dict.insert(
            Value::Str("1".to_string()),
            Value::Dict(dict_from_pairs(vec![("model", Value::Str("b".into()))])),
        );
        effects_dict.insert(
            Value::Str("z".to_string()),
            Value::Dict(dict_from_pairs(vec![("model", Value::Str("c".into()))])),
        );
        let raw = Value::Dict(dict_from_pairs(vec![(
            "effects",
            Value::Dict(effects_dict),
        )]));
        let settings = settings_from_raw(raw, "demo", Path::new("profiles/demo.yml"));
        let keys: Vec<&String> = settings.effects.keys().collect();
        assert_eq!(keys, vec!["1", "z"]);
        assert_eq!(
            settings
                .effects
                .get("1")
                .unwrap()
                .get(&Value::Str("model".into())),
            Some(&Value::Str("b".into()))
        );
    }

    #[test]
    fn settings_from_raw_drops_non_dict_effects_values() {
        let mut effects_dict = Dict::new();
        effects_dict.insert(Value::Str("a".to_string()), Value::Str("not a dict".into()));
        let raw = Value::Dict(dict_from_pairs(vec![(
            "effects",
            Value::Dict(effects_dict),
        )]));
        let settings = settings_from_raw(raw, "demo", Path::new("profiles/demo.yml"));
        assert!(settings.effects.is_empty());
    }

    #[test]
    fn settings_from_raw_non_dict_persistence_is_none() {
        let raw = Value::Dict(dict_from_pairs(vec![("persistence", Value::Bool(false))]));
        let settings = settings_from_raw(raw, "demo", Path::new("profiles/demo.yml"));
        assert_eq!(settings.persistence, None);
    }

    #[test]
    fn validate_profile_routing_pins_accepts_a_matching_named_band() {
        let routing = RoutingSettings {
            enabled: true,
            bands: vec![crate::complexity::ComplexityBand {
                model: "big".to_string(),
                max: None,
                name: Some("heavy".to_string()),
            }],
            ..RoutingSettings::default()
        };
        let mut effects = IndexMap::new();
        effects.insert(
            "t".to_string(),
            dict_from_pairs(vec![("routing", Value::Str("heavy".to_string()))]),
        );
        assert!(validate_profile_routing_pins(&effects, &routing, "demo").is_ok());
    }

    #[test]
    fn validate_profile_routing_pins_rejects_an_unknown_band() {
        let routing = RoutingSettings::default();
        let mut effects = IndexMap::new();
        effects.insert(
            "t".to_string(),
            dict_from_pairs(vec![("routing", Value::Str("heavy".to_string()))]),
        );
        let err = validate_profile_routing_pins(&effects, &routing, "demo").unwrap_err();
        assert_eq!(
            err.0,
            "Profile 'demo' pins effect(s) to unknown routing band(s): 't' -> 'heavy'. \
             Valid band names: (no named bands configured)"
        );
    }

    #[test]
    fn validate_profile_routing_pins_skips_a_false_opt_out() {
        let routing = RoutingSettings::default();
        let mut effects = IndexMap::new();
        effects.insert(
            "t".to_string(),
            dict_from_pairs(vec![("routing", Value::Bool(false))]),
        );
        assert!(validate_profile_routing_pins(&effects, &routing, "demo").is_ok());
    }
}
