//! Runtime decomposition (`core/decompose.py`) -- issue #521 (M1 lane
//! L2a of #448) ported every pure part that needs no `prompt`-effect
//! wiring: the route-up model, the planner's input/output shapes,
//! `_validate_plan`, the `meta.decomposition` record builders, the
//! plan's identity and cycle text, and the decomposition child's seed
//! state. [`maybe_decompose`] (issue #522, lane L2b) is the call-site
//! wiring: `exec::prompt::execute_prompt` calls it once, right after
//! the retry policy is resolved and before `build_options`, and acts
//! on whichever of [`Posture`]'s three cases comes back.
//!
//! `_check_capability_ceiling` (`core/decompose.py:582-607`) is not
//! ported (#448's Settled question 9): electricity never installs a
//! capability ceiling, so that check is always `[]` here, and
//! [`validate_plan`] never calls an equivalent of it at all.

use std::rc::Rc;

use crate::exec::key;
use crate::exec::prompt::{prompt_type_str, role_str};
use crate::exec::use_::{
    allowlist_names, record_children_enabled, resolve_dot_path, run_isolated, sha256_hex,
    validate_inline_yaml,
};
use crate::exec::{CtxChain, live_ctx};
use crate::params::ParamsCtx;
use crate::store::Slot;
use crate::{CancellationToken, Frame, NodeRef, RunContext, RunObserver, Store, VmError};
use electricity_bytecode::effects::{
    AssetRef, DispatchFile, PromptContent, PromptOp, PromptType, TimeoutMs,
};
use electricity_bytecode::param::ParamNode;
use electricity_bytecode::{EffectPath, Op, first_unsupported};
use electricity_compiler::DocumentOrigin;
use electricity_config::{ComplexitySettings, DecompositionSettings, SplittingSettings};
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
#[derive(Debug, Clone)]
pub struct DecompositionResult {
    pub meta: Value,
    pub posture: Posture,
    /// `runtime.state.record_children`'s own filtered copy of the
    /// child's declared effects (issue #672, `core/decompose.py::
    /// _graft_declared_children`): empty unless the setting is on AND
    /// the plan actually ran and succeeded (never set on a `max_depth`
    /// route-up/run-as-is, or any failure path, which never leaves a
    /// child's effects on the node at all). The caller
    /// (`exec::prompt::execute_prompt`) grafts these onto the
    /// decomposing effect's own node once it writes `value`, the same
    /// `Slot`-preserving copy `exec::use_::graft_child_record` gives a
    /// `use` child.
    pub child_record: Vec<(Value, Slot)>,
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
/// complexity settings, both decomposition triggers switched off --
/// scoring and routing stay as configured. The planner's own planning
/// prompt scores off the chart by construction, and its rendered input
/// (the source template plus the planning instructions) is enormous by
/// construction too, so a size-triggered split needs the same shield a
/// score-triggered decomposition already gets: left on, it would hand
/// the planner a plan built from a slice of its own instructions rather
/// than the planner itself, and under `splitting.on_failure: fail` would
/// turn every over-threshold decomposition attempt into a planner
/// failure. Python replaces the whole `complexity.decomposition` and
/// `complexity.splitting` blocks with `{"enabled": False}`; since this
/// crate already holds *resolved* settings rather than raw config, that
/// is exactly [`DecompositionSettings::default`]/[`SplittingSettings::
/// default`] with `enabled` forced false.
pub fn planner_settings(settings: &ComplexitySettings) -> ComplexitySettings {
    let mut cloned = settings.clone();
    cloned.decomposition = DecompositionSettings {
        enabled: false,
        ..DecompositionSettings::default()
    };
    cloned.splitting = SplittingSettings {
        enabled: false,
        ..SplittingSettings::default()
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

/// `core/decompose.py::_validate_generated_yaml` (issue #682): the four
/// checks every generated plan -- planned (score trigger) or
/// deterministically generated (size trigger) -- must pass before it
/// runs, minus the schema/chunk-count checks [`validate_plan`] alone
/// needs for a *planned* document. Shared so the size trigger's own,
/// never-planned plan runs through the same gate [`validate_plan`]
/// gives the score trigger's planner output.
fn validate_generated_yaml(yaml: &str, result_path: &str, runtime_config: &Value) -> Vec<String> {
    let throwaway = Plan {
        say: Value::None,
        chunks: Value::None,
        yaml: Some(yaml.to_string()),
        done: Value::None,
        result_path: result_path.to_string(),
    };
    let parsed = match electricity_yaml::load_yaml(yaml) {
        Err(err) => return vec![format!("YAML parse error: {err}")],
        Ok(value) => value,
    };
    if let Err(errors) = validate_inline_yaml(&parsed) {
        return errors;
    }
    let mut problems = check_merge_contract(&throwaway);
    problems.extend(check_allowlists(&throwaway, runtime_config));
    problems
}

/// `core/decompose.py::_validate_plan`. `_check_capability_ceiling`
/// is not ported (this module's own doc comment); nothing stands in
/// for it here.
///
/// `Err` is `_validate_plan`'s own uncaught-`ValueError` escape (a
/// calendar-invalid timestamp in the plan's own YAML, issue #528's own
/// comment): a bare message, discarding every problem already
/// collected, for the caller to raise the same way Python's own escape
/// reaches `exec.prompt`'s `except Exception as e:` and its `on_error`
/// -- never folded into the ordinary `Ok` problems list, which is
/// `dset.on_failure`'s own (invalid-plan) gate, a different, later
/// decision this exception bypasses entirely in Python too.
pub fn validate_plan(
    plan: &Plan,
    max_chunks: i64,
    runtime_config: &Value,
) -> Result<Vec<String>, String> {
    let mut problems = Vec::new();

    let yaml = match &plan.yaml {
        Some(text) if !py_strip(text).is_empty() => text.clone(),
        _ => {
            problems.push("the planner returned no orchestration YAML".to_string());
            return Ok(problems);
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
        // `core/decompose.py::_validate_plan`'s own `_validate_inline_yaml`
        // call never catches a calendar-invalid timestamp's bare
        // `ValueError` (issue #528's own comment) -- it escapes uncaught,
        // discarding every `problems` entry gathered so far and
        // propagating past `_validate_plan`/`maybe_decompose` entirely,
        // to the same generic, retryable-attempt path any other uncaught
        // dispatch exception reaches: `exec.prompt`'s own `on_error`
        // (default `"fail"`), not `dset.on_failure`'s invalid-plan gate.
        // Only the message itself is matched exactly: no location, no
        // "YAML parse error:" prefix.
        Err(electricity_yaml::YamlError::InvalidTimestampValue { message, .. }) => {
            return Err(message);
        }
        Err(err) => {
            problems.push(format!("YAML parse error: {err}"));
            return Ok(problems);
        }
        Ok(value) => value,
    };
    if let Err(errors) = validate_inline_yaml(&parsed) {
        problems.extend(errors);
        return Ok(problems);
    }

    problems.extend(check_merge_contract(plan));
    problems.extend(check_allowlists(plan, runtime_config));
    Ok(problems)
}

/// `core/decompose.py::maybe_decompose`'s own `base` dict (`:171-180`).
/// `trigger` (issue #672) always reads `"score"` here — this is the score
/// trigger's own record construction; the size trigger is a Python-only
/// preview feature electricity refuses up front rather than running.
pub fn base_meta(score: f64, settings: &DecompositionSettings, depth: u32) -> Dict {
    let mut dict = Dict::new();
    dict.insert(Value::Str("decomposed".to_string()), Value::Bool(false));
    dict.insert(
        Value::Str("trigger".to_string()),
        Value::Str("score".to_string()),
    );
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
        child_record: Vec::new(),
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
        child_record: Vec::new(),
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
        child_record: Vec::new(),
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

/// `core/decompose.py::_execute_split_plan`'s own cycle check
/// (`:1860-1863`) -- the size trigger's own text, distinct from
/// [`cycle_error`]'s "decomposition of" (the score trigger's own
/// wording).
pub fn split_cycle_error(prompt_name: &str, stack: &[String], identity: &str) -> Option<String> {
    if !stack.iter().any(|entry| entry == identity) {
        return None;
    }
    let mut chain: Vec<&str> = stack.iter().map(String::as_str).collect();
    chain.push(identity);
    Some(format!(
        "split of '{prompt_name}': cycle detected \u{2014} {}",
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

/// `core/decompose.py::_declared_effect_names`: the plan's own
/// top-level effect names, in document order, read straight off the
/// parsed YAML dict (the child can hold sibling effects its own
/// *copied* render context carried in for template resolution, never
/// actually run by this dispatch -- only the generated/planned
/// document's own effects belong on the decomposing node).
fn declared_effect_names(parsed: &Value) -> Vec<String> {
    let Some(effects) = parsed
        .as_dict()
        .and_then(|d| d.get(&Value::Str("effects".to_string())))
        .and_then(Value::as_list)
    else {
        return Vec::new();
    };
    effects
        .iter()
        .filter_map(|entry| {
            entry
                .as_dict()
                .and_then(|d| d.get(&Value::Str("name".to_string())))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// `core/decompose.py::_graft_declared_children`: every *declared_names*
/// key the child's own `prime` node holds, as `(key, Slot)` pairs ready
/// to insert onto the decomposing effect's own node -- filtered, unlike
/// `exec::use_::graft_child_record`'s blanket copy, for the reason
/// [`declared_effect_names`]'s own doc comment gives.
fn declared_children_entries(
    child_prime: &NodeRef,
    declared_names: &[String],
) -> Vec<(Value, Slot)> {
    child_prime
        .borrow()
        .iter()
        .filter(|(key, _)| matches!(key, Value::Str(s) if declared_names.iter().any(|n| n == s)))
        .map(|(key, slot)| (key.clone(), slot.clone()))
        .collect()
}

/// `core/decompose.py::_execute_plan`'s own missing-result text
/// (`:729-734`).
pub fn missing_result_error(result_path: &str) -> String {
    format!("the decomposition child completed but left nothing at result_path '{result_path}'")
}

/// `core/decompose.py::_execute_split_plan`'s own missing-result text
/// (`:1931-1934`) -- the size trigger's own text, distinct from
/// [`missing_result_error`]'s "the decomposition child" (the score
/// trigger's own wording).
pub fn split_missing_result_error(result_path: &str) -> String {
    format!("the split child completed but left nothing at result_path '{result_path}'")
}

/// `core/prompt.py:1066`'s own `self.defn.template or prompt_sent` --
/// the raw, un-rendered template source the emitted plan can
/// re-declare `{{...}}` references from, for a `template:` prompt;
/// *prompt_sent* (already materialized, self-contained) for a
/// `messages:` prompt, which has no single template, or an empty
/// `template:` (Python's `or` is falsy on `""` too).
fn planner_source_template<'a>(prompt: &'a PromptOp, prompt_sent: &'a str) -> &'a str {
    match &prompt.content {
        PromptContent::Template(text) if !text.source.is_empty() => &text.source,
        _ => prompt_sent,
    }
}

/// The numeric `score` `core/prompt.py::_maybe_decompose` reads out of
/// the pre-dispatch `meta.complexity` block before ever calling into
/// this module -- `None` when scoring is off, scoring itself failed,
/// or (defensively) the key is present but not a number, matching
/// Python's own `isinstance(score, (int, float))` guard.
fn complexity_score(complexity_meta: Option<&Value>) -> Option<f64> {
    let score = complexity_meta?
        .as_dict()?
        .get(&Value::Str("score".to_string()))?;
    match score {
        Value::Float(f) => Some(*f),
        Value::Int(electricity_value::IntValue::Small(n)) => Some(*n as f64),
        _ => None,
    }
}

/// `core/decompose.py::_rejected_payload_text`'s own chain walk has no
/// equivalent here: a [`VmError`] carries no exception chain of its
/// own, only a *cause* that survives a chain/tree wrap unchanged
/// (`exec::wrap_effect_error`'s own doc comment) -- so recovering the
/// planner's rejected reply text is just reading whatever *cause* the
/// error already carries, if any.
fn rejected_payload_text(err: &VmError) -> Option<String> {
    match err {
        VmError::Raised {
            cause: electricity_tools::ErrorCause::SchemaRejected { raw_response_text },
            ..
        } => Some(raw_response_text.clone()),
        _ => None,
    }
}

/// The planner's and the plan's own `Frame`, one level deeper than the
/// triggering prompt's -- `Frame::nested` doesn't apply
/// `with_display_depth` itself (callers chain it, same convention `use`
/// children already follow), so this helper does both steps for
/// either child. No `push_use`: only the plan's own frame needs one
/// (issue #522's Settled question 6's cycle guard), chained onto this
/// helper's result by the one caller that needs it.
fn child_frame(frame: &Frame, op_path: &EffectPath) -> Frame {
    frame
        .nested(op_path)
        .with_display_depth(crate::complexity::depth::use_child_root(
            frame.display_depth(),
        ))
}

/// The planner's and the plan's own `RunContext` -- the caller's own,
/// except always unlocked (`core/decompose.py::_run_isolated`'s own
/// `DynamicRuntime(model_locked=False)` default, issue #522's Settled
/// question 2) and never a dry run, with *complexity*/*decomposition_depth*
/// and the child program's own declared-prompts/effect-names tables
/// supplied by the one caller that knows which child this is (the
/// planner keeps the run's own `decomposition_depth` unchanged; the
/// plan increments it, issue #522's own Scope section 2 steps 4/7).
fn child_run_ctx<'a>(
    run_ctx: &RunContext<'a>,
    complexity: &'a ComplexitySettings,
    decomposition_depth: u32,
    declared_prompts: &'a indexmap::IndexMap<String, String>,
    effect_names: &'a std::collections::BTreeSet<String>,
) -> RunContext<'a> {
    RunContext {
        registry: run_ctx.registry,
        limiter: run_ctx.limiter,
        model: run_ctx.model,
        adapter: run_ctx.adapter,
        runtime_config: run_ctx.runtime_config,
        dry_run: false,
        armed: run_ctx.armed,
        adapters: run_ctx.adapters,
        default_adapter: Rc::clone(&run_ctx.default_adapter),
        model_locked: false,
        adapter_timeout_seconds: run_ctx.adapter_timeout_seconds,
        complexity,
        decomposition_depth,
        orchestration_dir: run_ctx.orchestration_dir,
        declared_prompts,
        effect_names,
        supported: run_ctx.supported,
    }
}

/// `core/prompt.py::_maybe_decompose` + `core/decompose.py::
/// maybe_decompose`, merged into this one call site (issue #522,
/// M1-L2b): `exec::prompt::execute_prompt` calls this once, right
/// after the retry policy is resolved and before `build_options`. See
/// this module's own doc comment for *why* there, and issue #522's own
/// Scope section 2 for the step-by-step this function follows. `Ok(None)`
/// means decomposition did not trigger at all (the caller dispatches
/// unchanged). `Err` carries either a fatal [`VmError`] (`VmError::
/// is_fatal`) -- a cancellation, or Settled question 6's own
/// refused-plan case -- which the caller returns unchanged, writing
/// nothing more; or a plain [`VmError::Raised`] standing in for
/// [`validate_plan`]'s own uncaught-`ValueError` escape (that
/// function's own doc comment), which the caller routes through
/// [`super::exec::prompt`]'s ordinary `on_error` handling instead --
/// in both `Err` cases, unlike `Ok`, no `meta.decomposition` is ever
/// written at all, matching Python's own `maybe_decompose` raising
/// before it ever returns a value its caller could record.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn maybe_decompose(
    op: &Op,
    prompt: &PromptOp,
    complexity_meta: Option<&Value>,
    prompt_sent: &str,
    resolved_model: &str,
    render_chain: &CtxChain,
    store: &Store,
    frame: &Frame,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
) -> Result<Option<DecompositionResult>, VmError> {
    let name = op.name.as_deref().unwrap_or("");
    let settings = run_ctx.complexity;

    // Size runs first (issue #682, `core/decompose.py::maybe_decompose`'s
    // own doc comment): a part is within budget by construction, a
    // planned chunk is not, so checking size first means every leaf
    // finally sent fits whatever the score trigger's planner does.
    if let Some(result) = maybe_split(
        op,
        prompt,
        prompt_sent,
        resolved_model,
        render_chain,
        store,
        frame,
        run_ctx,
        observer,
        token,
    )
    .await?
    {
        return Ok(Some(result));
    }

    let dset = &settings.decomposition;

    if !dset.enabled {
        return Ok(None);
    }
    let Some(score) = complexity_score(complexity_meta) else {
        return Ok(None);
    };
    if score <= dset.threshold {
        return Ok(None);
    }

    let depth = run_ctx.decomposition_depth;
    let fallback = route_up_model(settings, prompt.model.as_deref());
    let mut base = base_meta(score, dset, depth);

    if i64::from(depth) >= dset.max_depth {
        return Ok(Some(max_depth_result(base, fallback)));
    }

    let planner_warning = || {
        log::warn!(
            "Decomposition planner failed for prompt {}",
            Value::Str(name.to_string()).py_repr()
        );
    };
    let child_warning = || {
        log::warn!(
            "Decomposition child failed for prompt {}",
            Value::Str(name.to_string()).py_repr()
        );
    };

    // -- Plan: the synced planner runs isolated, decomposition off,
    // scoring/routing left as configured. --
    let parsed_planner = electricity_yaml::load_yaml(PLANNER_YAML).expect(
        "PLANNER_YAML is a build-time constant, already pinned to parse by this module's own tests",
    );
    let planner_program =
        electricity_compiler::compile_document(&parsed_planner, &DocumentOrigin::Generated).expect(
            "PLANNER_YAML is a build-time constant, already pinned to compile by this module's own tests",
        );

    let planner_ctx = live_ctx(render_chain, store);
    let planner_initial_state = match planner_input(
        planner_source_template(prompt, prompt_sent),
        &planner_ctx,
        prompt_type_str(prompt.prompt_type),
        prompt.schema.as_ref(),
        dset.max_chunks,
    ) {
        Ok(state) => state,
        Err(err) => {
            planner_warning();
            return Ok(Some(failure_result(
                base,
                dset,
                "planner_failed",
                err.to_string(),
                fallback,
                None,
            )));
        }
    };

    let planner_settings_value = planner_settings(settings);
    let planner_frame = child_frame(frame, &op.path);
    let planner_run_ctx = child_run_ctx(
        run_ctx,
        &planner_settings_value,
        run_ctx.decomposition_depth,
        &planner_program.prompts,
        &planner_program.effect_names,
    );

    let planner_store = match run_isolated(
        &planner_program,
        planner_initial_state,
        &planner_frame,
        &planner_run_ctx,
        observer,
        token,
    )
    .await
    {
        Err(err) if err.is_fatal() => return Err(err),
        Err(err) => {
            planner_warning();
            let raw_payload = rejected_payload_text(&err);
            return Ok(Some(failure_result(
                base,
                dset,
                "planner_failed",
                err.to_string(),
                fallback,
                raw_payload,
            )));
        }
        Ok(store) => store,
    };

    let plan = read_plan(&planner_store.snapshot(&planner_store.root));
    with_plan(&mut base, &plan);

    // -- Validate. --
    // `Err` is `_validate_plan`'s own uncaught-`ValueError` escape
    // (`validate_plan`'s own doc comment) -- raised here the same way,
    // bypassing `dset.on_failure`/`failure_result` entirely, for
    // `exec::prompt`'s `?` to propagate to its own `on_error` handling.
    let problems = match validate_plan(&plan, dset.max_chunks, run_ctx.runtime_config) {
        Ok(problems) => problems,
        Err(message) => {
            return Err(VmError::Raised {
                py_class: electricity_tools::PyExc::ValueError,
                message,
                cause: electricity_tools::ErrorCause::None,
                cause_class: None,
            });
        }
    };
    if !problems.is_empty() {
        return Ok(Some(failure_result(
            base,
            dset,
            "invalid_plan",
            problems.join("; "),
            fallback,
            None,
        )));
    }

    // -- Execute. --
    let yaml_text = plan.yaml.clone().unwrap_or_default();
    let parsed = match electricity_yaml::load_yaml_last_key_wins(&yaml_text) {
        Ok(value) => value,
        Err(err) => {
            child_warning();
            return Ok(Some(failure_result(
                base,
                dset,
                "execution_failed",
                err.to_string(),
                fallback,
                None,
            )));
        }
    };
    if parsed.as_dict().is_none() {
        child_warning();
        return Ok(Some(failure_result(
            base,
            dset,
            "execution_failed",
            "the emitted orchestration is not a YAML mapping".to_string(),
            fallback,
            None,
        )));
    }

    let identity = plan_identity(&yaml_text);
    if let Some(message) = cycle_error(name, frame.use_call_stack(), &identity) {
        child_warning();
        return Ok(Some(failure_result(
            base,
            dset,
            "execution_failed",
            message,
            fallback,
            None,
        )));
    }

    let plan_program =
        match electricity_compiler::compile_document(&parsed, &DocumentOrigin::Generated) {
            Ok(program) => program,
            Err(err) => {
                child_warning();
                return Ok(Some(failure_result(
                    base,
                    dset,
                    "execution_failed",
                    err.0,
                    fallback,
                    None,
                )));
            }
        };

    // Settled question 6: a plan whose content this build can't run is
    // a fatal, unrecorded refusal -- never `execution_failed`, never
    // absorbed by `on_error`/`on_failure`, nothing written to
    // `meta.decomposition` at all. Checked before anything below ever
    // touches *base* again.
    if let Some(refusal) = first_unsupported(&plan_program, false, &run_ctx.supported) {
        return Err(VmError::Refused {
            refusal,
            child_label: Some(format!("decomposition plan of '{name}'")),
        });
    }

    let seed_ctx = live_ctx(render_chain, store);
    let seed_state = child_seed_state(&seed_ctx, &plan.result_path);

    let plan_frame = child_frame(frame, &op.path).push_use(identity);
    let plan_run_ctx = child_run_ctx(
        run_ctx,
        run_ctx.complexity,
        depth + 1,
        &plan_program.prompts,
        &plan_program.effect_names,
    );

    let child_store = match run_isolated(
        &plan_program,
        seed_state,
        &plan_frame,
        &plan_run_ctx,
        observer,
        token,
    )
    .await
    {
        Err(VmError::Refused {
            refusal,
            child_label,
        }) => {
            return Err(VmError::Refused {
                refusal,
                child_label: child_label
                    .or_else(|| Some(format!("decomposition plan of '{name}'"))),
            });
        }
        Err(err) if err.is_fatal() => return Err(err),
        Err(err) => {
            child_warning();
            return Ok(Some(failure_result(
                base,
                dset,
                "execution_failed",
                err.to_string(),
                fallback,
                None,
            )));
        }
        Ok(store) => store,
    };

    let value = resolve_dot_path(&child_store.snapshot(&child_store.root), &plan.result_path);
    if matches!(value, Value::None) {
        child_warning();
        return Ok(Some(failure_result(
            base,
            dset,
            "execution_failed",
            missing_result_error(&plan.result_path),
            fallback,
            None,
        )));
    }

    let mut result = success_result(base, value);
    // `runtime.state.record_children` (issue #672): the child's own
    // declared effects, grafted alongside `value`/`meta` on the
    // decomposing node -- the caller (`exec::prompt::execute_prompt`)
    // does the actual graft, once it knows `value` is being written.
    if record_children_enabled(run_ctx.runtime_config) {
        if let Ok(Some(child_prime)) =
            child_store.child(&child_store.root, &Value::Str("prime".to_string()))
        {
            let declared = declared_effect_names(&parsed);
            result.child_record = declared_children_entries(&child_prime, &declared);
        }
    }
    Ok(Some(result))
}

// --------------------------------------------------------------------------
// size trigger (issue #672/#682)
// --------------------------------------------------------------------------

/// `core/decompose.py::_resolve_split_budget`.
fn resolve_split_budget(
    prompt: &PromptOp,
    sset: &SplittingSettings,
    resolved_model: &str,
) -> Option<i64> {
    if let Some(budget) = prompt.split_budget_tokens {
        return Some(budget);
    }
    if !sset.enabled {
        return None;
    }
    if let Some(budget) = sset.models.get(resolved_model) {
        return Some(*budget);
    }
    sset.max_input_tokens
}

/// `core/decompose.py::_native_merge_mode`.
fn native_merge_mode(prompt: &PromptOp) -> Option<&'static str> {
    match prompt.prompt_type {
        PromptType::Text => Some("join"),
        PromptType::Array => Some("concat"),
        PromptType::Json => {
            let is_array_schema = matches!(
                &prompt.schema,
                Some(Value::Dict(schema))
                    if matches!(
                        schema.get(&key("type")),
                        Some(Value::Str(t)) if t == "array"
                    )
            );
            if is_array_schema {
                Some("concat")
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `core/decompose.py::_maybe_split`'s own `base` dict (size trigger's
/// own key order -- `trigger: "size"`, distinct from the score
/// trigger's own [`base_meta`]).
fn split_base_meta(estimated: i64, budget: i64, depth: u32, sset: &SplittingSettings) -> Dict {
    let mut dict = Dict::new();
    dict.insert(key("decomposed"), Value::Bool(false));
    dict.insert(key("trigger"), Value::Str("size".to_string()));
    dict.insert(key("estimated_tokens"), Value::from(estimated));
    dict.insert(key("budget"), Value::from(budget));
    dict.insert(key("depth"), Value::from(i64::from(depth)));
    dict.insert(key("max_depth"), Value::from(sset.max_depth));
    dict
}

/// `core/decompose.py::_split_failure`: never a route-up (no bigger
/// context window), so `fallback_model` is never even a key in *base*
/// (unlike the score trigger's own [`failure_result`]).
fn split_failure_result(
    mut base: Dict,
    sset: &SplittingSettings,
    reason: &'static str,
    error: String,
) -> DecompositionResult {
    let fail = sset.on_failure == "fail";
    let outcome = if fail { "failed" } else { "run_as_is" };
    base.insert(key("outcome"), Value::Str(outcome.to_string()));
    base.insert(key("reason"), Value::Str(reason.to_string()));
    base.insert(key("error"), Value::Str(error.clone()));
    let posture = if fail {
        Posture::Fail { reason, error }
    } else {
        Posture::Dispatch {
            fallback_model: None,
        }
    };
    DecompositionResult {
        meta: Value::Dict(base),
        posture,
        child_record: Vec::new(),
    }
}

/// `core/split.py::NoSplitPoint`'s own text (`"{unit} does not fit the
/// room"`) -- this module never constructs the splitter's own error
/// type directly, only its message.
fn no_split_point_text(unit: &crate::split::NoSplitPoint) -> String {
    unit.to_string()
}

/// `core/decompose.py::_template_sources`: one `(label, raw text, role)`
/// per analyzable source -- the one template, or every `messages[]`
/// entry that isn't itself a dispatch-time file.
struct SplitSource {
    label: String,
    text: String,
    role: Option<electricity_bytecode::effects::Role>,
}

fn template_sources(prompt: &PromptOp) -> Vec<SplitSource> {
    match &prompt.content {
        PromptContent::Template(text) => vec![SplitSource {
            label: "template".to_string(),
            text: text.source.clone(),
            role: None,
        }],
        PromptContent::TemplateFile(_) => Vec::new(),
        PromptContent::Messages(messages) => messages
            .iter()
            .enumerate()
            .filter(|(_, message)| message.file.is_none())
            .map(|(index, message)| SplitSource {
                label: format!("messages[{index}].content"),
                text: message.content.source.clone(),
                role: Some(message.role),
            })
            .collect(),
    }
}

/// `core/decompose.py::_candidate_references`: every distinct state
/// reference across *expanded_sources*, in first-seen order --
/// `electricity_complexity::reference_names_sorted`'s own per-source
/// return is already sorted (`core/complexity.py::_extract_references`'s
/// own `sorted(references)`); this accumulates first-seen across
/// sources on top of that.
fn candidate_references(expanded_sources: &[String]) -> Result<Vec<String>, String> {
    let mut seen = std::collections::HashSet::new();
    let mut ordered = Vec::new();
    for text in expanded_sources {
        let names =
            electricity_complexity::reference_names_sorted(text).map_err(|err| err.to_string())?;
        for name in names {
            if seen.insert(name.clone()) {
                ordered.push(name);
            }
        }
    }
    Ok(ordered)
}

/// `core/decompose.py::_render_reference_alone`.
fn render_reference_alone(reference: &str, ctx: &Value) -> Result<String, String> {
    let mut template = String::with_capacity(reference.len() + 6);
    template.push_str("{{{");
    template.push_str(reference);
    template.push_str("}}}");
    electricity_template::render_template_with_root_and_escape(
        &template,
        ctx,
        &electricity_template::PlainCtx,
        "split reference",
        false,
    )
    .map_err(|err| err.to_string())
}

/// `core/decompose.py::_tag_occurrences_and_extensions`.
fn tag_occurrences_and_extensions(
    expanded_sources: &[String],
    reference: &str,
) -> Result<(usize, Vec<String>), String> {
    let mut count = 0usize;
    let mut extending = Vec::new();
    let prefix = format!("{reference}.");
    for text in expanded_sources {
        let tags = electricity_complexity::find_tags(text).map_err(|err| err.to_string())?;
        for (_sigil, name) in tags {
            if name == reference {
                count += 1;
            } else if name.starts_with(&prefix) {
                extending.push(name);
            }
        }
    }
    Ok((count, extending))
}

/// `core/decompose.py::_file_message_fixed_tokens`.
fn file_message_fixed_tokens(prompt: &PromptOp, ctx: &Value, name: &str) -> Result<i64, String> {
    let PromptContent::Messages(messages) = &prompt.content else {
        return Ok(0);
    };
    let mut total = 0i64;
    for (index, message) in messages.iter().enumerate() {
        let Some(source) = &message.file else {
            continue;
        };
        let field = format!("messages[{index}].content");
        let raw_path = ctx.resolve_from(&source.path_from);
        let within_value = source
            .within_from
            .as_deref()
            .and_then(|path| ctx.resolve_from(path));
        let read = electricity_compiler::prompt_files::read_dispatch_file(
            source,
            raw_path.as_ref(),
            within_value.as_ref(),
            &field,
            &format!("Prompt effect '{name}'"),
        )?;
        total +=
            crate::split::estimate_tokens(&format!("{}: {}", role_str(message.role), read.text));
    }
    Ok(total)
}

/// `core/decompose.py::_serialize_message_file`.
fn serialize_message_file(source: &DispatchFile) -> Value {
    let mut file = Dict::new();
    file.insert(key("from"), Value::Str(source.path_from.clone()));
    let mut out = Dict::new();
    out.insert(key("file"), Value::Dict(file));
    let within = match &source.within {
        Some(literal) => Value::Str(literal.clone()),
        None => {
            let mut within_dict = Dict::new();
            within_dict.insert(
                key("from"),
                Value::Str(source.within_from.clone().unwrap_or_default()),
            );
            Value::Dict(within_dict)
        }
    };
    out.insert(key("within"), within);
    Value::Dict(out)
}

/// `core/decompose.py::_set_dotted_copy`: a shallow-structural copy of
/// *ctx* with *value* set at the dotted *path* -- only the branch
/// actually being modified is rebuilt.
/// `core/prompt_compose.py::_dotted_get`: the split reference's own
/// value lookup, unlike every other dotted-path read in this module
/// (`exec::use_::resolve_dot_path`, ported from `use.py`'s own
/// `_resolve_dot_path`), walks a list segment too -- `int(part)`,
/// negative indices counting from the end, `None` (here, `Value::None`)
/// on a non-dict/non-list segment, a missing key, a segment that isn't
/// an integer, or an out-of-range index.
fn dotted_get(ctx: &Value, dot_path: &str) -> Value {
    let mut current = ctx.clone();
    for part in dot_path.split('.') {
        current = match &current {
            Value::Dict(dict) => match dict.get(&Value::Str(part.to_string())) {
                Some(next) => next.clone(),
                None => return Value::None,
            },
            Value::List(items) => {
                let Ok(index) = part.parse::<i64>() else {
                    return Value::None;
                };
                match python_list_index(items.len(), index) {
                    Some(resolved) => items[resolved].clone(),
                    None => return Value::None,
                }
            }
            _ => return Value::None,
        };
    }
    current
}

/// Python list indexing (negative indices count from the end) -- `None`
/// on any out-of-range index, never a panic.
fn python_list_index(len: usize, index: i64) -> Option<usize> {
    let len = len as i64;
    let normalized = if index < 0 { index + len } else { index };
    if normalized < 0 || normalized >= len {
        None
    } else {
        Some(normalized as usize)
    }
}

fn set_dotted_copy(ctx: &Value, path: &[&str], value: Value) -> Value {
    let Some((head, rest)) = path.split_first() else {
        return ctx.clone();
    };
    let mut new_dict = ctx.as_dict().cloned().unwrap_or_default();
    if rest.is_empty() {
        new_dict.insert(Value::Str((*head).to_string()), value);
    } else {
        let current = new_dict
            .get(&Value::Str((*head).to_string()))
            .cloned()
            .unwrap_or(Value::Dict(Dict::new()));
        let current = if current.as_dict().is_some() {
            current
        } else {
            Value::Dict(Dict::new())
        };
        new_dict.insert(
            Value::Str((*head).to_string()),
            set_dotted_copy(&current, rest, value),
        );
    }
    Value::Dict(new_dict)
}

/// Everything [`prepare_split`] worked out about *how* to split --
/// `core/decompose.py::_SplitPrep`.
struct SplitPrep {
    ref_path: String,
    kind: &'static str,
    parts: Vec<Value>,
    sizes: Vec<i64>,
    template_text: Option<String>,
    message_texts: Option<Vec<Option<String>>>,
    extra_state: indexmap::IndexMap<String, String>,
}

/// `core/decompose.py::_prepare_split`: pick the split reference,
/// measure the room each part has, and cut its value into whole-unit
/// parts.
fn prepare_split(
    prompt: &PromptOp,
    name: &str,
    ctx: &Value,
    declared_prompts: &indexmap::IndexMap<String, String>,
    effect_names: &std::collections::BTreeSet<String>,
    budget: i64,
) -> Result<SplitPrep, String> {
    if let PromptContent::TemplateFile(source) = &prompt.content {
        let raw_path = ctx.resolve_from(&source.path_from);
        let within_value = source
            .within_from
            .as_deref()
            .and_then(|path| ctx.resolve_from(path));
        let read = electricity_compiler::prompt_files::read_dispatch_file(
            source,
            raw_path.as_ref(),
            within_value.as_ref(),
            "template",
            &format!("Prompt effect '{name}'"),
        )?;
        if budget < 1 {
            return Err("the budget leaves no room for any part".to_string());
        }
        let parts = crate::split::split_text(&read.text, budget)
            .map_err(|err| no_split_point_text(&err))?;
        let sizes = parts
            .iter()
            .map(|part| crate::split::estimate_tokens(part))
            .collect();
        return Ok(SplitPrep {
            ref_path: "template".to_string(),
            kind: "string",
            parts: parts.into_iter().map(Value::Str).collect(),
            sizes,
            template_text: None,
            message_texts: None,
            extra_state: indexmap::IndexMap::new(),
        });
    }

    let sources = template_sources(prompt);
    if sources.is_empty() {
        return Err("the prompt has no template text to split".to_string());
    }

    let source_pairs: Vec<(&str, &str)> = sources
        .iter()
        .map(|source| (source.label.as_str(), source.text.as_str()))
        .collect();
    let (expanded_texts, extra_state) = crate::compose::expand_sources(
        &source_pairs,
        ctx,
        declared_prompts,
        effect_names,
        &electricity_template::PlainCtx,
    )?;

    let references = candidate_references(&expanded_texts)?;
    if references.is_empty() {
        return Err(
            "the template has no state references to split \u{2014} its fixed text alone is \
             over budget"
                .to_string(),
        );
    }

    let mut best_reference: Option<String> = None;
    let mut best_tokens: i64 = -1;
    for reference in &references {
        let rendered = render_reference_alone(reference, ctx)?;
        let tokens = crate::split::estimate_tokens(&rendered);
        if tokens > best_tokens {
            best_tokens = tokens;
            best_reference = Some(reference.clone());
        }
    }
    let reference = best_reference.expect("references is non-empty");

    let (occurrences, extending) = tag_occurrences_and_extensions(&expanded_texts, &reference)?;
    if let Some(first) = extending.first() {
        return Err(format!(
            "the template reaches into the split reference {} with a further path: {}",
            Value::Str(reference.clone()).py_repr(),
            Value::Str(first.clone()).py_repr()
        ));
    }
    if occurrences < 1 {
        return Err(format!(
            "the split reference {} does not occur in the template",
            Value::Str(reference.clone()).py_repr()
        ));
    }

    let value = dotted_get(ctx, &reference);
    let (kind, empty): (&'static str, Value) = match &value {
        Value::Str(_) => ("string", Value::Str(String::new())),
        Value::Dict(_) => ("mapping", Value::Dict(Dict::new())),
        Value::List(_) => ("list", Value::List(Vec::new())),
        _ => {
            return Err(format!(
                "the split reference {} is not a list, mapping, or string",
                Value::Str(reference.clone()).py_repr()
            ));
        }
    };

    let path_segments: Vec<&str> = reference.split('.').collect();
    let fixed_ctx = set_dotted_copy(ctx, &path_segments, empty);
    let mut merged_dict = fixed_ctx.as_dict().cloned().unwrap_or_default();
    for (extra_key, extra_value) in &extra_state {
        merged_dict.insert(
            Value::Str(extra_key.clone()),
            Value::Str(extra_value.clone()),
        );
    }
    let merged_ctx = Value::Dict(merged_dict);

    let mut rendered_fixed = Vec::with_capacity(sources.len());
    for (index, text) in expanded_texts.iter().enumerate() {
        let rendered = electricity_template::render_template_with_root_and_escape(
            text,
            &merged_ctx,
            &electricity_template::PlainCtx,
            &sources[index].label,
            false,
        )
        .map_err(|err| err.to_string())?;
        rendered_fixed.push(rendered);
    }

    let (fixed_text, template_text, message_texts) =
        if matches!(prompt.content, PromptContent::Template(_)) {
            (
                rendered_fixed[0].clone(),
                Some(expanded_texts[0].clone()),
                None,
            )
        } else {
            let fixed_text = sources
                .iter()
                .zip(rendered_fixed.iter())
                .map(|(source, text)| {
                    format!(
                        "{}: {text}",
                        role_str(source.role.expect("messages source has a role"))
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            let PromptContent::Messages(messages) = &prompt.content else {
                unreachable!(
                    "template_sources only returns messages-shaped sources for a messages prompt"
                )
            };
            let mut expanded_iter = expanded_texts.into_iter();
            let message_texts: Vec<Option<String>> = messages
                .iter()
                .map(|message| {
                    if message.file.is_some() {
                        None
                    } else {
                        Some(
                            expanded_iter
                                .next()
                                .expect("one expanded text per non-file message"),
                        )
                    }
                })
                .collect();
            (fixed_text, None, Some(message_texts))
        };

    let fixed_tokens =
        crate::split::estimate_tokens(&fixed_text) + file_message_fixed_tokens(prompt, ctx, name)?;
    let room = (budget - fixed_tokens).div_euclid(occurrences as i64);
    if room < 1 {
        return Err(
            "the template's fixed text (everything except the split reference) does not leave \
             room for a part within the budget"
                .to_string(),
        );
    }

    let parts: Vec<Value> = match kind {
        "list" => crate::split::split_list(value.as_list().expect("kind matched list above"), room)
            .map_err(|err| no_split_point_text(&err))?
            .into_iter()
            .map(Value::List)
            .collect(),
        "mapping" => {
            crate::split::split_mapping(value.as_dict().expect("kind matched mapping above"), room)
                .map_err(|err| no_split_point_text(&err))?
                .into_iter()
                .map(Value::Dict)
                .collect()
        }
        _ => crate::split::split_text(value.as_str().expect("kind matched string above"), room)
            .map_err(|err| no_split_point_text(&err))?
            .into_iter()
            .map(Value::Str)
            .collect(),
    };
    let sizes: Vec<i64> = parts
        .iter()
        .map(|part| match part {
            Value::Str(text) => crate::split::estimate_tokens(text),
            other => crate::split::estimate_tokens(&other.py_repr()),
        })
        .collect();

    Ok(SplitPrep {
        ref_path: reference,
        kind,
        parts,
        sizes,
        template_text,
        message_texts,
        extra_state,
    })
}

/// `core/decompose.py::_SIGIL_TAG_FORMATS`/`_rewrite_exact`: every tag
/// reading exactly *old_path*, every sigil, rewritten to *new_name*.
fn rewrite_exact(text: &str, old_path: &str, new_name: &str) -> String {
    fn regex_escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            if "\\.+*?()|[]{}^$".contains(c) {
                out.push('\\');
            }
            out.push(c);
        }
        out
    }
    fn replace_all(text: &str, re: &fancy_regex::Regex, replacement: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut last = 0usize;
        for found in re.find_iter(text) {
            let Ok(found) = found else { continue };
            out.push_str(&text[last..found.start()]);
            out.push_str(replacement);
            last = found.end();
        }
        out.push_str(&text[last..]);
        out
    }

    let escaped = regex_escape(old_path);
    let specs: [(&str, &str, &str, &str); 6] = [
        (r"\{\{\{", r"\}\}\}", "{{{", "}}}"),
        (r"\{\{&", r"\}\}", "{{&", "}}"),
        (r"\{\{#", r"\}\}", "{{#", "}}"),
        (r"\{\{\^", r"\}\}", "{{^", "}}"),
        (r"\{\{/", r"\}\}", "{{/", "}}"),
        (r"\{\{", r"\}\}", "{{", "}}"),
    ];
    let mut result = text.to_string();
    for (open_pattern, close_pattern, open_literal, close_literal) in specs {
        let pattern = format!(r"{open_pattern}\s*{escaped}\s*{close_pattern}");
        let re = fancy_regex::Regex::new(&pattern)
            .expect("a fixed tag shape around an escaped state-reference path always compiles");
        let mut replacement = String::new();
        replacement.push_str(open_literal);
        replacement.push_str(new_name);
        replacement.push_str(close_literal);
        result = replace_all(&result, &re, &replacement);
    }
    result
}

/// `core/decompose.py::_pick_parts_key`.
fn pick_parts_key(ctx: &Value) -> String {
    let existing: std::collections::HashSet<&str> = ctx
        .as_dict()
        .and_then(|dict| dict.get(&key("input")))
        .and_then(Value::as_dict)
        .map(|dict| dict.keys().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut candidate = "split_parts".to_string();
    while existing.contains(candidate.as_str()) {
        candidate.push('_');
    }
    candidate
}

/// `core/decompose.py::_pick_effect_name`.
fn pick_effect_name(base: &str, ctx: &Value) -> String {
    let existing: std::collections::HashSet<&str> = ctx
        .as_dict()
        .and_then(|dict| dict.get(&key("prime")))
        .and_then(Value::as_dict)
        .map(|dict| dict.keys().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut candidate = base.to_string();
    while existing.contains(candidate.as_str()) {
        candidate.push('_');
    }
    candidate
}

/// `core/decompose.py::_serialize_timeout_ms`.
fn serialize_timeout_ms(timeout_ms: &TimeoutMs) -> Value {
    match timeout_ms {
        TimeoutMs::Literal(n) => Value::from(*n),
        TimeoutMs::From(from) => {
            let mut dict = Dict::new();
            dict.insert(key("from"), Value::Str(from.path.clone()));
            if let Some(default) = from.default {
                dict.insert(key("default"), Value::from(default));
            }
            Value::Dict(dict)
        }
    }
}

/// A compiled [`ParamNode`] back into the plain YAML-shaped [`Value`]
/// it was compiled from -- the same round trip `_copy_source_fields`
/// needs for `defn.params`/`defn.inputs`, since those are read from
/// the already-compiled [`PromptOp`], never the raw document dict.
fn param_node_to_value(node: &ParamNode) -> Value {
    match node {
        ParamNode::Literal(value) => value.clone(),
        ParamNode::Template(text) => Value::Str(text.source.clone()),
        ParamNode::From {
            path,
            default,
            as_json,
        } => {
            let mut dict = Dict::new();
            dict.insert(key("from"), Value::Str(path.clone()));
            if let Some(default) = default {
                dict.insert(key("default"), default.clone());
            }
            if *as_json {
                dict.insert(key("as"), Value::Str("json".to_string()));
            }
            Value::Dict(dict)
        }
        ParamNode::Map(map) => Value::Dict(
            map.iter()
                .map(|(k, v)| (k.clone(), param_node_to_value(v)))
                .collect(),
        ),
        ParamNode::List(list) => Value::List(list.iter().map(param_node_to_value).collect()),
    }
}

/// `core/decompose.py::_copy_source_fields`.
fn copy_source_fields(prompt: &PromptOp) -> Dict {
    let mut out = Dict::new();
    out.insert(
        key("prompt_type"),
        Value::Str(prompt_type_str(prompt.prompt_type).to_string()),
    );
    if let Some(schema) = &prompt.schema {
        out.insert(key("schema"), schema.clone());
    }
    if let Some(model) = &prompt.model {
        out.insert(key("model"), Value::Str(model.clone()));
    }
    if let Some(provider) = &prompt.provider {
        out.insert(key("provider"), Value::Str(provider.clone()));
    }
    if !prompt.provider_fallbacks.is_empty() {
        out.insert(
            key("provider_fallbacks"),
            Value::List(
                prompt
                    .provider_fallbacks
                    .iter()
                    .cloned()
                    .map(Value::Str)
                    .collect(),
            ),
        );
    }
    if let Some(params) = &prompt.model_params {
        out.insert(key("params"), param_node_to_value(params));
    }
    if let Some(timeout_ms) = &prompt.timeout_ms {
        out.insert(key("timeout_ms"), serialize_timeout_ms(timeout_ms));
    }
    out.insert(key("deterministic"), Value::Bool(prompt.deterministic));
    if let Some(retries) = &prompt.retries {
        let mut dict = Dict::new();
        dict.insert(
            key("max_attempts"),
            Value::from(i64::from(retries.max_attempts)),
        );
        dict.insert(
            key("backoff_ms"),
            Value::from(i64::from(retries.backoff_ms)),
        );
        out.insert(key("retries"), Value::Dict(dict));
    }
    if let Some(inputs) = &prompt.inputs {
        out.insert(key("inputs"), param_node_to_value(inputs));
    }
    if !prompt.assets.is_empty() {
        out.insert(
            key("assets"),
            Value::List(
                prompt
                    .assets
                    .iter()
                    .map(|asset: &AssetRef| {
                        let mut dict = Dict::new();
                        dict.insert(key("kind"), Value::Str(asset.kind.clone()));
                        dict.insert(key("ref"), Value::Str(asset.reference.clone()));
                        Value::Dict(dict)
                    })
                    .collect(),
            ),
        );
    }
    if let Some(group) = &prompt.group {
        out.insert(key("group"), Value::Str(group.clone()));
    }
    if let Some(description) = &prompt.description {
        out.insert(key("description"), Value::Str(description.clone()));
    }
    out
}

/// `core/decompose.py::_forwarded_split_config`: the source's own
/// `split:` block, forwarded onto both generated prompts unrewritten.
fn forwarded_split_config(prompt: &PromptOp) -> Option<Dict> {
    let mut out = Dict::new();
    if let Some(template) = &prompt.split_merge_template {
        let mut merge = Dict::new();
        merge.insert(key("template"), Value::Str(template.clone()));
        out.insert(key("merge"), Value::Dict(merge));
    }
    if let Some(separator) = &prompt.split_separator {
        out.insert(key("separator"), Value::Str(separator.clone()));
    }
    if let Some(max_parts) = prompt.split_max_parts {
        out.insert(key("max_parts"), Value::from(max_parts));
    }
    if let Some(budget_tokens) = prompt.split_budget_tokens {
        out.insert(key("budget_tokens"), Value::from(budget_tokens));
    }
    if out.is_empty() { None } else { Some(out) }
}

/// `core/decompose.py::_generate_split_plan`: `(parts_key, plan dict,
/// result_path)`.
fn generate_split_plan(
    prompt: &PromptOp,
    prep: &SplitPrep,
    merge_mode: &str,
    n_parts: usize,
    max_concurrency: Option<i64>,
    ctx: &Value,
) -> Result<(String, Value, String), String> {
    let ref_path = &prep.ref_path;
    let parts_key = pick_parts_key(ctx);
    let loop_name = pick_effect_name("parts", ctx);

    let mut map_prompt = Dict::new();
    map_prompt.insert(key("type"), Value::Str("prompt".to_string()));
    map_prompt.insert(key("name"), Value::Str("map".to_string()));
    for (k, v) in copy_source_fields(prompt) {
        map_prompt.insert(k, v);
    }
    match &prompt.content {
        PromptContent::Template(_) => {
            let template_text = prep
                .template_text
                .as_ref()
                .expect("a template source always has template_text");
            map_prompt.insert(
                key("template"),
                Value::Str(rewrite_exact(template_text, ref_path, "part")),
            );
        }
        PromptContent::TemplateFile(_) => {
            map_prompt.insert(key("template"), Value::Str("{{{part}}}".to_string()));
        }
        PromptContent::Messages(messages) => {
            let message_texts = prep
                .message_texts
                .as_ref()
                .expect("a messages source always has message_texts");
            let mut list = Vec::with_capacity(messages.len());
            for (message, text) in messages.iter().zip(message_texts.iter()) {
                let mut entry = Dict::new();
                entry.insert(key("role"), Value::Str(role_str(message.role).to_string()));
                let content = match (&message.file, text) {
                    (Some(source), _) => serialize_message_file(source),
                    (None, Some(text)) => Value::Str(rewrite_exact(text, ref_path, "part")),
                    (None, None) => {
                        return Err("a non-file message is missing its expanded text".to_string());
                    }
                };
                entry.insert(key("content"), content);
                list.push(Value::Dict(entry));
            }
            map_prompt.insert(key("messages"), Value::List(list));
        }
    }
    map_prompt.insert(key("on_error"), Value::Str("fail".to_string()));
    if let Some(forwarded) = forwarded_split_config(prompt) {
        map_prompt.insert(key("split"), Value::Dict(forwarded));
    }

    let mut loop_effect = Dict::new();
    loop_effect.insert(key("type"), Value::Str("loop".to_string()));
    loop_effect.insert(key("name"), Value::Str(loop_name.clone()));
    loop_effect.insert(key("flow"), Value::Str("tree".to_string()));
    let mut each = Dict::new();
    each.insert(key("in"), Value::Str(format!("input.{parts_key}")));
    each.insert(key("as"), Value::Str("part".to_string()));
    loop_effect.insert(key("each"), Value::Dict(each));
    loop_effect.insert(key("max_iterations"), Value::from(n_parts as i64));
    if let Some(max_concurrency) = max_concurrency {
        loop_effect.insert(key("max_concurrency"), Value::from(max_concurrency));
    }
    loop_effect.insert(key("collect"), Value::Str("map".to_string()));
    loop_effect.insert(key("on_error"), Value::Str("fail".to_string()));
    loop_effect.insert(key("body"), Value::List(vec![Value::Dict(map_prompt)]));

    let mut effects = vec![Value::Dict(loop_effect)];
    let result_path = if merge_mode == "reduce" {
        let merge_name = pick_effect_name("merge", ctx);
        let mut merge_prompt = Dict::new();
        merge_prompt.insert(key("type"), Value::Str("prompt".to_string()));
        merge_prompt.insert(key("name"), Value::Str(merge_name.clone()));
        for (k, v) in copy_source_fields(prompt) {
            merge_prompt.insert(k, v);
        }
        let merge_template_src = prompt
            .split_merge_template
            .as_deref()
            .expect("merge_mode == reduce implies split.merge.template");
        let new_prefix = format!("prime.{loop_name}.collected.value");
        let rewritten =
            electricity_complexity::rewrite_tag_prefix(merge_template_src, "parts", &new_prefix)
                .map_err(|err| err.to_string())?;
        merge_prompt.insert(key("template"), Value::Str(rewritten));
        merge_prompt.insert(key("on_error"), Value::Str("fail".to_string()));
        if let Some(forwarded) = forwarded_split_config(prompt) {
            merge_prompt.insert(key("split"), Value::Dict(forwarded));
        }
        effects.push(Value::Dict(merge_prompt));
        format!("prime.{merge_name}.value")
    } else {
        format!("prime.{loop_name}.collected.value")
    };

    let mut parts_input = Dict::new();
    parts_input.insert(key("type"), Value::Str("array".to_string()));
    parts_input.insert(key("required"), Value::Bool(true));
    let mut inputs = Dict::new();
    inputs.insert(Value::Str(parts_key.clone()), Value::Dict(parts_input));
    let mut interface = Dict::new();
    interface.insert(key("inputs"), Value::Dict(inputs));

    let mut plan = Dict::new();
    plan.insert(key("interface"), Value::Dict(interface));
    plan.insert(key("effects"), Value::List(effects));

    Ok((parts_key, Value::Dict(plan), result_path))
}

/// The [`electricity_yaml::MappingAlias`] groups [`generate_split_plan`]
/// creates between the map and merge prompts, mirroring exactly what
/// `core/decompose.py::_copy_source_fields`/`_forwarded_split_config`
/// share by Python object identity: `schema`/`params`/`inputs`, each
/// only if the source prompt set it (`_copy_source_fields`'s own field
/// order, which is also `yaml.safe_dump`'s own anchor numbering order
/// since both prompts are built field-by-field in that order), then
/// `split.merge`, reached later through `split` -- present whenever
/// *merge_mode* is `"reduce"` at all, since that is exactly the
/// condition under which the source carried a `split.merge.template`
/// to forward. [`generate_split_plan`]'s own shape is fixed rather than
/// discovered (`effects[0].body[0]` is always the map prompt,
/// `effects[1]` the merge prompt whenever one exists at all), so every
/// path here is static.
fn plan_aliases(prompt: &PromptOp, merge_mode: &str) -> Vec<electricity_yaml::MappingAlias> {
    use electricity_yaml::{MappingAlias, PathSegment};

    if merge_mode != "reduce" {
        return Vec::new();
    }
    let map_path = |field: &str| {
        vec![
            PathSegment::Key(key("effects")),
            PathSegment::Index(0),
            PathSegment::Key(key("body")),
            PathSegment::Index(0),
            PathSegment::Key(key(field)),
        ]
    };
    let merge_path = |field: &str| {
        vec![
            PathSegment::Key(key("effects")),
            PathSegment::Index(1),
            PathSegment::Key(key(field)),
        ]
    };

    let mut aliases = Vec::new();
    let mut next_id = 1u32;
    if prompt.schema.is_some() {
        aliases.push(MappingAlias {
            id: next_id,
            anchor_path: map_path("schema"),
            alias_paths: vec![merge_path("schema")],
        });
        next_id += 1;
    }
    if prompt.model_params.is_some() {
        aliases.push(MappingAlias {
            id: next_id,
            anchor_path: map_path("params"),
            alias_paths: vec![merge_path("params")],
        });
        next_id += 1;
    }
    if prompt.inputs.is_some() {
        aliases.push(MappingAlias {
            id: next_id,
            anchor_path: map_path("inputs"),
            alias_paths: vec![merge_path("inputs")],
        });
        next_id += 1;
    }
    if prompt.split_merge_template.is_some() {
        let mut map_merge = map_path("split");
        map_merge.push(PathSegment::Key(key("merge")));
        let mut merge_merge = merge_path("split");
        merge_merge.push(PathSegment::Key(key("merge")));
        aliases.push(MappingAlias {
            id: next_id,
            anchor_path: map_merge,
            alias_paths: vec![merge_merge],
        });
    }
    aliases
}

/// `core/decompose.py::_apply_native_merge`/`_MergeInvalidError`.
fn apply_native_merge(
    merge_mode: &str,
    parts_value: &Value,
    schema: Option<&Value>,
    separator: Option<&str>,
) -> Result<Value, String> {
    let Value::List(items) = parts_value else {
        return Err("the collected parts are not a list".to_string());
    };

    if merge_mode == "join" {
        let mut texts = Vec::with_capacity(items.len());
        for item in items {
            let Value::Str(text) = item else {
                return Err(format!(
                    "a part's value is {}, not a string, and the output type is text",
                    item.type_name()
                ));
            };
            texts.push(text.clone());
        }
        return Ok(Value::Str(texts.join(separator.unwrap_or("\n\n"))));
    }

    if merge_mode == "concat" {
        let mut merged = Vec::new();
        for item in items {
            let Value::List(part_items) = item else {
                return Err(format!(
                    "a part's value is {}, not an array, and the output type is an array",
                    item.type_name()
                ));
            };
            merged.extend(part_items.iter().cloned());
        }
        let merged_value = Value::List(merged);
        if let Some(schema) = schema {
            match electricity_schema::validate_prompt_schema(schema, &merged_value) {
                electricity_schema::PromptSchemaCheck::Valid => {}
                electricity_schema::PromptSchemaCheck::Invalid(message) => {
                    return Err(format!("the merged value failed its schema: {message}"));
                }
                electricity_schema::PromptSchemaCheck::SchemaError(message) => {
                    return Err(format!("the merged value's schema is invalid: {message}"));
                }
            }
        }
        return Ok(merged_value);
    }

    Err(format!("unknown merge mode {merge_mode:?}"))
}

/// `core/decompose.py::_maybe_split` -- the size trigger. See
/// [`maybe_decompose`], which runs this first. `Ok(None)` means size
/// does not apply at all; everything else is a real attempt.
#[allow(clippy::too_many_arguments)]
async fn maybe_split(
    op: &Op,
    prompt: &PromptOp,
    prompt_sent: &str,
    resolved_model: &str,
    render_chain: &CtxChain,
    store: &Store,
    frame: &Frame,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
) -> Result<Option<DecompositionResult>, VmError> {
    let name = op.name.as_deref().unwrap_or("");
    let sset = &run_ctx.complexity.splitting;

    if prompt.split_enabled == Some(false) {
        return Ok(None);
    }

    let Some(budget) = resolve_split_budget(prompt, sset, resolved_model) else {
        return Ok(None);
    };

    let estimated = crate::split::estimate_tokens(prompt_sent);
    if estimated <= budget {
        return Ok(None);
    }

    let depth = run_ctx.decomposition_depth;
    let max_parts = prompt.split_max_parts.unwrap_or(sset.max_parts);

    let mut base = split_base_meta(estimated, budget, depth, sset);

    if i64::from(depth) >= sset.max_depth {
        return Ok(Some(split_failure_result(
            base,
            sset,
            "max_depth",
            "max depth reached before splitting".to_string(),
        )));
    }

    let has_merge_template = prompt
        .split_merge_template
        .as_deref()
        .is_some_and(|text| !py_strip(text).is_empty());
    let merge_mode: &'static str = if has_merge_template {
        "reduce"
    } else {
        match native_merge_mode(prompt) {
            Some(mode) => mode,
            None => {
                return Ok(Some(split_failure_result(
                    base,
                    sset,
                    "no_merge",
                    format!(
                        "prompt_type {} needs split.merge.template to merge parts",
                        Value::Str(prompt_type_str(prompt.prompt_type).to_string()).py_repr()
                    ),
                )));
            }
        }
    };
    base.insert(key("merge"), Value::Str(merge_mode.to_string()));

    let ctx = live_ctx(render_chain, store);
    let prep = match prepare_split(
        prompt,
        name,
        &ctx,
        run_ctx.declared_prompts,
        run_ctx.effect_names,
        budget,
    ) {
        Ok(prep) => prep,
        Err(message) => {
            return Ok(Some(split_failure_result(
                base,
                sset,
                "no_split_point",
                message,
            )));
        }
    };

    let n_parts = prep.parts.len();
    if n_parts as i64 > max_parts {
        return Ok(Some(split_failure_result(
            base,
            sset,
            "too_many_parts",
            format!("splitting produced {n_parts} parts; the budget is {max_parts}"),
        )));
    }

    let mut split_record = Dict::new();
    split_record.insert(key("path"), Value::Str(prep.ref_path.clone()));
    split_record.insert(key("kind"), Value::Str(prep.kind.to_string()));
    split_record.insert(key("parts"), Value::from(n_parts as i64));
    split_record.insert(
        key("sizes"),
        Value::List(prep.sizes.iter().map(|size| Value::from(*size)).collect()),
    );
    base.insert(key("split"), Value::Dict(split_record));
    base.insert(key("chunk_count"), Value::from(n_parts as i64));

    let (parts_key, plan_dict, result_path) = match generate_split_plan(
        prompt,
        &prep,
        merge_mode,
        n_parts,
        sset.max_concurrency,
        &ctx,
    ) {
        Ok(generated) => generated,
        Err(message) => {
            return Ok(Some(split_failure_result(
                base,
                sset,
                "execution_failed",
                message,
            )));
        }
    };

    let aliases = plan_aliases(prompt, merge_mode);
    let plan_yaml = match electricity_yaml::dump_aliased(
        &plan_dict,
        &DumpOptions { sort_keys: false },
        &aliases,
    ) {
        Ok(text) => text,
        Err(err) => {
            return Ok(Some(split_failure_result(
                base,
                sset,
                "execution_failed",
                err.to_string(),
            )));
        }
    };
    base.insert(key("yaml"), Value::Str(plan_yaml.clone()));
    base.insert(key("result_path"), Value::Str(result_path.clone()));

    let problems = validate_generated_yaml(&plan_yaml, &result_path, run_ctx.runtime_config);
    if !problems.is_empty() {
        log::warn!(
            "Generated split plan for prompt {} failed validation: {}",
            Value::Str(name.to_string()).py_repr(),
            problems.join("; ")
        );
        return Ok(Some(split_failure_result(
            base,
            sset,
            "execution_failed",
            problems.join("; "),
        )));
    }

    let identity = plan_identity(&plan_yaml);
    if let Some(message) = split_cycle_error(name, frame.use_call_stack(), &identity) {
        log::warn!(
            "Decomposition (size trigger) child failed for prompt {}",
            Value::Str(name.to_string()).py_repr()
        );
        return Ok(Some(split_failure_result(
            base,
            sset,
            "execution_failed",
            message,
        )));
    }

    let plan_program =
        match electricity_compiler::compile_document(&plan_dict, &DocumentOrigin::Generated) {
            Ok(program) => program,
            Err(err) => {
                return Ok(Some(split_failure_result(
                    base,
                    sset,
                    "execution_failed",
                    err.0,
                )));
            }
        };
    if let Some(refusal) = first_unsupported(&plan_program, false, &run_ctx.supported) {
        return Err(VmError::Refused {
            refusal,
            child_label: Some(format!("split plan of '{name}'")),
        });
    }

    let declared = declared_effect_names(&plan_dict);
    let mut seed_state = child_seed_state(&ctx, &result_path);
    if let Value::Dict(seed_dict) = &mut seed_state {
        let input_entry = seed_dict
            .entry(key("input"))
            .or_insert_with(|| Value::Dict(Dict::new()));
        if let Value::Dict(input_dict) = input_entry {
            input_dict.insert(
                Value::Str(parts_key.clone()),
                Value::List(prep.parts.clone()),
            );
        }
        for (extra_key, extra_value) in &prep.extra_state {
            seed_dict.insert(
                Value::Str(extra_key.clone()),
                Value::Str(extra_value.clone()),
            );
        }
        if let Some(Value::Dict(prime)) = seed_dict.get_mut(&key("prime")) {
            for declared_name in &declared {
                prime.shift_remove(&Value::Str(declared_name.clone()));
            }
        }
    }

    let plan_frame = child_frame(frame, &op.path).push_use(identity);
    let plan_run_ctx = child_run_ctx(
        run_ctx,
        run_ctx.complexity,
        depth + 1,
        &plan_program.prompts,
        &plan_program.effect_names,
    );

    let child_store = match run_isolated(
        &plan_program,
        seed_state,
        &plan_frame,
        &plan_run_ctx,
        observer,
        token,
    )
    .await
    {
        Err(VmError::Refused {
            refusal,
            child_label,
        }) => {
            return Err(VmError::Refused {
                refusal,
                child_label: child_label.or_else(|| Some(format!("split plan of '{name}'"))),
            });
        }
        Err(err) if err.is_fatal() => return Err(err),
        Err(err) => {
            log::warn!(
                "Decomposition (size trigger) child failed for prompt {}",
                Value::Str(name.to_string()).py_repr()
            );
            return Ok(Some(split_failure_result(
                base,
                sset,
                "execution_failed",
                err.to_string(),
            )));
        }
        Ok(store) => store,
    };

    let collected = resolve_dot_path(&child_store.snapshot(&child_store.root), &result_path);
    if matches!(collected, Value::None) {
        log::warn!(
            "Decomposition (size trigger) child failed for prompt {}",
            Value::Str(name.to_string()).py_repr()
        );
        return Ok(Some(split_failure_result(
            base,
            sset,
            "execution_failed",
            split_missing_result_error(&result_path),
        )));
    }

    let value = if merge_mode == "reduce" {
        collected
    } else {
        match apply_native_merge(
            merge_mode,
            &collected,
            prompt.schema.as_ref(),
            prompt.split_separator.as_deref(),
        ) {
            Ok(value) => value,
            Err(message) => {
                return Ok(Some(split_failure_result(
                    base,
                    sset,
                    "merge_invalid",
                    message,
                )));
            }
        }
    };

    let mut result = success_result(base, value);
    if record_children_enabled(run_ctx.runtime_config) {
        if let Ok(Some(child_prime)) =
            child_store.child(&child_store.root, &Value::Str("prime".to_string()))
        {
            result.child_record = declared_children_entries(&child_prime, &declared);
        }
    }
    Ok(Some(result))
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
    fn planner_settings_disables_splitting_too() {
        let mut settings = ComplexitySettings::default();
        settings.splitting.enabled = true;
        settings.splitting.max_input_tokens = Some(10);
        settings.splitting.on_failure = "fail".to_string();
        let planner = planner_settings(&settings);
        assert!(!planner.splitting.enabled);
        assert_eq!(planner.splitting, SplittingSettings::default());
    }

    #[test]
    fn dotted_get_walks_a_dict_then_a_list_index() {
        let ctx = dict(vec![(
            "input",
            dict(vec![(
                "docs",
                Value::List(vec![
                    Value::Str("first".to_string()),
                    Value::Str("second".to_string()),
                ]),
            )]),
        )]);
        assert_eq!(
            dotted_get(&ctx, "input.docs.0"),
            Value::Str("first".to_string())
        );
        assert_eq!(
            dotted_get(&ctx, "input.docs.1"),
            Value::Str("second".to_string())
        );
    }

    #[test]
    fn dotted_get_walks_a_list_index_with_a_negative_index() {
        let ctx = Value::List(vec![
            Value::from(1i64),
            Value::from(2i64),
            Value::from(3i64),
        ]);
        assert_eq!(dotted_get(&ctx, "-1"), Value::from(3i64));
    }

    #[test]
    fn dotted_get_is_none_on_a_non_integer_list_segment_or_an_out_of_range_index() {
        let list = Value::List(vec![Value::from(1i64)]);
        assert_eq!(dotted_get(&list, "x"), Value::None);
        assert_eq!(dotted_get(&list, "5"), Value::None);
    }

    #[test]
    fn dotted_get_is_none_past_a_scalar() {
        let ctx = dict(vec![("n", Value::from(1i64))]);
        assert_eq!(dotted_get(&ctx, "n.x"), Value::None);
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
            validate_plan(&p, 5, &Value::None).unwrap(),
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
            validate_plan(&blank, 5, &Value::None).unwrap(),
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
            let problems = validate_plan(&p, 5, &Value::None).unwrap();
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
                .unwrap()
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
                .unwrap()
                .iter()
                .any(|p| p.contains("the budget is 5"))
        );
    }

    #[test]
    fn validate_plan_reports_a_calendar_invalid_timestamp_with_no_location() {
        // Issue #528's own comment: PyYAML's bare `ValueError` for a
        // calendar-invalid timestamp carries no location -- unlike
        // every other `YamlError`, whose own `Display` text does.
        let yaml = "effects: []\nwhen: 2024-02-30\n";
        let p = ok_plan(yaml, "prime.merge.value");
        let err = validate_plan(&p, 5, &Value::None).unwrap_err();
        assert_eq!(err, "day is out of range for month");
    }

    #[test]
    fn validate_plan_rejects_a_result_path_not_under_prime() {
        let yaml = "effects:\n  - {type: tool, name: merge, provider: json, params: {mode: parse, input: '1'}}\n";
        let p = ok_plan(yaml, "input.merge.value");
        let problems = validate_plan(&p, 5, &Value::None).unwrap();
        assert!(
            problems.iter().any(|e| e.contains("does not point into")),
            "{problems:?}"
        );
    }

    #[test]
    fn validate_plan_rejects_a_result_path_with_no_matching_top_level_effect() {
        let yaml = "effects:\n  - {type: tool, name: merge, provider: json, params: {mode: parse, input: '1'}}\n";
        let p = ok_plan(yaml, "prime.nope.value");
        let problems = validate_plan(&p, 5, &Value::None).unwrap();
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
        assert_eq!(
            validate_plan(&p, 5, &Value::None).unwrap(),
            Vec::<String>::new()
        );
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
        let problems = validate_plan(&p, 5, &runtime_config).unwrap();
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
            vec![
                "decomposed",
                "trigger",
                "score",
                "threshold",
                "depth",
                "max_depth"
            ]
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

    #[test]
    fn split_cycle_error_fires_only_when_identity_is_already_on_the_stack() {
        assert_eq!(split_cycle_error("ask", &[], "inline:abc"), None);
        let stack = vec!["inline:abc".to_string()];
        let message = split_cycle_error("ask", &stack, "inline:abc").unwrap();
        assert_eq!(
            message,
            "split of 'ask': cycle detected \u{2014} inline:abc \u{2192} inline:abc"
        );
    }

    #[test]
    fn split_missing_result_error_has_the_expected_shape() {
        assert_eq!(
            split_missing_result_error("prime.merge.value"),
            "the split child completed but left nothing at result_path 'prime.merge.value'"
        );
    }

    #[test]
    fn complexity_score_reads_a_float_or_small_int_score_key() {
        let meta = dict(vec![("score", Value::Float(12.5))]);
        assert_eq!(complexity_score(Some(&meta)), Some(12.5));
        let meta = dict(vec![("score", Value::from(7i64))]);
        assert_eq!(complexity_score(Some(&meta)), Some(7.0));
    }

    #[test]
    fn complexity_score_is_none_without_a_numeric_score() {
        assert_eq!(complexity_score(None), None);
        let meta = dict(vec![("no_score", Value::Float(1.0))]);
        assert_eq!(complexity_score(Some(&meta)), None);
        let meta = dict(vec![("score", Value::Str("nope".to_string()))]);
        assert_eq!(complexity_score(Some(&meta)), None);
        let meta = dict(vec![("score", Value::None)]);
        assert_eq!(complexity_score(Some(&meta)), None);
        assert_eq!(complexity_score(Some(&Value::None)), None);
    }

    fn template_op(source: &str) -> PromptOp {
        PromptOp {
            content: PromptContent::Template(electricity_bytecode::template::TemplateText::new(
                source,
                true,
                electricity_bytecode::template::Escape::None,
            )),
            prompt_type: electricity_bytecode::effects::PromptType::Text,
            schema: None,
            model: None,
            provider: None,
            provider_fallbacks: Vec::new(),
            routing_override: None,
            model_params: None,
            timeout_ms: None,
            deterministic: false,
            inputs: None,
            assets: Vec::new(),
            retries: None,
            description: None,
            group: None,
            split_budget_tokens: None,
            split_enabled: None,
            split_max_parts: None,
            split_separator: None,
            split_merge_template: None,
        }
    }

    #[test]
    fn planner_source_template_prefers_the_raw_template_source() {
        let op = template_op("hi {{input.topic}}");
        assert_eq!(
            planner_source_template(&op, "hi topic-rendered"),
            "hi {{input.topic}}"
        );
    }

    #[test]
    fn planner_source_template_falls_back_to_prompt_sent_when_the_template_is_empty() {
        let op = template_op("");
        assert_eq!(planner_source_template(&op, "materialized"), "materialized");
    }

    #[test]
    fn planner_source_template_falls_back_to_prompt_sent_for_a_messages_prompt() {
        let mut op = template_op("unused");
        op.content = PromptContent::Messages(Vec::new());
        assert_eq!(planner_source_template(&op, "materialized"), "materialized");
    }

    #[test]
    fn rejected_payload_text_reads_the_schema_rejected_cause() {
        let err = VmError::Raised {
            py_class: electricity_tools::PyExc::SchemaValidationError,
            message: "bad".to_string(),
            cause: electricity_tools::ErrorCause::SchemaRejected {
                raw_response_text: "the whole rejected reply".to_string(),
            },
            cause_class: None,
        };
        assert_eq!(
            rejected_payload_text(&err),
            Some("the whole rejected reply".to_string())
        );
    }

    #[test]
    fn rejected_payload_text_is_none_without_that_cause() {
        let err = VmError::Raised {
            py_class: electricity_tools::PyExc::RuntimeError,
            message: "boom".to_string(),
            cause: electricity_tools::ErrorCause::None,
            cause_class: None,
        };
        assert_eq!(rejected_payload_text(&err), None);
        assert_eq!(rejected_payload_text(&VmError::Cancelled), None);
    }

    #[test]
    fn rejected_payload_text_survives_a_wrap_since_cause_passes_through_unchanged() {
        // `exec::wrap_effect_error` rewrites `py_class` to `RuntimeError`
        // and sets `cause_class`, but never touches `cause` itself --
        // the same shape a chain/tree wrap of the planner's own
        // rejected reply would carry.
        let err = VmError::Raised {
            py_class: electricity_tools::PyExc::RuntimeError,
            message: "prime.plan: bad".to_string(),
            cause: electricity_tools::ErrorCause::SchemaRejected {
                raw_response_text: "still here after a wrap".to_string(),
            },
            cause_class: Some(electricity_tools::PyExc::SchemaValidationError),
        };
        assert_eq!(
            rejected_payload_text(&err),
            Some("still here after a wrap".to_string())
        );
    }

    fn minimal_run_ctx_parts() -> (
        electricity_tools::ToolRegistry,
        crate::Limiter,
        Value,
        crate::adapter::AdapterRegistry,
        std::path::PathBuf,
        indexmap::IndexMap<String, String>,
        std::collections::BTreeSet<String>,
    ) {
        (
            electricity_tools::ToolRegistry::new(),
            crate::Limiter::new(),
            Value::None,
            crate::adapter::AdapterRegistry::new(),
            std::path::PathBuf::from("."),
            indexmap::IndexMap::new(),
            std::collections::BTreeSet::new(),
        )
    }

    #[test]
    fn child_run_ctx_is_unlocked_and_never_a_dry_run_whatever_the_caller_was() {
        // `_run_isolated`'s own `DynamicRuntime(model_locked=False)`
        // default (issue #522's Settled question 2) -- even a caller
        // locked by a profile's run-level `model:`, mid-dry-run, hands
        // the planner/plan an unlocked, non-dry-run context.
        let (registry, limiter, runtime_config, adapters, dir, prompts, names) =
            minimal_run_ctx_parts();
        let complexity = ComplexitySettings::default();
        let caller = RunContext {
            registry: &registry,
            limiter: &limiter,
            model: "m",
            adapter: "_noop",
            runtime_config: &runtime_config,
            dry_run: true,
            armed: false,
            adapters: &adapters,
            default_adapter: Rc::new(crate::adapter::NoopAdapter),
            model_locked: true,
            adapter_timeout_seconds: 120,
            complexity: &complexity,
            decomposition_depth: 3,
            orchestration_dir: &dir,
            declared_prompts: &prompts,
            effect_names: &names,
            supported: electricity_bytecode::Supported::current(&[]),
        };
        let child_complexity = ComplexitySettings::default();
        let child = child_run_ctx(
            &caller,
            &child_complexity,
            caller.decomposition_depth,
            &prompts,
            &names,
        );
        assert!(!child.model_locked);
        assert!(!child.dry_run);
        // Every other field is the caller's own, unchanged.
        assert_eq!(child.model, "m");
        assert_eq!(child.adapter_timeout_seconds, 120);
    }

    #[test]
    fn child_run_ctx_takes_the_decomposition_depth_the_caller_passes_in() {
        // The planner keeps the run's own depth unchanged; the plan
        // increments it (issue #522's own Scope section 2 steps 4/7) --
        // this helper just takes whatever the caller computed, so both
        // calls go through the one assertion here.
        let (registry, limiter, runtime_config, adapters, dir, prompts, names) =
            minimal_run_ctx_parts();
        let complexity = ComplexitySettings::default();
        let caller = RunContext {
            registry: &registry,
            limiter: &limiter,
            model: "m",
            adapter: "_noop",
            runtime_config: &runtime_config,
            dry_run: false,
            armed: false,
            adapters: &adapters,
            default_adapter: Rc::new(crate::adapter::NoopAdapter),
            model_locked: false,
            adapter_timeout_seconds: 120,
            complexity: &complexity,
            decomposition_depth: 5,
            orchestration_dir: &dir,
            declared_prompts: &prompts,
            effect_names: &names,
            supported: electricity_bytecode::Supported::current(&[]),
        };
        let planner_child = child_run_ctx(
            &caller,
            &complexity,
            caller.decomposition_depth,
            &prompts,
            &names,
        );
        assert_eq!(planner_child.decomposition_depth, 5);
        let plan_child = child_run_ctx(
            &caller,
            &complexity,
            caller.decomposition_depth + 1,
            &prompts,
            &names,
        );
        assert_eq!(plan_child.decomposition_depth, 6);
    }

    #[test]
    fn child_frame_bumps_display_depth_by_one_and_composes_the_path() {
        let frame = Frame::root().with_display_depth(2);
        let op_path = electricity_bytecode::EffectPath::root().push_name("big");
        let child = child_frame(&frame, &op_path);
        assert_eq!(child.display_depth(), 3);
        assert_eq!(child.use_call_stack(), frame.use_call_stack());
    }

    #[test]
    fn only_the_plans_own_frame_pushes_a_use_call_stack_entry() {
        let frame = Frame::root();
        let op_path = electricity_bytecode::EffectPath::root().push_name("big");
        let planner_frame = child_frame(&frame, &op_path);
        assert!(planner_frame.use_call_stack().is_empty());
        let plan_frame = child_frame(&frame, &op_path).push_use("inline:abc".to_string());
        assert_eq!(plan_frame.use_call_stack(), &["inline:abc".to_string()]);
    }

    // -- size trigger (issue #682) --------------------------------------

    #[test]
    fn resolve_split_budget_prefers_the_prompts_own_budget_even_with_the_switch_off() {
        let mut prompt = template_op("hi");
        prompt.split_budget_tokens = Some(42);
        let sset = SplittingSettings {
            enabled: false,
            ..SplittingSettings::default()
        };
        assert_eq!(resolve_split_budget(&prompt, &sset, "m"), Some(42));
    }

    #[test]
    fn resolve_split_budget_is_none_with_the_switch_off_and_no_own_budget() {
        let prompt = template_op("hi");
        let sset = SplittingSettings::default();
        assert_eq!(resolve_split_budget(&prompt, &sset, "m"), None);
    }

    #[test]
    fn resolve_split_budget_prefers_the_models_table_over_max_input_tokens() {
        let prompt = template_op("hi");
        let mut models = indexmap::IndexMap::new();
        models.insert("m".to_string(), 10);
        let sset = SplittingSettings {
            enabled: true,
            models,
            max_input_tokens: Some(99),
            ..SplittingSettings::default()
        };
        assert_eq!(resolve_split_budget(&prompt, &sset, "m"), Some(10));
        assert_eq!(resolve_split_budget(&prompt, &sset, "other"), Some(99));
    }

    #[test]
    fn native_merge_mode_covers_text_array_and_array_shaped_json() {
        let mut text_prompt = template_op("hi");
        text_prompt.prompt_type = PromptType::Text;
        assert_eq!(native_merge_mode(&text_prompt), Some("join"));

        let mut array_prompt = template_op("hi");
        array_prompt.prompt_type = PromptType::Array;
        assert_eq!(native_merge_mode(&array_prompt), Some("concat"));

        let mut json_array_prompt = template_op("hi");
        json_array_prompt.prompt_type = PromptType::Json;
        let mut schema = Dict::new();
        schema.insert(key("type"), Value::Str("array".to_string()));
        json_array_prompt.schema = Some(Value::Dict(schema));
        assert_eq!(native_merge_mode(&json_array_prompt), Some("concat"));

        let mut json_object_prompt = template_op("hi");
        json_object_prompt.prompt_type = PromptType::Json;
        let mut object_schema = Dict::new();
        object_schema.insert(key("type"), Value::Str("object".to_string()));
        json_object_prompt.schema = Some(Value::Dict(object_schema));
        assert_eq!(native_merge_mode(&json_object_prompt), None);

        let mut boolean_prompt = template_op("hi");
        boolean_prompt.prompt_type = PromptType::Boolean;
        assert_eq!(native_merge_mode(&boolean_prompt), None);
    }

    #[test]
    fn pick_parts_key_appends_underscores_until_no_collision() {
        let input_ns = {
            let mut d = Dict::new();
            d.insert(key("split_parts"), Value::Bool(true));
            d.insert(key("split_parts_"), Value::Bool(true));
            Value::Dict(d)
        };
        let ctx = {
            let mut d = Dict::new();
            d.insert(key("input"), input_ns);
            Value::Dict(d)
        };
        assert_eq!(pick_parts_key(&ctx), "split_parts__");
        assert_eq!(pick_parts_key(&Value::Dict(Dict::new())), "split_parts");
    }

    #[test]
    fn pick_effect_name_appends_underscores_until_no_collision() {
        let prime = {
            let mut d = Dict::new();
            d.insert(key("parts"), Value::Bool(true));
            Value::Dict(d)
        };
        let ctx = {
            let mut d = Dict::new();
            d.insert(key("prime"), prime);
            Value::Dict(d)
        };
        assert_eq!(pick_effect_name("parts", &ctx), "parts_");
        assert_eq!(pick_effect_name("merge", &ctx), "merge");
    }

    #[test]
    fn rewrite_exact_rewrites_every_sigil_but_leaves_other_names_alone() {
        let text = "{{input.doc}} {{{input.doc}}} {{&input.doc}} {{#input.doc}}x{{/input.doc}} \
                    {{^input.doc}}y{{/input.doc}} {{other}}";
        let rewritten = rewrite_exact(text, "input.doc", "part");
        assert_eq!(
            rewritten,
            "{{part}} {{{part}}} {{&part}} {{#part}}x{{/part}} {{^part}}y{{/part}} {{other}}"
        );
    }

    #[test]
    fn apply_native_merge_joins_text_parts_with_the_default_or_given_separator() {
        let parts = Value::List(vec![
            Value::Str("a".to_string()),
            Value::Str("b".to_string()),
        ]);
        assert_eq!(
            apply_native_merge("join", &parts, None, None).unwrap(),
            Value::Str("a\n\nb".to_string())
        );
        assert_eq!(
            apply_native_merge("join", &parts, None, Some("-")).unwrap(),
            Value::Str("a-b".to_string())
        );
    }

    #[test]
    fn apply_native_merge_join_rejects_a_non_string_part() {
        let parts = Value::List(vec![Value::from(1i64)]);
        let err = apply_native_merge("join", &parts, None, None).unwrap_err();
        assert!(err.contains("not a string"), "{err}");
    }

    #[test]
    fn apply_native_merge_concat_flattens_and_validates_against_the_schema() {
        let parts = Value::List(vec![
            Value::List(vec![Value::from(1i64)]),
            Value::List(vec![Value::from(2i64)]),
        ]);
        let mut schema = Dict::new();
        schema.insert(key("type"), Value::Str("array".to_string()));
        schema.insert(
            key("items"),
            Value::Dict({
                let mut d = Dict::new();
                d.insert(key("type"), Value::Str("string".to_string()));
                d
            }),
        );
        let schema = Value::Dict(schema);
        let err = apply_native_merge("concat", &parts, Some(&schema), None).unwrap_err();
        assert!(err.contains("failed its schema"), "{err}");
    }

    #[test]
    fn apply_native_merge_concat_accepts_a_matching_schema() {
        let parts = Value::List(vec![
            Value::List(vec![Value::from(1i64)]),
            Value::List(vec![Value::from(2i64)]),
        ]);
        let mut schema = Dict::new();
        schema.insert(key("type"), Value::Str("array".to_string()));
        let schema = Value::Dict(schema);
        assert_eq!(
            apply_native_merge("concat", &parts, Some(&schema), None).unwrap(),
            Value::List(vec![Value::from(1i64), Value::from(2i64)])
        );
    }

    #[test]
    fn plan_aliases_is_empty_for_a_native_merge() {
        let prompt = template_op("x");
        assert_eq!(plan_aliases(&prompt, "join"), Vec::new());
        assert_eq!(plan_aliases(&prompt, "concat"), Vec::new());
    }

    #[test]
    fn plan_aliases_covers_only_the_fields_the_source_actually_set() {
        let mut prompt = template_op("x");
        prompt.split_merge_template = Some("y".to_string());
        let aliases = plan_aliases(&prompt, "reduce");
        // No schema/params/inputs on this source -- only `split.merge`.
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].id, 1);
    }

    #[test]
    fn plan_aliases_numbers_schema_params_inputs_then_merge_in_that_order() {
        use electricity_bytecode::param::ParamNode;
        let mut prompt = template_op("x");
        prompt.schema = Some(Value::Dict(Dict::new()));
        prompt.model_params = Some(ParamNode::Literal(Value::from(1i64)));
        prompt.inputs = Some(ParamNode::Literal(Value::from(2i64)));
        prompt.split_merge_template = Some("y".to_string());
        let aliases = plan_aliases(&prompt, "reduce");
        let ids: Vec<u32> = aliases.iter().map(|a| a.id).collect();
        assert_eq!(ids, vec![1, 2, 3, 4]);
    }

    #[test]
    fn plan_aliases_anchor_and_alias_paths_reach_map_then_merge_prompt() {
        use electricity_yaml::PathSegment;
        let mut prompt = template_op("x");
        prompt.schema = Some(Value::Dict(Dict::new()));
        let aliases = plan_aliases(&prompt, "reduce");
        assert_eq!(aliases.len(), 1);
        let schema_alias = &aliases[0];
        assert_eq!(
            schema_alias.anchor_path,
            vec![
                PathSegment::Key(Value::Str("effects".to_string())),
                PathSegment::Index(0),
                PathSegment::Key(Value::Str("body".to_string())),
                PathSegment::Index(0),
                PathSegment::Key(Value::Str("schema".to_string())),
            ]
        );
        assert_eq!(
            schema_alias.alias_paths,
            vec![vec![
                PathSegment::Key(Value::Str("effects".to_string())),
                PathSegment::Index(1),
                PathSegment::Key(Value::Str("schema".to_string())),
            ]]
        );
    }
}
