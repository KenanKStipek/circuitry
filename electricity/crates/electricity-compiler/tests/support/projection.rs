//! Lane C: projects a compiled [`Program`] back into the shape
//! `_compiler_corpus.py`'s `_dump_definition` dumps Circuitry's own
//! `DynamicDefinition` tree as -- plain dataclass-shaped JSON, field
//! for field, per `electricity-bytecode`'s own
//! `tests/definition_fields.rs` mapping. Used only by `golden_compile.rs`
//! (lane C); nothing outside this crate's own tests reads it.
//!
//! Two deliberate, narrow exceptions from a literal field-for-field
//! dump, both already documented as such in `electricity-bytecode`'s
//! own crate docs -- [`definitions_match`] (not a plain `==`) knows
//! about both:
//!
//! - `max_concurrency`/`stop_on_error` on a **chain**-flow
//!   `DynamicDefinition`: Circuitry carries both fields unconditionally
//!   (documented as meaningful only under `flow: tree`); this IR's
//!   `Region::Block` (chain) has no field for either, so a chain-flow
//!   dynamic's own value for both is unrecoverable and skipped rather
//!   than reproduced.
//! - `retries`: `None` in Circuitry's own dump when a document omits
//!   `retries:` entirely; this IR always materializes
//!   `RetryPolicy::default()` (`max_attempts: 1, backoff_ms: 1000`) --
//!   semantically identical defaults, but a structural `null` vs.
//!   `{...}` difference. A `null` on the Circuitry side is accepted
//!   unconditionally; a concrete object is compared structurally.

// golden_smoke.rs/no_leaked_paths.rs (lane A) compile this module too
// (via `mod support;`) but never call into it -- only golden_compile.rs
// (this lane) does, the same reason `reference.rs` carries this same
// allow.
#![allow(dead_code)]

use electricity_bytecode::effects::{PromptContent, PromptType, Role, UseSource};
use electricity_bytecode::param::ParamNode;
use electricity_bytecode::region::{Condition, ExpectCondition, LoopFlow, LoopSpec, Region};
use electricity_bytecode::{LeafKind, NodeKind, OnError, Op, Program};
use electricity_value::Value;
use serde_json::{Map, Value as Json, json};

fn on_error_str(on_error: OnError) -> &'static str {
    match on_error {
        OnError::Fail => "fail",
        OnError::Skip => "skip",
        OnError::Continue => "continue",
        OnError::Break => "break",
    }
}

/// `Value` (the document/runtime value type) -> plain JSON, the way
/// Python's own `json.dumps` would render the same data read off YAML:
/// a Python `int`/`float`/`str`/`bool`/`None`/`list`/`dict` round-trips
/// through `json.dumps` with no special-casing -- a `bytes`/`date`/
/// `datetime` leaf is not reachable from a `params`/`inputs`/`schema`
/// value in any golden case this corpus generates, so is not handled
/// specially here.
fn value_to_json(value: &Value) -> Json {
    match value {
        Value::None => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(i) => match i {
            electricity_value::IntValue::Small(n) => Json::Number((*n).into()),
            electricity_value::IntValue::Big(big) => {
                use num_traits::ToPrimitive;
                big.to_i64().map(Json::from).unwrap_or(Json::Null)
            }
        },
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        Value::Str(s) => Json::String(s.clone()),
        Value::Bytes(b) => Json::String(String::from_utf8_lossy(b).to_string()),
        Value::List(items) => Json::Array(items.iter().map(value_to_json).collect()),
        Value::Dict(dict) => {
            let mut map = Map::new();
            for (key, item) in dict {
                let key_str = match key {
                    Value::Str(s) => s.clone(),
                    other => other.py_str(),
                };
                map.insert(key_str, value_to_json(item));
            }
            Json::Object(map)
        }
        Value::Date(d) => Json::String(d.to_string()),
        Value::DateTime(naive, _) => Json::String(naive.to_string()),
    }
}

