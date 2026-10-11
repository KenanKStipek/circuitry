//! The pure half of `cli/runtime_shim.py`'s preflight (issue #495,
//! lane R): classification (`classify_preflight_results`, with
//! Python's own hard/soft split) and the two formatters
//! (`format_preflight_errors`/`format_preflight_warnings`). The driver
//! itself -- building each referenced adapter/tool from the config's
//! own runtime block and awaiting `check()` -- cannot live in this
//! crate (it depends only on `electricity-value`/`electricity-json`,
//! and can't reach `electricity-vm`'s `Adapter` trait without a
//! dependency cycle); that half is `electricity` (lib)'s own
//! `src/preflight.rs`.
//!
//! [`CheckResult`] mirrors `electricity_vm::adapter::CheckResult`
//! (itself mirroring `preflight.py::CheckResult`) rather than reusing
//! it, for the same dependency-direction reason.

use crate::allowlist::{format_list, walk_orchestration_refs};
use electricity_value::Value;
use std::collections::HashMap;

/// `preflight.py::CheckResult`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckResult {
    pub ok: bool,
    pub missing: Vec<String>,
    pub message: Option<String>,
}

/// `cli/allowlist.py::AdapterUsage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterUsage {
    pub effect_name: Option<String>,
    pub on_error: String,
}

fn get<'a>(dict: &'a electricity_value::Dict, key: &str) -> Option<&'a Value> {
    dict.get(&Value::Str(key.to_string()))
}

fn is_truthy(value: Option<&Value>) -> bool {
    crate::util::is_truthy(value)
}

/// `allowlist.py::_condition_mode`: `mode` on a condition dict,
/// defaulting to `"model"` when absent, `"cel"` only when it is
/// exactly (case/space-insensitively) `"cel"`.
fn condition_mode(condition: Option<&Value>) -> &'static str {
    let Some(dict) = condition.and_then(Value::as_dict) else {
        return "model";
    };
    let mode = get(dict, "mode")
        .filter(|v| is_truthy(Some(v)))
        .map(Value::py_str)
        .unwrap_or_default();
    if electricity_value::pycompat::py_strip(&mode).to_lowercase() == "cel" {
        "cel"
    } else {
        "model"
    }
}

fn normalized_on_error(effect: &electricity_value::Dict, valid: &[&str]) -> String {
    match get(effect, "on_error").and_then(Value::as_str) {
        Some(value) if valid.contains(&value) => value.to_string(),
        _ => "fail".to_string(),
    }
}

/// `allowlist.py::collect_adapter_usages`: for each adapter name the
/// orchestration references, every effect that uses it and that
/// effect's `on_error` handling. Walks only `effects` (never `steps`)
/// at the root, inside `dynamic`/`reflector`, plus the root's and
/// `dynamic`'s own `finally:` -- a different, narrower traversal than
/// [`walk_orchestration_refs`]'s own (that one also reads `steps` and
/// never descends into a model-mode `if`/`while` condition as a usage
/// of its own).
pub fn collect_adapter_usages(document: &Value) -> HashMap<String, Vec<AdapterUsage>> {
    let mut usages = HashMap::new();
    let Some(orch) = document.as_dict() else {
        return usages;
    };
    let default_adapter = get(orch, "adapter")
        .and_then(Value::as_str)
        .map(electricity_value::pycompat::py_strip)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    walk_effects_usages(
        get(orch, "effects"),
        default_adapter.as_deref(),
        &mut usages,
    );
    walk_effects_usages(
        get(orch, "finally"),
        default_adapter.as_deref(),
        &mut usages,
    );
    usages
}

