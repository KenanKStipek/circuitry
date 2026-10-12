//! Decomposition's pure helpers (`core/decompose.py`), issue #521
//! (M1 lane L2a of #448). This file ports every part of runtime
//! decomposition that needs no `prompt`-effect wiring: the route-up
//! model, the planner's input/output shapes, `_validate_plan`, the
//! `meta.decomposition` record builders, the plan's identity and
//! cycle text, and the decomposition child's seed state.
//!
//! Every function here is `pub` so nothing is dead code before lane
//! L2b (#522) -- which needs them unchanged -- calls them from the
//! `prompt` effect. Nothing in this crate calls them yet.
//!
//! `_check_capability_ceiling` (`core/decompose.py:582-607`) is not
//! ported (#448's Settled question 9): electricity never installs a
//! capability ceiling, so that check is always `[]` here, and
//! [`validate_plan`] never calls an equivalent of it at all.

use crate::exec::use_::{allowlist_names, resolve_dot_path, sha256_hex, validate_inline_yaml};
use electricity_config::{ComplexitySettings, DecompositionSettings};
use electricity_value::pycompat::py_strip;
use electricity_value::{Dict, Value};
use electricity_yaml::{DumpError, DumpOptions};

/// Circuitry's own decomposition planner
/// (`src/circuitry/curation/agents/decompose.yml`), copied byte for
/// byte by `electricity/scripts/generate_decompose_planner.py
/// --check`. `core/decompose.py::_planner_path`'s default -- the path
/// an override (`_decomposition_planner_path`) is not ported here,
/// since nothing in this lane runs the planner yet (lane L2b's own
/// concern).
pub const PLANNER_YAML: &str = include_str!("decompose_planner.yml");

/// `core/decompose.py::_DEFAULT_RESULT_PATH`.
pub const DEFAULT_RESULT_PATH: &str = "prime.merge.value";

/// `core/decompose.py::_CANONICAL_PLANNER_OUTPUTS`, Python's own
/// dict order.
pub const PLANNER_OUTPUTS: [(&str, &str); 5] = [
    ("say", "prime.plan.value.say"),
    ("chunks", "prime.plan.value.chunks"),
    ("yaml", "prime.check.value.yaml"),
    ("done", "prime.done.value"),
    ("result_path", "prime.result_path.value"),
];

/// `core/state_ns.py::NAMESPACES`.
const NAMESPACES: [&str; 3] = ["input", "prime", "runtime"];

/// `core/decompose.py::_Plan` -- the planner run's outputs, read
/// through its interface.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub say: Value,
    pub chunks: Value,
    pub yaml: Option<String>,
    pub done: Value,
    pub result_path: String,
}