/// Converts a [`ParamNode`] back to the raw value Circuitry's own
/// `ToolDefinition.params`/`UseDefinition.inputs`/`PromptDefinition.
/// inputs`/`.params` carry -- Python never restructures `params`/
/// `inputs` into a typed tree the way this IR does for the VM's sake;
/// it keeps the raw, as-written mapping, so comparing against its dump
/// means undoing this IR's own `Template`/`From` wrapping.
fn param_node_to_json(node: &ParamNode) -> Json {
    match node {
        ParamNode::Literal(value) => value_to_json(value),
        ParamNode::Template(text) => Json::String(text.source.clone()),
        ParamNode::From { path, default } => {
            let mut map = Map::new();
            map.insert("from".to_string(), Json::String(path.clone()));
            if let Some(default) = default {
                map.insert("default".to_string(), value_to_json(default));
            }
            Json::Object(map)
        }
        ParamNode::Map(entries) => {
            let mut map = Map::new();
            for (key, child) in entries {
                map.insert(key.clone(), param_node_to_json(child));
            }
            Json::Object(map)
        }
        ParamNode::List(items) => Json::Array(items.iter().map(param_node_to_json).collect()),
    }
}

const REFLECTOR_PRIME_MARKER: &str = "<REFLECTOR_PRIME>";

fn template_source_or_marker(source: &str) -> Json {
    if source == electricity_compiler::reflector_prime::REFLECTOR_PRIME {
        Json::String(REFLECTOR_PRIME_MARKER.to_string())
    } else {
        Json::String(source.to_string())
    }
}

fn project_condition(cond: &Condition) -> Json {
    match cond {
        Condition::Cel { expr, strict } => json!({
            "mode": "cel",
            "template": Json::Null,
            "expr": expr,
            "strict": strict,
        }),
        Condition::Model { template } => json!({
            "mode": "model",
            "template": template.source,
            "expr": Json::Null,
            "strict": false,
        }),
    }
}

fn project_expect(expect: &Option<ExpectCondition>) -> Json {
    match expect {
        None => Json::Null,
        Some(ExpectCondition::Cel { expr }) => json!({
            "mode": "cel",
            "expr": expr,
            "template": Json::Null,
        }),
        Some(ExpectCondition::Model { template }) => json!({
            "mode": "model",
            "expr": Json::Null,
            "template": template.source,
        }),
    }
}

/// `RetryPolicy` always materializes Circuitry's own defaults when the
/// document omits `retries:`; projected as a concrete object either
/// way -- [`definitions_match`] accepts a Circuitry-side `null` against
/// it (see this module's own docs).
fn project_retries(retries: electricity_bytecode::effects::RetryPolicy) -> Json {
    json!({
        "max_attempts": retries.max_attempts,
        "backoff_ms": retries.backoff_ms,
    })
}

/// Ops in document order, taken from whichever of a `Region`'s shapes
/// carries a flat op list.
fn block_ops(region: &Region) -> &[Op] {
    match region {
        Region::Block { ops, .. } => ops,
        Region::Parallel { branches, .. } => branches,
        _ => &[],
    }
}

fn flow_of(region: &Region) -> &'static str {
    match region {
        Region::Parallel { .. } => "tree",
        _ => "chain",
    }
}