fn effect_name_of(effect: &electricity_value::Dict) -> Option<String> {
    get(effect, "name")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn walk_effects_usages(
    effects: Option<&Value>,
    default_adapter: Option<&str>,
    usages: &mut HashMap<String, Vec<AdapterUsage>>,
) {
    let Some(Value::List(items)) = effects else {
        return;
    };
    for effect in items {
        let Some(effect) = effect.as_dict() else {
            continue;
        };
        let etype = get(effect, "type").and_then(Value::as_str).unwrap_or("");
        let effect_name = effect_name_of(effect);

        match etype {
            "prompt" => {
                let on_error = normalized_on_error(effect, &["fail", "skip", "continue"]);
                let primary = get(effect, "provider").and_then(Value::as_str);
                let mut adapter_name =
                    primary.and_then(crate::allowlist::provider_token_to_adapter);
                if adapter_name.is_none() {
                    adapter_name = default_adapter.map(str::to_string);
                }
                if let Some(name) = adapter_name {
                    usages.entry(name).or_default().push(AdapterUsage {
                        effect_name: effect_name.clone(),
                        on_error: on_error.clone(),
                    });
                }
                if let Some(Value::List(fallbacks)) = get(effect, "provider_fallbacks") {
                    for token in fallbacks {
                        if let Some(token) = token.as_str() {
                            if let Some(name) = crate::allowlist::provider_token_to_adapter(token) {
                                usages.entry(name).or_default().push(AdapterUsage {
                                    effect_name: effect_name.clone(),
                                    on_error: on_error.clone(),
                                });
                            }
                        }
                    }
                }
            }
            "dynamic" => {
                walk_effects_usages(get(effect, "effects"), default_adapter, usages);
                walk_effects_usages(get(effect, "finally"), default_adapter, usages);
            }
            "if" | "conditional" => {
                if condition_mode(get(effect, "if")) == "model" {
                    if let Some(name) = default_adapter {
                        let on_error = normalized_on_error(effect, &["fail", "skip", "continue"]);
                        usages
                            .entry(name.to_string())
                            .or_default()
                            .push(AdapterUsage {
                                effect_name: effect_name.clone(),
                                on_error,
                            });
                    }
                }
                walk_effects_usages(get(effect, "then"), default_adapter, usages);
                walk_effects_usages(get(effect, "else"), default_adapter, usages);
            }
            "loop" => {
                let while_def = get(effect, "while");
                if while_def.is_some() && condition_mode(while_def) == "model" {
                    if let Some(name) = default_adapter {
                        let on_error = normalized_on_error(effect, &["fail", "break", "continue"]);
                        usages
                            .entry(name.to_string())
                            .or_default()
                            .push(AdapterUsage {
                                effect_name: effect_name.clone(),
                                on_error,
                            });
                    }
                }
                walk_effects_usages(get(effect, "body"), default_adapter, usages);
            }
            "reflector" => {
                walk_effects_usages(get(effect, "effects"), default_adapter, usages);
            }
            _ => {}
        }
    }
}

/// `allowlist.py::is_hard_adapter_dependency`: soft only when there is
/// at least one recorded usage and every one of them tolerates failure.
pub fn is_hard_adapter_dependency(name: &str, usages: &HashMap<String, Vec<AdapterUsage>>) -> bool {
    match usages.get(name) {
        None => true,
        Some(items) if items.is_empty() => true,
        Some(items) => items.iter().any(|u| u.on_error == "fail"),
    }
}

/// `allowlist.py::skippable_effect_names`.
pub fn skippable_effect_names(
    name: &str,
    usages: &HashMap<String, Vec<AdapterUsage>>,
) -> Vec<String> {
    usages
        .get(name)
        .into_iter()
        .flatten()
        .filter(|u| u.on_error != "fail")
        .map(|u| effect_name_or_unnamed(u.effect_name.as_deref()))
        .collect()
}

/// `allowlist.py::hard_effect_names`.
pub fn hard_effect_names(name: &str, usages: &HashMap<String, Vec<AdapterUsage>>) -> Vec<String> {
    usages
        .get(name)
        .into_iter()
        .flatten()
        .filter(|u| u.on_error == "fail")
        .map(|u| effect_name_or_unnamed(u.effect_name.as_deref()))
        .collect()
}

/// `u.effect_name or "<unnamed>"` (`allowlist.py:255`, `:265`): Python's
/// `or` falls through on an empty string too, not only `None` -- the
/// schema's `NamePattern` blocks an effect from actually naming itself
/// `""` today, so this only matters if that ever changes.
fn effect_name_or_unnamed(effect_name: Option<&str>) -> String {
    match effect_name {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "<unnamed>".to_string(),
    }
}

/// `runtime_shim.py::classify_preflight_results`: splits raw preflight
/// results into `(hard, soft)`. *known_adapter_names* is Circuitry's
/// own full registry (`electricity_vm::adapter_names::ADAPTER_NAMES`,
/// passed in by the caller so this crate never depends on
/// `electricity-vm`).
pub type PreflightResult = (String, CheckResult);

/// [`classify_preflight_results`]'s own `(hard, soft)` return pair.
pub type ClassifiedPreflightResults = (Vec<PreflightResult>, Vec<PreflightResult>);

pub fn classify_preflight_results(
    document: &Value,
    results: Vec<PreflightResult>,
    known_adapter_names: &[&str],
) -> ClassifiedPreflightResults {
    let usages = collect_adapter_usages(document);
    let mut hard = Vec::new();
    let mut soft = Vec::new();
    for (label, result) in results {
        if result.ok || !label.starts_with("adapter:") {
            hard.push((label, result));
            continue;
        }
        let adapter_name = label["adapter:".len()..].to_string();
        let normalized = electricity_value::pycompat::py_strip(&adapter_name).to_lowercase();
        if !known_adapter_names.contains(&normalized.as_str())
            || is_hard_adapter_dependency(&adapter_name, &usages)
        {
            let required_by = hard_effect_names(&adapter_name, &usages);
            if !required_by.is_empty() {
                let mut message = format!(
                    "adapter '{adapter_name}' unavailable; required by effects {}",
                    format_list(&required_by)
                );
                if let Some(msg) = non_empty_message(&result.message) {
                    message.push_str(&format!(" ({msg})"));
                }
                hard.push((
                    label,
                    CheckResult {
                        ok: false,
                        missing: result.missing,
                        message: Some(message),
                    },
                ));
            } else {
                hard.push((label, result));
            }
            continue;
        }
        let effects = skippable_effect_names(&adapter_name, &usages);
        let mut message = format!(
            "adapter '{adapter_name}' unavailable; effects {} will skip",
            format_list(&effects)
        );
        if let Some(msg) = non_empty_message(&result.message) {
            message.push_str(&format!(" ({msg})"));
        }
        soft.push((
            label,
            CheckResult {
                ok: false,
                missing: result.missing,
                message: Some(message),
            },
        ));
    }
    (hard, soft)
}

/// `runtime_shim.py::format_preflight_errors`.
pub fn format_preflight_errors(results: &[(String, CheckResult)]) -> Vec<String> {
    results
        .iter()
        .filter(|(_, r)| !r.ok)
        .map(|(label, r)| {
            let mut parts = vec![format!("{label}: not ready")];
            if !r.missing.is_empty() {
                parts.push(format!("missing {}", format_list(&r.missing)));
            }
            if let Some(message) = non_empty_message(&r.message) {
                parts.push(message.to_string());
            }
            parts.join(" \u{2014} ")
        })
        .collect()
}

/// `runtime_shim.py::format_preflight_warnings`.
pub fn format_preflight_warnings(results: &[(String, CheckResult)]) -> Vec<String> {
    results
        .iter()
        .map(|(label, r)| {
            let mut message = non_empty_message(&r.message)
                .map(str::to_string)
                .unwrap_or_else(|| format!("{label}: not ready"));
            if !r.missing.is_empty() {
                message.push_str(&format!(" (missing {})", format_list(&r.missing)));
            }
            message
        })
        .collect()
}

/// `r.message` (truthy, `runtime_shim.py`'s own `if r.message:`/`r.message
/// or ...`): Python never treats an empty string as present. No real
/// check constructs `CheckResult { message: Some(String::new()), .. }`
/// today; this only matters if one ever does.
fn non_empty_message(message: &Option<String>) -> Option<&str> {
    message.as_deref().filter(|m| !m.is_empty())
}

/// `walk_orchestration_refs`, re-exported under this module so lane
/// R's own driver (`electricity` lib's `src/preflight.rs`) can build
/// preflight's own `(adapter_refs, tool_refs)` pair without a second
/// public path into [`crate::allowlist`].
pub fn preflight_refs(
    document: &Value,
    include_document_adapter: bool,
) -> (Vec<String>, Vec<String>) {
    let (adapters, tools) = walk_orchestration_refs(document, include_document_adapter);
    (adapters.into_iter().collect(), tools.into_iter().collect())
}

/// `allowlist.py::_provider_token_to_adapter`, re-exported under this
/// module for the same reason [`preflight_refs`] is -- lane R's own
/// driver (`image_asset_warnings`) needs it too.
pub fn provider_token_to_adapter(token: &str) -> Option<String> {
    crate::allowlist::provider_token_to_adapter(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    fn doc(effects: Vec<Value>) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("effects".to_string()), Value::List(effects));
        Value::Dict(dict)
    }

    fn doc_with_adapter(adapter: &str, effects: Vec<Value>) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("adapter".to_string()), Value::from(adapter));
        dict.insert(Value::Str("effects".to_string()), Value::List(effects));
        Value::Dict(dict)
    }

    fn prompt(name: &str, on_error: Option<&str>) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("type".to_string()), Value::from("prompt"));
        dict.insert(Value::Str("name".to_string()), Value::from(name));
        if let Some(oe) = on_error {
            dict.insert(Value::Str("on_error".to_string()), Value::from(oe));
        }
        Value::Dict(dict)
    }

    fn ok() -> CheckResult {
        CheckResult {
            ok: true,
            ..Default::default()
        }
    }

    fn failed(message: &str) -> CheckResult {
        CheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some(message.to_string()),
        }
    }

    #[test]
    fn an_ok_result_is_always_hard() {
        let document = doc(vec![]);
        let (hard, soft) = classify_preflight_results(
            &document,
            vec![("adapter:scripted".to_string(), ok())],
            &["scripted"],
        );
        assert_eq!(hard.len(), 1);
        assert!(soft.is_empty());
    }

    #[test]
    fn a_tool_failure_is_always_hard() {
        let document = doc(vec![]);
        let (hard, soft) = classify_preflight_results(
            &document,
            vec![("tool:awk".to_string(), failed("not ready"))],
            &[],
        );
        assert_eq!(hard.len(), 1);
        assert!(soft.is_empty());
    }

    #[test]
    fn an_unknown_adapter_name_is_hard_even_with_no_usages() {
        let document = doc(vec![]);
        let (hard, soft) = classify_preflight_results(
            &document,
            vec![("adapter:bogus".to_string(), failed("oops"))],
            &[],
        );
        assert_eq!(hard.len(), 1);
        assert!(soft.is_empty());
        assert_eq!(hard[0].1.message.as_deref(), Some("oops"));
    }

    #[test]
    fn no_usages_of_a_known_adapter_is_hard() {
        let document = doc(vec![]);
        let (hard, soft) = classify_preflight_results(
            &document,
            vec![("adapter:scripted".to_string(), failed("oops"))],
            &["scripted"],
        );
        assert_eq!(hard.len(), 1);
        assert!(soft.is_empty());
    }

    #[test]
    fn a_mixed_fail_and_skip_usage_is_hard_and_names_the_failing_effect() {
        let document = doc_with_adapter(
            "scripted",
            vec![prompt("a", Some("fail")), prompt("b", Some("skip"))],
        );
        let (hard, soft) = classify_preflight_results(
            &document,
            vec![("adapter:scripted".to_string(), failed("down"))],
            &["scripted"],
        );
        assert_eq!(hard.len(), 1);
        assert!(soft.is_empty());
        assert_eq!(
            hard[0].1.message.as_deref(),
            Some("adapter 'scripted' unavailable; required by effects ['a'] (down)")
        );
    }

    #[test]
    fn an_all_skip_usage_is_soft() {
        let document = doc_with_adapter(
            "scripted",
            vec![prompt("a", Some("skip")), prompt("b", Some("continue"))],
        );
        let (hard, soft) = classify_preflight_results(
            &document,
            vec![("adapter:scripted".to_string(), failed("down"))],
            &["scripted"],
        );
        assert!(hard.is_empty());
        assert_eq!(soft.len(), 1);
        assert_eq!(
            soft[0].1.message.as_deref(),
            Some("adapter 'scripted' unavailable; effects ['a', 'b'] will skip (down)")
        );
    }

    #[test]
    fn an_unnamed_effect_reports_unnamed() {
        let document = doc_with_adapter("scripted", vec![prompt("", Some("skip"))]);
        let (hard, soft) = classify_preflight_results(
            &document,
            vec![("adapter:scripted".to_string(), failed("down"))],
            &["scripted"],
        );
        assert!(hard.is_empty());
        assert_eq!(
            soft[0].1.message.as_deref(),
            Some("adapter 'scripted' unavailable; effects ['<unnamed>'] will skip (down)")
        );
    }

    fn model_if(name: &str, on_error: Option<&str>) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("type".to_string()), Value::from("if"));
        dict.insert(Value::Str("name".to_string()), Value::from(name));
        dict.insert(Value::Str("if".to_string()), Value::Dict(Dict::new()));
        if let Some(oe) = on_error {
            dict.insert(Value::Str("on_error".to_string()), Value::from(oe));
        }
        Value::Dict(dict)
    }

    fn model_while(name: &str, on_error: Option<&str>) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("type".to_string()), Value::from("loop"));
        dict.insert(Value::Str("name".to_string()), Value::from(name));
        dict.insert(Value::Str("while".to_string()), Value::Dict(Dict::new()));
        if let Some(oe) = on_error {
            dict.insert(Value::Str("on_error".to_string()), Value::from(oe));
        }
        Value::Dict(dict)
    }

    #[test]
    fn a_model_mode_if_condition_is_a_usage_of_the_document_adapter() {
        let document = doc_with_adapter("scripted", vec![model_if("cond", Some("skip"))]);
        let usages = collect_adapter_usages(&document);
        assert_eq!(skippable_effect_names("scripted", &usages), vec!["cond"]);
    }

    #[test]
    fn a_model_mode_while_condition_is_a_usage_of_the_document_adapter() {
        let document = doc_with_adapter("scripted", vec![model_while("loop1", Some("break"))]);
        let usages = collect_adapter_usages(&document);
        assert_eq!(skippable_effect_names("scripted", &usages), vec!["loop1"]);
    }

    #[test]
    fn a_steps_only_document_has_no_usages() {
        let mut dict = Dict::new();
        dict.insert(
            Value::Str("steps".to_string()),
            Value::List(vec![prompt("a", None)]),
        );
        let document = Value::Dict(dict);
        let usages = collect_adapter_usages(&document);
        assert!(usages.is_empty());
    }

    #[test]
    fn format_preflight_errors_joins_with_an_em_dash() {
        let errors = format_preflight_errors(&[(
            "adapter:scripted".to_string(),
            CheckResult {
                ok: false,
                missing: vec!["env:OPENAI_API_KEY".to_string()],
                message: Some("down".to_string()),
            },
        )]);
        assert_eq!(
            errors,
            vec![
                "adapter:scripted: not ready \u{2014} missing ['env:OPENAI_API_KEY'] \u{2014} down"
                    .to_string()
            ]
        );
    }

    #[test]
    fn format_preflight_errors_skips_ok_results() {
        assert!(format_preflight_errors(&[("adapter:scripted".to_string(), ok())]).is_empty());
    }

    #[test]
    fn format_preflight_warnings_falls_back_to_not_ready() {
        let warnings = format_preflight_warnings(&[(
            "adapter:scripted".to_string(),
            CheckResult {
                ok: false,
                missing: vec![],
                message: None,
            },
        )]);
        assert_eq!(warnings, vec!["adapter:scripted: not ready".to_string()]);
    }

    /// Python's `r.message or f"{label}: not ready"` treats an empty
    /// string the same as `None` -- so does this, even though no real
    /// check constructs `Some(String::new())` today.
    #[test]
    fn format_preflight_warnings_treats_an_empty_message_as_absent() {
        let warnings = format_preflight_warnings(&[(
            "adapter:scripted".to_string(),
            CheckResult {
                ok: false,
                missing: vec![],
                message: Some(String::new()),
            },
        )]);
        assert_eq!(warnings, vec!["adapter:scripted: not ready".to_string()]);
    }

    /// Python's `if r.message:` is also falsy on an empty string.
    #[test]
    fn format_preflight_errors_treats_an_empty_message_as_absent() {
        let errors = format_preflight_errors(&[(
            "adapter:scripted".to_string(),
            CheckResult {
                ok: false,
                missing: vec![],
                message: Some(String::new()),
            },
        )]);
        assert_eq!(errors, vec!["adapter:scripted: not ready".to_string()]);
    }
}