/// What a decomposition attempt decided to do next, once its own
/// `meta.decomposition` record is built -- `core/decompose.py::
/// DecompositionResult`'s three postures (`succeeded`/`fail`/
/// neither), reshaped as an enum so a caller can't construct an
/// invalid combination (e.g. both `succeeded` and `fail`) the way the
/// Python dataclass's loose boolean fields allowed.
#[derive(Debug, Clone, PartialEq)]
pub enum Posture {
    /// Write *0* at the effect's own path and stop.
    Decomposed(Value),
    /// `on_failure: fail` -- the caller raises
    /// [`failure_text`]`(prompt_name, reason, error)`.
    Fail { reason: &'static str, error: String },
    /// Run the original prompt, on `fallback_model` when set (route
    /// up) or on the already-resolved model when not (run as-is).
    Dispatch { fallback_model: Option<String> },
}

/// `core/decompose.py::DecompositionResult`, reshaped (this module's
/// own doc comment): `meta` always lands at `meta.decomposition` on
/// the effect node.
#[derive(Debug, Clone, PartialEq)]
pub struct DecompositionResult {
    pub meta: Value,
    pub posture: Posture,
}

/// `core/decompose.py::_route_up_model`.
///
/// Returns `None` when routing is off or there are no bands; `None`
/// when *prompt_model* is non-empty and `respect_explicit` is on;
/// otherwise the last (catch-all) band's model. Never reads the
/// run-level lock (`_route_up_model`'s own signature has no such
/// parameter either).
pub fn route_up_model(settings: &ComplexitySettings, prompt_model: Option<&str>) -> Option<String> {
    let routing = &settings.routing;
    if !routing.enabled || routing.bands.is_empty() {
        return None;
    }
    if prompt_model.is_some_and(|model| !model.is_empty()) && routing.respect_explicit {
        return None;
    }
    routing.bands.last().map(|band| band.model.clone())
}

/// `core/decompose.py::_planner_runtime_config`: the planner's own
/// complexity settings, decomposition switched off -- scoring and
/// routing stay as configured. Python replaces the whole `complexity.
/// decomposition` block with `{"enabled": False}`; since this crate
/// already holds *resolved* settings rather than raw config, that is
/// exactly [`DecompositionSettings::default`] with `enabled` forced
/// false.
pub fn planner_settings(settings: &ComplexitySettings) -> ComplexitySettings {
    let mut cloned = settings.clone();
    cloned.decomposition = DecompositionSettings {
        enabled: false,
        ..DecompositionSettings::default()
    };
    cloned
}

/// `core/decompose.py::_describe_inputs`.
pub fn describe_inputs(ctx: &Value) -> String {
    let mut keys: Vec<String> = match ctx
        .as_dict()
        .and_then(|dict| dict.get(&Value::Str("input".to_string())))
        .and_then(Value::as_dict)
    {
        Some(input_ns) => input_ns
            .keys()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        None => ctx
            .as_dict()
            .map(|dict| {
                dict.keys()
                    .filter_map(Value::as_str)
                    .filter(|key| !NAMESPACES.contains(key) && !key.starts_with('_'))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    };
    keys.sort();

    if keys.is_empty() {
        return "(no declared inputs)".to_string();
    }
    let mut dict = Dict::new();
    dict.insert(
        Value::Str("inputs".to_string()),
        Value::List(keys.into_iter().map(Value::Str).collect()),
    );
    electricity_yaml::dump(&Value::Dict(dict), &DumpOptions { sort_keys: false })
        .expect("a flat {inputs: [str, ...]} mapping never exceeds the dumper's depth limit")
}

/// `{"prompt_type": <name>, "schema": <schema or None>}`, dumped --
/// the inline construction inside `core/decompose.py::_run_planner`'s
/// own `"source_output"` entry.
pub fn source_output(prompt_type: &str, schema: Option<&Value>) -> Result<String, DumpError> {
    let mut dict = Dict::new();
    dict.insert(
        Value::Str("prompt_type".to_string()),
        Value::Str(prompt_type.to_string()),
    );
    dict.insert(
        Value::Str("schema".to_string()),
        schema.cloned().unwrap_or(Value::None),
    );
    electricity_yaml::dump(&Value::Dict(dict), &DumpOptions { sort_keys: false })
}

/// `core/decompose.py::_run_planner`'s own `planner_state` dict,
/// wrapped under `{"input": ...}` the way the planner is actually
/// seeded (`initial_state={"input": planner_state}`).
pub fn planner_input(
    source_template: &str,
    ctx: &Value,
    prompt_type: &str,
    schema: Option<&Value>,
    max_chunks: i64,
) -> Result<Value, DumpError> {
    let mut inner = Dict::new();
    inner.insert(
        Value::Str("source_template".to_string()),
        Value::Str(source_template.to_string()),
    );
    inner.insert(
        Value::Str("source_interface".to_string()),
        Value::Str(describe_inputs(ctx)),
    );
    inner.insert(
        Value::Str("source_output".to_string()),
        Value::Str(source_output(prompt_type, schema)?),
    );
    inner.insert(
        Value::Str("max_chunks".to_string()),
        Value::from(max_chunks),
    );
    let mut outer = Dict::new();
    outer.insert(Value::Str("input".to_string()), Value::Dict(inner));
    Ok(Value::Dict(outer))
}

/// `core/decompose.py::_run_planner`'s own read half (`:504-519`,
/// `_planner_outputs`'s canonical half only -- #448's Settled question
/// 11): reads [`PLANNER_OUTPUTS`]' five paths out of *planner_state*
/// with [`resolve_dot_path`].
pub fn read_plan(planner_state: &Value) -> Plan {
    let resolved: Dict = PLANNER_OUTPUTS
        .iter()
        .map(|(name, path)| {
            (
                Value::Str(name.to_string()),
                resolve_dot_path(planner_state, path),
            )
        })
        .collect();
    let get = |name: &str| {
        resolved
            .get(&Value::Str(name.to_string()))
            .cloned()
            .unwrap_or(Value::None)
    };

    let raw_yaml = get("yaml");
    let yaml = raw_yaml.as_str().map(electricity_yaml::clean_yaml_fences);
    let raw_result_path = get("result_path");
    let result_path = match raw_result_path.as_str().map(py_strip) {
        Some(stripped) if !stripped.is_empty() => stripped.to_string(),
        _ => DEFAULT_RESULT_PATH.to_string(),
    };

    Plan {
        say: get("say"),
        chunks: get("chunks"),
        yaml,
        done: get("done"),
        result_path,
    }
}

/// `core/decompose.py::_check_merge_contract`.
fn check_merge_contract(plan: &Plan) -> Vec<String> {
    let yaml = plan.yaml.as_deref().unwrap_or("");
    let parsed = match electricity_yaml::load_yaml_last_key_wins(yaml) {
        Err(err) => return vec![format!("YAML parse error: {err}")],
        Ok(value) => value,
    };
    let Some(dict) = parsed.as_dict() else {
        return vec!["the emitted orchestration is not a YAML mapping".to_string()];
    };
    let parts: Vec<&str> = plan.result_path.split('.').collect();
    if parts.len() < 2 || parts[0] != "prime" {
        return vec![format!(
            "result_path {} does not point into the emitted orchestration's own state \
             (expected 'prime.<effect>...')",
            Value::Str(plan.result_path.clone()).py_repr()
        )];
    }
    let names: std::collections::HashSet<&Value> =
        match dict.get(&Value::Str("effects".to_string())) {
            Some(Value::List(items)) => items
                .iter()
                .filter_map(Value::as_dict)
                .filter_map(|entry| entry.get(&Value::Str("name".to_string())))
                .collect(),
            _ => std::collections::HashSet::new(),
        };
    let segment = Value::Str(parts[1].to_string());
    if !names.contains(&segment) {
        return vec![format!(
            "the emitted orchestration has no top-level effect named {}, so nothing writes \
             result_path {}",
            segment.py_repr(),
            Value::Str(plan.result_path.clone()).py_repr()
        )];
    }
    Vec::new()
}

/// `core/decompose.py::_check_allowlists`.
fn check_allowlists(plan: &Plan, runtime_config: &Value) -> Vec<String> {
    let yaml = plan.yaml.as_deref().unwrap_or("");
    let Ok(parsed) = electricity_yaml::load_yaml_last_key_wins(yaml) else {
        return Vec::new(); // already reported by the schema check
    };
    if parsed.as_dict().is_none() {
        return Vec::new();
    }
    electricity_config::orchestration_denials(
        &parsed,
        allowlist_names(runtime_config, "adapters").as_deref(),
        allowlist_names(runtime_config, "tools").as_deref(),
        true,
        // #448's Settled question 10: `_check_allowlists` passes
        // `include_document_adapter=True`, `cli/allowlist.py::
        // orchestration_denials`'s own default, which decompose
        // doesn't override (unlike a `use` child, which passes
        // `false`).
        true,
    )
}

/// `core/decompose.py::_validate_plan`. `_check_capability_ceiling`
/// is not ported (this module's own doc comment); nothing stands in
/// for it here.
pub fn validate_plan(plan: &Plan, max_chunks: i64, runtime_config: &Value) -> Vec<String> {
    let mut problems = Vec::new();

    let yaml = match &plan.yaml {
        Some(text) if !py_strip(text).is_empty() => text.clone(),
        _ => {
            problems.push("the planner returned no orchestration YAML".to_string());
            return problems;
        }
    };

    if matches!(plan.done, Value::Bool(false)) {
        problems.push(
            "the planner's own done gate is false (its output never validated within budget)"
                .to_string(),
        );
    }

    match &plan.chunks {
        Value::List(items) if !items.is_empty() => {
            if items.len() < 2 {
                problems.push(format!(
                    "the plan has {} chunk(s); a decomposition needs at least 2",
                    items.len()
                ));
            } else if items.len() as i64 > max_chunks {
                problems.push(format!(
                    "the plan has {} chunks; the budget is {max_chunks}",
                    items.len()
                ));
            }
        }
        _ => problems.push("the planner returned no chunk list".to_string()),
    }

    let parsed = match electricity_yaml::load_yaml(&yaml) {
        Err(err) => {
            problems.push(format!("YAML parse error: {err}"));
            return problems;
        }
        Ok(value) => value,
    };
    if let Err(errors) = validate_inline_yaml(&parsed) {
        problems.extend(errors);
        return problems;
    }

    problems.extend(check_merge_contract(plan));
    problems.extend(check_allowlists(plan, runtime_config));
    problems
}

/// `core/decompose.py::maybe_decompose`'s own `base` dict (`:171-180`).
pub fn base_meta(score: f64, settings: &DecompositionSettings, depth: u32) -> Dict {
    let mut dict = Dict::new();
    dict.insert(Value::Str("decomposed".to_string()), Value::Bool(false));
    dict.insert(Value::Str("score".to_string()), Value::Float(score));
    dict.insert(
        Value::Str("threshold".to_string()),
        Value::Float(settings.threshold),
    );
    dict.insert(
        Value::Str("depth".to_string()),
        Value::from(i64::from(depth)),
    );
    dict.insert(
        Value::Str("max_depth".to_string()),
        Value::from(settings.max_depth),
    );
    dict
}

/// `core/decompose.py::maybe_decompose`'s own `base["plan"]`/
/// `base["chunk_count"]`/`base["result_path"]`/`base["yaml"]`
/// (`:191-194`), appended to *base* in that order.
pub fn with_plan(base: &mut Dict, plan: &Plan) {
    let mut plan_dict = Dict::new();
    plan_dict.insert(Value::Str("say".to_string()), plan.say.clone());
    plan_dict.insert(Value::Str("chunks".to_string()), plan.chunks.clone());
    base.insert(Value::Str("plan".to_string()), Value::Dict(plan_dict));

    let chunk_count = match &plan.chunks {
        Value::List(items) => Value::from(items.len() as i64),
        _ => Value::None,
    };
    base.insert(Value::Str("chunk_count".to_string()), chunk_count);
    base.insert(
        Value::Str("result_path".to_string()),
        Value::Str(plan.result_path.clone()),
    );
    base.insert(
        Value::Str("yaml".to_string()),
        match &plan.yaml {
            Some(text) => Value::Str(text.clone()),
            None => Value::None,
        },
    );
}

/// `core/decompose.py::maybe_decompose`'s own max-depth record
/// (`:195-203`). The depth ceiling is never a failure, so this is
/// always [`Posture::Dispatch`].
pub fn max_depth_result(mut base: Dict, fallback_model: Option<String>) -> DecompositionResult {
    let outcome = if fallback_model.is_some() {
        "route_up"
    } else {
        "run_as_is"
    };
    base.insert(
        Value::Str("outcome".to_string()),
        Value::Str(outcome.to_string()),
    );
    base.insert(
        Value::Str("reason".to_string()),
        Value::Str("max_depth".to_string()),
    );
    base.insert(
        Value::Str("fallback_model".to_string()),
        match &fallback_model {
            Some(model) => Value::Str(model.clone()),
            None => Value::None,
        },
    );
    DecompositionResult {
        meta: Value::Dict(base),
        posture: Posture::Dispatch { fallback_model },
    }
}

/// `core/decompose.py::_failure`.
pub fn failure_result(
    mut base: Dict,
    settings: &DecompositionSettings,
    reason: &'static str,
    error: String,
    fallback_model: Option<String>,
    raw_payload: Option<String>,
) -> DecompositionResult {
    let fail = settings.on_failure == "fail";
    let (outcome, fallback_model): (&str, Option<String>) = if fail {
        ("failed", None)
    } else if fallback_model.is_some() {
        ("route_up", fallback_model)
    } else {
        ("run_as_is", fallback_model)
    };

    base.insert(
        Value::Str("outcome".to_string()),
        Value::Str(outcome.to_string()),
    );
    base.insert(
        Value::Str("reason".to_string()),
        Value::Str(reason.to_string()),
    );
    base.insert(Value::Str("error".to_string()), Value::Str(error.clone()));
    base.insert(
        Value::Str("fallback_model".to_string()),
        match &fallback_model {
            Some(model) => Value::Str(model.clone()),
            None => Value::None,
        },
    );
    if let Some(payload) = &raw_payload {
        if !payload.is_empty() {
            base.insert(
                Value::Str("raw_payload".to_string()),
                Value::Str(payload.clone()),
            );
        }
    }

    let posture = if fail {
        Posture::Fail { reason, error }
    } else {
        Posture::Dispatch { fallback_model }
    };
    DecompositionResult {
        meta: Value::Dict(base),
        posture,
    }
}

/// `core/decompose.py::maybe_decompose`'s own success record
/// (`:259-263`): `decomposed` updated in place (its own already-first
/// key -- `Dict`/`IndexMap::insert` preserves an existing key's
/// position, matching Python's `{**base, "decomposed": True}`), then
/// `outcome`/`reason` appended.
pub fn success_result(mut base: Dict, value: Value) -> DecompositionResult {
    base.insert(Value::Str("decomposed".to_string()), Value::Bool(true));
    base.insert(
        Value::Str("outcome".to_string()),
        Value::Str("decomposed".to_string()),
    );
    base.insert(Value::Str("reason".to_string()), Value::None);
    DecompositionResult {
        meta: Value::Dict(base),
        posture: Posture::Decomposed(value),
    }
}

/// `core/prompt.py:654-657`'s own `DecompositionError` text.
pub fn failure_text(prompt_name: &str, reason: &str, error: &str) -> String {
    format!("decomposition of '{prompt_name}' failed ({reason}): {error}")
}

/// `inline:` plus the first 16 hex characters of the SHA-256 digest
/// of *yaml*'s UTF-8 bytes (`core/decompose.py::_execute_plan:693`).
pub fn plan_identity(yaml: &str) -> String {
    let digest = sha256_hex(yaml.as_bytes());
    format!("inline:{}", &digest[..16])
}

/// `core/decompose.py::_execute_plan`'s own cycle check (`:694-698`).
pub fn cycle_error(prompt_name: &str, stack: &[String], identity: &str) -> Option<String> {
    if !stack.iter().any(|entry| entry == identity) {
        return None;
    }
    let mut chain: Vec<&str> = stack.iter().map(String::as_str).collect();
    chain.push(identity);
    Some(format!(
        "decomposition of '{prompt_name}': cycle detected \u{2014} {}",
        chain.join(" \u{2192} ")
    ))
}

/// `core/decompose.py::_execute_plan`'s own child-state copy
/// (`:700-712`): the top-level *string*-keyed entries of *ctx*, deep
/// copied, with `prime.<result_path's second segment>` removed so the
/// merged value can never be stale parent state read back.
pub fn child_seed_state(ctx: &Value, result_path: &str) -> Value {
    let mut seed = Dict::new();
    if let Some(dict) = ctx.as_dict() {
        for (key, value) in dict {
            if matches!(key, Value::Str(_)) {
                seed.insert(key.clone(), value.clone());
            }
        }
    }
    let parts: Vec<&str> = result_path.split('.').collect();
    if parts.len() >= 2 {
        if let Some(Value::Dict(prime)) = seed.get_mut(&Value::Str("prime".to_string())) {
            prime.shift_remove(&Value::Str(parts[1].to_string()));
        }
    }
    Value::Dict(seed)
}

/// `core/decompose.py::_execute_plan`'s own missing-result text
/// (`:729-734`).
pub fn missing_result_error(result_path: &str) -> String {
    format!("the decomposition child completed but left nothing at result_path '{result_path}'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_bytecode::{Supported, first_unsupported};
    use electricity_compiler::DocumentOrigin;
    use electricity_config::{ComplexityBand, RoutingSettings};

    fn dict(pairs: Vec<(&str, Value)>) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(d)
    }

    #[test]
    fn the_planner_copy_loads_with_no_structural_errors_and_compiles() {
        let parsed = electricity_yaml::load_yaml(PLANNER_YAML).expect("the copy parses as YAML");
        assert_eq!(
            electricity_compiler::structural_errors(&parsed),
            Vec::<String>::new()
        );
        let program = electricity_compiler::compile_document(&parsed, &DocumentOrigin::Generated)
            .expect("the copy compiles");
        assert!(first_unsupported(&program, false, &Supported::current(&[])).is_none());
    }

    #[test]
    fn the_planner_copy_declares_the_five_canonical_output_paths() {
        let parsed = electricity_yaml::load_yaml(PLANNER_YAML).unwrap();
        let outputs = parsed
            .as_dict()
            .unwrap()
            .get(&Value::Str("interface".to_string()))
            .unwrap()
            .as_dict()
            .unwrap()
            .get(&Value::Str("outputs".to_string()))
            .unwrap()
            .as_dict()
            .unwrap();
        for (name, path) in PLANNER_OUTPUTS {
            let declared = outputs
                .get(&Value::Str(name.to_string()))
                .unwrap()
                .as_dict()
                .unwrap()
                .get(&Value::Str("path".to_string()))
                .unwrap()
                .as_str()
                .unwrap();
            assert_eq!(declared, path, "output {name}");
        }
    }

    fn routing_with_bands(
        respect_explicit: bool,
        bands: Vec<ComplexityBand>,
    ) -> ComplexitySettings {
        ComplexitySettings {
            routing: RoutingSettings {
                enabled: true,
                bands,
                respect_explicit,
            },
            ..ComplexitySettings::default()
        }
    }

    fn band(model: &str, max: Option<f64>) -> ComplexityBand {
        ComplexityBand {
            model: model.to_string(),
            max,
            name: None,
        }
    }

    #[test]
    fn route_up_model_is_none_when_routing_is_off() {
        let settings = ComplexitySettings::default();
        assert_eq!(route_up_model(&settings, None), None);
    }

    #[test]
    fn route_up_model_is_none_with_no_bands() {
        let settings = routing_with_bands(true, Vec::new());
        assert_eq!(route_up_model(&settings, None), None);
    }

    #[test]
    fn route_up_model_is_none_when_explicit_model_set_and_respected() {
        let settings = routing_with_bands(true, vec![band("small", Some(10.0)), band("big", None)]);
        assert_eq!(route_up_model(&settings, Some("pinned")), None);
    }

    #[test]
    fn route_up_model_is_the_catch_all_band_when_explicit_not_respected() {
        let settings =
            routing_with_bands(false, vec![band("small", Some(10.0)), band("big", None)]);
        assert_eq!(
            route_up_model(&settings, Some("pinned")),
            Some("big".to_string())
        );
    }

    #[test]
    fn route_up_model_is_the_catch_all_band_with_no_explicit_model() {
        let settings = routing_with_bands(true, vec![band("small", Some(10.0)), band("big", None)]);
        assert_eq!(route_up_model(&settings, None), Some("big".to_string()));
    }

    #[test]
    fn planner_settings_disables_decomposition_but_keeps_scoring_and_routing() {
        let mut settings = ComplexitySettings::default();
        settings.decomposition.enabled = true;
        settings.decomposition.threshold = 42.0;
        settings.scoring.enabled = true;
        settings.routing.enabled = true;
        let planner = planner_settings(&settings);
        assert!(!planner.decomposition.enabled);
        assert_eq!(planner.decomposition, DecompositionSettings::default());
        assert!(planner.scoring.enabled);
        assert!(planner.routing.enabled);
    }

    #[test]
    fn describe_inputs_prefers_the_input_namespace() {
        let ctx = dict(vec![
            (
                "input",
                dict(vec![("b", Value::from(1i64)), ("a", Value::from(2i64))]),
            ),
            ("prime", dict(vec![])),
        ]);
        assert_eq!(describe_inputs(&ctx), "inputs:\n- a\n- b\n");
    }

    #[test]
    fn describe_inputs_falls_back_to_top_level_keys_excluding_namespaces_and_underscored() {
        let ctx = dict(vec![
            ("topic", Value::Str("x".to_string())),
            ("prime", dict(vec![])),
            ("runtime", dict(vec![])),
            ("_private", Value::Str("y".to_string())),
        ]);
        assert_eq!(describe_inputs(&ctx), "inputs:\n- topic\n");
    }

    #[test]
    fn describe_inputs_with_no_keys_says_so() {
        let ctx = dict(vec![("input", dict(vec![]))]);
        assert_eq!(describe_inputs(&ctx), "(no declared inputs)");
    }

    #[test]
    fn source_output_dumps_prompt_type_and_schema() {
        let text = source_output("text", None).unwrap();
        assert_eq!(text, "prompt_type: text\nschema: null\n");
    }

    #[test]
    fn planner_input_has_the_expected_key_order() {
        let ctx = dict(vec![(
            "input",
            dict(vec![("topic", Value::Str("x".to_string()))]),
        )]);
        let input = planner_input("hi {{input.topic}}", &ctx, "text", None, 5).unwrap();
        let inner = input
            .as_dict()
            .unwrap()
            .get(&Value::Str("input".to_string()))
            .unwrap()
            .as_dict()
            .unwrap();
        let keys: Vec<&str> = inner.keys().filter_map(Value::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "source_template",
                "source_interface",
                "source_output",
                "max_chunks"
            ]
        );
    }

    fn plan(say: Value, chunks: Value, yaml: Option<&str>, done: Value, result_path: &str) -> Plan {
        Plan {
            say,
            chunks,
            yaml: yaml.map(str::to_string),
            done,
            result_path: result_path.to_string(),
        }
    }

    #[test]
    fn read_plan_resolves_the_five_canonical_paths_and_cleans_the_yaml() {
        let mut prime = Dict::new();
        prime.insert(
            Value::Str("plan".to_string()),
            dict(vec![(
                "value",
                dict(vec![
                    ("say", Value::Str("hi".to_string())),
                    ("chunks", Value::List(vec![Value::from(1i64)])),
                ]),
            )]),
        );
        prime.insert(
            Value::Str("check".to_string()),
            dict(vec![(
                "value",
                dict(vec![(
                    "yaml",
                    Value::Str("```\neffects: []\n```".to_string()),
                )]),
            )]),
        );
        prime.insert(
            Value::Str("done".to_string()),
            dict(vec![("value", Value::Bool(true))]),
        );
        prime.insert(
            Value::Str("result_path".to_string()),
            dict(vec![(
                "value",
                Value::Str("  prime.merge.value  ".to_string()),
            )]),
        );
        let state = dict(vec![("prime", Value::Dict(prime))]);

        let read = read_plan(&state);
        assert_eq!(read.say, Value::Str("hi".to_string()));
        assert_eq!(read.chunks, Value::List(vec![Value::from(1i64)]));
        assert_eq!(read.yaml, Some("effects: []".to_string()));
        assert_eq!(read.done, Value::Bool(true));
        assert_eq!(read.result_path, "prime.merge.value");
    }

    #[test]
    fn read_plan_falls_back_to_the_default_result_path_when_blank_or_absent() {
        let state = dict(vec![]);
        assert_eq!(read_plan(&state).result_path, DEFAULT_RESULT_PATH);
    }

    fn ok_plan(yaml: &str, result_path: &str) -> Plan {
        plan(
            Value::None,
            Value::List(vec![Value::from(1i64), Value::from(2i64)]),
            Some(yaml),
            Value::Bool(true),
            result_path,
        )
    }

    #[test]
    fn validate_plan_rejects_missing_or_blank_yaml() {
        let p = plan(
            Value::None,
            Value::None,
            None,
            Value::None,
            DEFAULT_RESULT_PATH,
        );
        assert_eq!(
            validate_plan(&p, 5, &Value::None),
            vec!["the planner returned no orchestration YAML".to_string()]
        );
        let blank = plan(
            Value::None,
            Value::None,
            Some("   "),
            Value::None,
            DEFAULT_RESULT_PATH,
        );
        assert_eq!(
            validate_plan(&blank, 5, &Value::None),
            vec!["the planner returned no orchestration YAML".to_string()]
        );
    }

    #[test]
    fn validate_plan_rejects_done_only_when_exactly_false() {
        let yaml = "effects:\n  - {type: tool, name: merge, provider: json, params: {mode: parse, input: '1'}}\n";
        for (done, rejected) in [
            (Value::Bool(false), true),
            (Value::None, false),
            (Value::from(0i64), false),
        ] {
            let p = plan(
                Value::None,
                Value::List(vec![Value::from(1i64), Value::from(2i64)]),
                Some(yaml),
                done.clone(),
                "prime.merge.value",
            );
            let problems = validate_plan(&p, 5, &Value::None);
            let has_done_problem = problems.iter().any(|p| p.contains("done gate is false"));
            assert_eq!(has_done_problem, rejected, "done={done:?}");
        }
    }

    #[test]
    fn validate_plan_rejects_too_few_or_too_many_chunks() {
        let yaml = "effects:\n  - {type: tool, name: merge, provider: json, params: {mode: parse, input: '1'}}\n";
        let one_chunk = plan(
            Value::None,
            Value::List(vec![Value::from(1i64)]),
            Some(yaml),
            Value::Bool(true),
            "prime.merge.value",
        );
        assert!(
            validate_plan(&one_chunk, 5, &Value::None)
                .iter()
                .any(|p| p.contains("needs at least 2"))
        );

        let many = plan(
            Value::None,
            Value::List((0..6).map(Value::from).collect()),
            Some(yaml),
            Value::Bool(true),
            "prime.merge.value",
        );
        assert!(
            validate_plan(&many, 5, &Value::None)
                .iter()
                .any(|p| p.contains("the budget is 5"))
        );
    }

    #[test]
    fn validate_plan_rejects_a_result_path_not_under_prime() {
        let yaml = "effects:\n  - {type: tool, name: merge, provider: json, params: {mode: parse, input: '1'}}\n";
        let p = ok_plan(yaml, "input.merge.value");
        let problems = validate_plan(&p, 5, &Value::None);
        assert!(
            problems.iter().any(|e| e.contains("does not point into")),
            "{problems:?}"
        );
    }

    #[test]
    fn validate_plan_rejects_a_result_path_with_no_matching_top_level_effect() {
        let yaml = "effects:\n  - {type: tool, name: merge, provider: json, params: {mode: parse, input: '1'}}\n";
        let p = ok_plan(yaml, "prime.nope.value");
        let problems = validate_plan(&p, 5, &Value::None);
        assert!(
            problems
                .iter()
                .any(|e| e.contains("no top-level effect named")),
            "{problems:?}"
        );
    }

    #[test]
    fn validate_plan_accepts_a_sound_plan() {
        let yaml = "effects:\n  - {type: tool, name: a, provider: json, params: {mode: parse, input: '1'}}\n  - {type: tool, name: merge, provider: json, params: {mode: parse, input: '1'}}\n";
        let p = ok_plan(yaml, "prime.merge.value");
        assert_eq!(validate_plan(&p, 5, &Value::None), Vec::<String>::new());
    }

    #[test]
    fn validate_plan_reports_allowlist_denials() {
        let yaml =
            "effects:\n  - {type: tool, name: merge, provider: shell, params: {cmd: echo}}\n";
        let p = ok_plan(yaml, "prime.merge.value");
        let mut allowlists = Dict::new();
        allowlists.insert(
            Value::Str("tools".to_string()),
            Value::List(vec![Value::Str("json".to_string())]),
        );
        let runtime_config = dict(vec![("_allowlists", Value::Dict(allowlists))]);
        let problems = validate_plan(&p, 5, &runtime_config);
        assert!(problems.iter().any(|e| e.contains("shell")), "{problems:?}");
    }

    #[test]
    fn base_meta_uses_float_score_and_threshold_and_int_depth() {
        let settings = DecompositionSettings::default();
        let base = base_meta(50.0, &settings, 1);
        assert_eq!(
            base.get(&Value::Str("score".to_string())),
            Some(&Value::Float(50.0))
        );
        assert_eq!(
            base.get(&Value::Str("threshold".to_string())),
            Some(&Value::Float(settings.threshold))
        );
        assert_eq!(
            base.get(&Value::Str("depth".to_string())),
            Some(&Value::from(1i64))
        );
        assert_eq!(
            base.get(&Value::Str("max_depth".to_string())),
            Some(&Value::from(settings.max_depth))
        );
        let keys: Vec<&str> = base.keys().filter_map(Value::as_str).collect();
        assert_eq!(
            keys,
            vec!["decomposed", "score", "threshold", "depth", "max_depth"]
        );
    }

    #[test]
    fn max_depth_result_routes_up_with_a_fallback_or_runs_as_is_without_one() {
        let base = base_meta(90.0, &DecompositionSettings::default(), 2);
        let with_fallback = max_depth_result(base.clone(), Some("big".to_string()));
        assert_eq!(
            with_fallback.posture,
            Posture::Dispatch {
                fallback_model: Some("big".to_string())
            }
        );
        let meta = with_fallback.meta.as_dict().unwrap();
        assert_eq!(
            meta.get(&Value::Str("outcome".to_string())),
            Some(&Value::Str("route_up".to_string()))
        );
        assert_eq!(
            meta.get(&Value::Str("reason".to_string())),
            Some(&Value::Str("max_depth".to_string()))
        );

        let without_fallback = max_depth_result(base, None);
        assert_eq!(
            without_fallback.posture,
            Posture::Dispatch {
                fallback_model: None
            }
        );
        let meta = without_fallback.meta.as_dict().unwrap();
        assert_eq!(
            meta.get(&Value::Str("outcome".to_string())),
            Some(&Value::Str("run_as_is".to_string()))
        );
    }

    #[test]
    fn failure_result_fails_outright_under_on_failure_fail_forcing_fallback_to_none() {
        let settings = DecompositionSettings {
            on_failure: "fail".to_string(),
            ..DecompositionSettings::default()
        };
        let base = base_meta(90.0, &settings, 0);
        let result = failure_result(
            base,
            &settings,
            "planner_failed",
            "boom".to_string(),
            Some("big".to_string()),
            None,
        );
        assert_eq!(
            result.posture,
            Posture::Fail {
                reason: "planner_failed",
                error: "boom".to_string()
            }
        );
        let meta = result.meta.as_dict().unwrap();
        assert_eq!(
            meta.get(&Value::Str("fallback_model".to_string())),
            Some(&Value::None)
        );
        assert_eq!(meta.get(&Value::Str("raw_payload".to_string())), None);
    }

    #[test]
    fn failure_result_routes_up_under_on_failure_route_up_and_keeps_a_nonempty_raw_payload() {
        let settings = DecompositionSettings::default();
        let base = base_meta(90.0, &settings, 0);
        let result = failure_result(
            base,
            &settings,
            "planner_failed",
            "boom".to_string(),
            Some("big".to_string()),
            Some("raw text".to_string()),
        );
        assert_eq!(
            result.posture,
            Posture::Dispatch {
                fallback_model: Some("big".to_string())
            }
        );
        let meta = result.meta.as_dict().unwrap();
        assert_eq!(
            meta.get(&Value::Str("raw_payload".to_string())),
            Some(&Value::Str("raw text".to_string()))
        );
    }

    #[test]
    fn failure_result_drops_an_empty_raw_payload() {
        let settings = DecompositionSettings::default();
        let base = base_meta(90.0, &settings, 0);
        let result = failure_result(
            base,
            &settings,
            "invalid_plan",
            "bad".to_string(),
            None,
            Some(String::new()),
        );
        let meta = result.meta.as_dict().unwrap();
        assert_eq!(meta.get(&Value::Str("raw_payload".to_string())), None);
    }

    #[test]
    fn success_result_updates_decomposed_in_place_as_the_first_key() {
        let settings = DecompositionSettings::default();
        let base = base_meta(90.0, &settings, 0);
        let result = success_result(base, Value::Str("merged".to_string()));
        let meta = result.meta.as_dict().unwrap();
        let keys: Vec<&str> = meta.keys().filter_map(Value::as_str).collect();
        assert_eq!(keys[0], "decomposed");
        assert_eq!(
            meta.get(&Value::Str("decomposed".to_string())),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            meta.get(&Value::Str("outcome".to_string())),
            Some(&Value::Str("decomposed".to_string()))
        );
        assert_eq!(
            meta.get(&Value::Str("reason".to_string())),
            Some(&Value::None)
        );
        assert_eq!(
            result.posture,
            Posture::Decomposed(Value::Str("merged".to_string()))
        );
    }

    #[test]
    fn failure_text_has_the_expected_shape() {
        assert_eq!(
            failure_text("ask", "planner_failed", "boom"),
            "decomposition of 'ask' failed (planner_failed): boom"
        );
    }

    #[test]
    fn plan_identity_is_inline_plus_sixteen_hex_characters() {
        let identity = plan_identity("effects: []");
        assert!(identity.starts_with("inline:"));
        assert_eq!(identity.len(), "inline:".len() + 16);
    }

    #[test]
    fn cycle_error_fires_only_when_identity_is_already_on_the_stack() {
        assert_eq!(cycle_error("ask", &[], "inline:abc"), None);
        let stack = vec!["inline:abc".to_string()];
        let message = cycle_error("ask", &stack, "inline:abc").unwrap();
        assert_eq!(
            message,
            "decomposition of 'ask': cycle detected \u{2014} inline:abc \u{2192} inline:abc"
        );
    }

    #[test]
    fn child_seed_state_keeps_only_string_keys_and_clears_the_result_segment() {
        let mut prime = Dict::new();
        prime.insert(
            Value::Str("merge".to_string()),
            Value::Str("stale".to_string()),
        );
        prime.insert(
            Value::Str("other".to_string()),
            Value::Str("kept".to_string()),
        );
        let mut ctx = Dict::new();
        ctx.insert(Value::Str("prime".to_string()), Value::Dict(prime));
        ctx.insert(
            Value::Str("input".to_string()),
            Value::Str("kept too".to_string()),
        );
        ctx.insert(
            Value::from(1i64),
            Value::Str("dropped, non-string key".to_string()),
        );
        let seed = child_seed_state(&Value::Dict(ctx), "prime.merge.value");
        let seed_dict = seed.as_dict().unwrap();
        assert_eq!(seed_dict.len(), 2);
        let seed_prime = seed_dict
            .get(&Value::Str("prime".to_string()))
            .unwrap()
            .as_dict()
            .unwrap();
        assert!(!seed_prime.contains_key(&Value::Str("merge".to_string())));
        assert_eq!(
            seed_prime.get(&Value::Str("other".to_string())),
            Some(&Value::Str("kept".to_string()))
        );
    }

    #[test]
    fn missing_result_error_has_the_expected_shape() {
        assert_eq!(
            missing_result_error("prime.merge.value"),
            "the decomposition child completed but left nothing at result_path 'prime.merge.value'"
        );
    }
}