/// Projects a `dynamic`-shaped `Region` (`Block`/`Parallel`, optionally
/// wrapped in `TryFinally`) into a `DynamicDefinition`-shaped object,
/// minus `prompts`/`effect_names` (root-only; added by
/// [`project_program`]).
fn project_dynamic_region(op: &Op, region: &Region) -> Json {
    let (body, finally_ops): (&Region, Vec<Op>) = match region {
        Region::TryFinally { body, finally } => (body, block_ops(finally).to_vec()),
        other => (other, Vec::new()),
    };
    let flow = flow_of(body);
    let ops: Vec<Json> = block_ops(body).iter().map(project_op).collect();
    let finally_effects: Vec<Json> = finally_ops.iter().map(project_op).collect();

    let (max_concurrency, stop_on_error) = match body {
        Region::Parallel {
            max_concurrency,
            stop_on_error,
            ..
        } => (
            max_concurrency
                .map(|n| Json::Number(n.into()))
                .unwrap_or(Json::Null),
            Json::Bool(*stop_on_error),
        ),
        // Chain-flow: unrecoverable from this IR -- `definitions_match`
        // skips comparing these two keys whenever `flow` is `"chain"`.
        _ => (Json::Null, Json::Null),
    };

    json!({
        "name": op.name,
        "effects": ops,
        "flow": flow,
        "finally_effects": finally_effects,
        "max_concurrency": max_concurrency,
        "stop_on_error": stop_on_error,
        "on_error": on_error_str(op.on_error),
        "labels": op.labels.as_ref().map(value_to_json).unwrap_or(Json::Null),
        "enabled": op.enabled,
        // Root-only in Circuitry's own dump; every non-root dynamic
        // (including a reflector's synthetic "inner") carries empty
        // placeholders here, overwritten by `project_program` for the
        // real root alone.
        "prompts": {},
        "effect_names": Json::Array(Vec::new()),
    })
}

fn project_conditional_region(op: &Op, region: &Region) -> Json {
    let Region::If {
        cond,
        then_,
        else_,
        threshold,
    } = region
    else {
        unreachable!("caller only passes Region::If")
    };
    let then_effects: Vec<Json> = block_ops(then_).iter().map(project_op).collect();
    let else_effects: Vec<Json> = match else_ {
        Some(region) => block_ops(region).iter().map(project_op).collect(),
        None => Vec::new(),
    };
    json!({
        "name": op.name,
        "condition": project_condition(cond),
        "then_effects": then_effects,
        "else_effects": else_effects,
        "threshold": threshold,
        "on_error": on_error_str(op.on_error),
        "enabled": op.enabled,
        "labels": op.labels.as_ref().map(value_to_json).unwrap_or(Json::Null),
    })
}

fn project_loop_region(op: &Op, region: &Region) -> Json {
    let Region::Loop {
        spec,
        body,
        flow,
        max_concurrency,
        max_iterations,
        min_iterations,
        collect,
    } = region
    else {
        unreachable!("caller only passes Region::Loop")
    };
    let body_ops: Vec<Json> = block_ops(body).iter().map(project_op).collect();
    let (while_def, each_def) = match spec {
        LoopSpec::While(cond) => (project_condition(cond), Json::Null),
        LoopSpec::Each {
            in_path,
            as_name,
            truncate,
        } => (
            Json::Null,
            json!({"in_path": in_path, "as_name": as_name, "truncate": truncate}),
        ),
    };
    json!({
        "name": op.name,
        "body": body_ops,
        "while_def": while_def,
        "each_def": each_def,
        "max_iterations": max_iterations.map(|n| Json::Number(n.into())).unwrap_or(Json::Null),
        "min_iterations": min_iterations,
        "on_error": on_error_str(op.on_error),
        "collect": collect,
        "flow": match flow {
            LoopFlow::Chain => "chain",
            LoopFlow::Tree => "tree",
        },
        "max_concurrency": max_concurrency.map(|n| Json::Number(n.into())).unwrap_or(Json::Null),
        "enabled": op.enabled,
        "labels": op.labels.as_ref().map(value_to_json).unwrap_or(Json::Null),
    })
}

fn project_leaf(op: &Op, leaf: &LeafKind) -> Json {
    match leaf {
        LeafKind::Prompt(prompt) => {
            let (template, messages) = match &prompt.content {
                PromptContent::Template(text) => (Json::String(text.source.clone()), Json::Null),
                PromptContent::Messages(messages) => (
                    Json::Null,
                    Json::Array(
                        messages
                            .iter()
                            .map(|m| {
                                json!({
                                    "role": match m.role {
                                        Role::System => "system",
                                        Role::User => "user",
                                        Role::Assistant => "assistant",
                                        Role::Tool => "tool",
                                    },
                                    "content": m.content.source,
                                })
                            })
                            .collect(),
                    ),
                ),
            };
            let prompt_type = match prompt.prompt_type {
                PromptType::Text => "text",
                PromptType::Json => "json",
                PromptType::Boolean => "boolean",
                PromptType::Tool => "tool",
                PromptType::Number => "number",
                PromptType::Array => "array",
                PromptType::Object => "object",
            };
            json!({
                "name": op.name,
                "template": template,
                "messages": messages,
                "prompt_type": prompt_type,
                "schema": prompt.schema.as_ref().map(value_to_json).unwrap_or(Json::Null),
                "model": prompt.model,
                "provider": prompt.provider,
                "provider_fallbacks": if prompt.provider_fallbacks.is_empty() { Json::Null } else { Json::Array(prompt.provider_fallbacks.iter().cloned().map(Json::String).collect()) },
                "routing_override": Json::Null,
                "params": prompt.model_params.as_ref().map(param_node_to_json).unwrap_or(Json::Null),
                "timeout_ms": prompt.timeout_ms.map(|n| Json::Number(n.into())).unwrap_or(Json::Null),
                "deterministic": prompt.deterministic,
                "inputs": prompt.inputs.as_ref().map(param_node_to_json).unwrap_or(Json::Null),
                "assets": if prompt.assets.is_empty() { Json::Null } else { Json::Array(prompt.assets.iter().map(|a| json!({"kind": a.kind, "ref": a.reference})).collect()) },
                "retries": project_retries(prompt.retries),
                "on_error": on_error_str(op.on_error),
                "description": prompt.description,
                "enabled": op.enabled,
                "group": prompt.group,
            })
        }
        LeafKind::Tool(tool) => json!({
            "name": op.name,
            "provider": tool.provider,
            "params": param_node_to_json(&tool.params),
            "params_json": tool.params_json.as_ref().map(|t| Json::String(t.source.clone())).unwrap_or(Json::Null),
            "prompt": tool.prompt.as_ref().map(|t| Json::String(t.source.clone())).unwrap_or(Json::Null),
            "model": tool.model,
            "timeout_ms": tool.timeout_ms.map(|n| Json::Number(n.into())).unwrap_or(Json::Null),
            "on_error": on_error_str(op.on_error),
            "description": tool.description,
            "retries": project_retries(tool.retries),
            "expect": project_expect(&tool.expect),
            "enabled": op.enabled,
            "group": tool.group,
        }),
        LeafKind::Use(use_op) => {
            let (path, orchestration, inline) = match &use_op.source {
                UseSource::Path(p) => (Json::String(p.clone()), Json::Null, Json::Null),
                UseSource::Orchestration(o) => (Json::Null, Json::String(o.clone()), Json::Null),
                UseSource::Inline(text) => {
                    (Json::Null, Json::Null, Json::String(text.source.clone()))
                }
            };
            json!({
                "name": op.name,
                "ref": Json::Null,
                "path": path,
                "orchestration": orchestration,
                "inline": inline,
                "inputs": use_op.inputs.as_ref().map(param_node_to_json).unwrap_or(Json::Null),
                "outputs": use_op.outputs.as_ref().map(|m| {
                    let mut map = Map::new();
                    for (k, v) in m {
                        map.insert(k.clone(), Json::String(v.clone()));
                    }
                    Json::Object(map)
                }).unwrap_or(Json::Null),
                "validate": use_op.validate,
                "on_error": on_error_str(op.on_error),
                "description": use_op.description,
                "retries": project_retries(use_op.retries),
                "expect": project_expect(&use_op.expect),
                "enabled": op.enabled,
            })
        }
        LeafKind::Yield(yield_op) => json!({
            "name": op.name,
            "template": yield_op.template.source,
            "inputs": yield_op.inputs.as_ref().map(param_node_to_json).unwrap_or(Json::Null),
            "on_error": on_error_str(op.on_error),
            "description": yield_op.description,
            "enabled": op.enabled,
        }),
        LeafKind::Reflector(reflector) => json!({
            "name": op.name,
            "inner": project_dynamic_region(
                &Op {
                    path: op.path.clone(),
                    name: Some("inner".to_string()),
                    kind: NodeKind::Control((*reflector.inner).clone()),
                    on_error: OnError::Fail,
                    labels: None,
                    enabled: true,
                },
                &reflector.inner,
            ),
            "plan_from_step": reflector.plan_from_step,
            "max_iterations": reflector.max_iterations,
            "generated_key": reflector.generated_key,
            "stop_on_done": reflector.stop_on_done,
            "prime_template": template_source_or_marker(&reflector.prime_template.source),
            "max_effects": reflector.max_effects,
            "enabled": op.enabled,
        }),
    }
}

fn project_op(op: &Op) -> Json {
    match &op.kind {
        NodeKind::Leaf(leaf) => project_leaf(op, leaf),
        NodeKind::Control(region) => match region {
            Region::If { .. } => project_conditional_region(op, region),
            Region::Loop { .. } => project_loop_region(op, region),
            _ => project_dynamic_region(op, region),
        },
    }
}

/// Projects *program* into Circuitry's own `DynamicDefinition`-shaped
/// JSON, including the root-only `prompts`/`effect_names` fields.
pub fn project_program(program: &Program) -> Json {
    let mut root = project_dynamic_region_from_op(&program.root);
    let Json::Object(map) = &mut root else {
        unreachable!("project_op on a Control op always returns an object")
    };
    let mut prompts = Map::new();
    for (key, value) in &program.prompts {
        prompts.insert(key.clone(), Json::String(value.clone()));
    }
    map.insert("prompts".to_string(), Json::Object(prompts));
    map.insert(
        "effect_names".to_string(),
        Json::Array(
            program
                .effect_names
                .iter()
                .cloned()
                .map(Json::String)
                .collect(),
        ),
    );
    root
}

fn project_dynamic_region_from_op(op: &Op) -> Json {
    match &op.kind {
        NodeKind::Control(region) => project_dynamic_region(op, region),
        NodeKind::Leaf(_) => unreachable!("Program.root is always a dynamic"),
    }
}

/// Compares *expected* (a golden `definition`, from Circuitry's own
/// dump) against *actual* (this crate's own [`project_program`]),
/// applying the two narrow, documented exceptions this module's own
/// docs describe.
pub fn definitions_match(expected: &Json, actual: &Json) -> bool {
    match (expected, actual) {
        (Json::Object(expected_map), Json::Object(actual_map)) => {
            let is_chain_dynamic = matches!(expected_map.get("flow"), Some(Json::String(f)) if f == "chain")
                && expected_map.contains_key("effects");
            for (key, expected_value) in expected_map {
                if is_chain_dynamic && (key == "max_concurrency" || key == "stop_on_error") {
                    continue;
                }
                if key == "retries" && expected_value.is_null() {
                    continue;
                }
                let Some(actual_value) = actual_map.get(key) else {
                    return false;
                };
                if !definitions_match(expected_value, actual_value) {
                    return false;
                }
            }
            // Every key in `expected_map` was already checked present
            // (and equal) in `actual_map` by the loop above; an equal
            // length is what rules out an *extra* key on the actual
            // side that the loop never visits.
            expected_map.len() == actual_map.len()
        }
        (Json::Array(expected_items), Json::Array(actual_items)) => {
            expected_items.len() == actual_items.len()
                && expected_items
                    .iter()
                    .zip(actual_items.iter())
                    .all(|(e, a)| definitions_match(e, a))
        }
        _ => expected == actual,
    }
}
