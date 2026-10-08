//! Lane C: compiles `prompt`, `tool`, `use`, `yield` and `reflector`
//! into their [`electricity_bytecode::effects`] option structs -- every
//! effect type's own required-field checks, templates, params, `expect`
//! and `outputs`, and Python's coercions (`int(...)`, `float(...)`,
//! truthiness, an invalid `on_error` becoming `"fail"`).

use crate::CompileError;
use crate::compile::Ctx;
use crate::compile::coerce::{get_bool_default, get_truthy, py_int};
use crate::compile::containers::{compile_effects_in_scope, normalize_flow};
use crate::compile::expect::{compile_expect, compile_retries};
use crate::compile::params::{build_tool_params, build_use_inputs};
use crate::compile::templates::{check_templates, template_text};
use electricity_bytecode::effects::{
    AssetRef, Message, PromptContent, PromptOp, PromptType, ReflectorOp, Role, ToolOp, UseOp,
    UseSource, YieldOp,
};
use electricity_bytecode::path::EffectPath;
use electricity_bytecode::{Escape, LeafKind, NodeKind, OnError, Op, Region};
use electricity_value::{Dict, Value};
use indexmap::IndexMap;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

fn leaf_op(path: EffectPath, name: &str, kind: LeafKind, on_error: OnError) -> Op {
    Op {
        path,
        name: Some(name.to_string()),
        kind: NodeKind::Leaf(Box::new(kind)),
        on_error,
        labels: None,
        enabled: true,
    }
}

fn normalize_on_error_default(dict: &Dict) -> OnError {
    match crate::compile::coerce::normalize_choice(
        dict,
        "on_error",
        "fail",
        &["fail", "skip", "continue"],
        "fail",
    )
    .as_str()
    {
        "skip" => OnError::Skip,
        "continue" => OnError::Continue,
        _ => OnError::Fail,
    }
}

fn optional_str(dict: &Dict, key: &str) -> Option<String> {
    match dict.get(&Value::Str(key.to_string())) {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

/// A template/content field's text: *value* itself, or a `{file: ...}`'s
/// -- a minimal, lane-C-local stand-in for `core/prompt_files.py::
/// resolve_text_or_file` (lane D's own file): the plain-string case
/// (every corpus case this lane owns) is ported exactly; a `{file:
/// ...}` source -- lane D's own prompt-file reading/confinement -- is
/// left as a clearly-marked gap rather than silently mishandled, the
/// same way lane A's own seam stubs mark an unimplemented lane.
fn resolve_text_or_file(value: &Value, field: &str) -> Result<String, CompileError> {
    match value {
        Value::Str(s) => Ok(s.clone()),
        Value::Dict(dict)
            if dict.len() == 1 && dict.contains_key(&Value::Str("file".to_string())) =>
        {
            Err(CompileError(crate::not_implemented(
                &format!("leaves::resolve_text_or_file({field}: {{file: ...}})"),
                "D",
            )))
        }
        _ => Err(CompileError(format!(
            "{field} must be a string or {{file: <path>}}."
        ))),
    }
}

pub(crate) fn compile_prompt(
    effect: &Dict,
    path: &EffectPath,
    name: &str,
    effect_path: &str,
) -> Result<Op, CompileError> {
    let prompt_type_raw = get_truthy(effect, "prompt_type")
        .map(Value::py_str)
        .unwrap_or_else(|| "text".to_string())
        .trim()
        .to_lowercase();

    if prompt_type_raw == "image" {
        return Err(CompileError(format!(
            "Prompt '{name}': prompt_type 'image' is no longer supported. \
             Use a tool effect with provider: comfyui instead. \
             See the orchestration reference for migration instructions."
        )));
    }

    let template_raw = effect.get(&Value::Str("template".to_string()));
    let template = match template_raw {
        Some(value) if !value.is_none() => Some(resolve_text_or_file(
            value,
            &format!("{effect_path}.template"),
        )?),
        _ => None,
    };

    let mut messages: Vec<Message> = Vec::new();
    if let Some(Value::List(items)) = effect.get(&Value::Str("messages".to_string())) {
        if !items.is_empty() {
            for (index, item) in items.iter().enumerate() {
                let Value::Dict(message_dict) = item else {
                    continue;
                };
                let role_str = message_dict
                    .get(&Value::Str("role".to_string()))
                    .and_then(Value::as_str)
                    .unwrap_or("user");
                let role = match role_str {
                    "system" => Role::System,
                    "assistant" => Role::Assistant,
                    "tool" => Role::Tool,
                    _ => Role::User,
                };
                let content_raw = message_dict
                    .get(&Value::Str("content".to_string()))
                    .cloned()
                    .unwrap_or_else(|| Value::Str(String::new()));
                let content = resolve_text_or_file(
                    &content_raw,
                    &format!("{effect_path}.messages[{index}].content"),
                )?;
                messages.push(Message {
                    role,
                    // `composable: true`, `Escape::None` (prompt text,
                    // #397) -- the real syntax check runs a few lines
                    // below, against every message's `content` at once.
                    content: electricity_bytecode::TemplateText::new(content, true, Escape::None),
                });
            }
        }
    }

    let template_truthy = template.as_deref().is_some_and(|t| !t.is_empty());
    let messages_truthy = !messages.is_empty();
    if !template_truthy && !messages_truthy {
        return Err(CompileError(format!(
            "Prompt '{name}' must have 'template' or 'messages'."
        )));
    }

    if let Some(template) = &template {
        check_templates(&Value::Str(template.clone()), effect_path, "template", true)?;
    }
    for (index, message) in messages.iter().enumerate() {
        check_templates(
            &Value::Str(message.content.source.clone()),
            effect_path,
            &format!("messages[{index}].content"),
            true,
        )?;
    }

    let valid_prompt_types = [
        "text", "json", "boolean", "tool", "number", "array", "object",
    ];
    let prompt_type_raw = if valid_prompt_types.contains(&prompt_type_raw.as_str()) {
        prompt_type_raw
    } else {
        "text".to_string()
    };
    let prompt_type = match prompt_type_raw.as_str() {
        "json" => PromptType::Json,
        "boolean" => PromptType::Boolean,
        "tool" => PromptType::Tool,
        "number" => PromptType::Number,
        "array" => PromptType::Array,
        "object" => PromptType::Object,
        _ => PromptType::Text,
    };

    let schema = match effect.get(&Value::Str("schema".to_string())) {
        Some(value @ Value::Dict(_)) => Some(value.clone()),
        _ => None,
    };

    if matches!(
        prompt_type,
        PromptType::Json | PromptType::Object | PromptType::Array
    ) && schema.is_none()
    {
        return Err(CompileError(format!(
            "Prompt '{name}': prompt_type '{prompt_type_raw}' requires a 'schema' field."
        )));
    }

    // Known divergence from `core/compiler.py`'s own
    // `PromptDefinition.template`/`.messages`, documented in
    // `electricity-bytecode`'s own crate docs: a document setting both
    // compiles there and `template` silently wins at run time; this
    // compiler reproduces that by choosing `Template` here and
    // discarding `messages` when both are present.
    let content = if template_truthy {
        PromptContent::Template(template_text(
            template.as_deref().unwrap_or_default(),
            effect_path,
            "template",
            true,
            Escape::None,
        )?)
    } else {
        PromptContent::Messages(messages)
    };

    let model = optional_str(effect, "model");
    let provider = optional_str(effect, "provider");
    let provider_fallbacks = match effect.get(&Value::Str("provider_fallbacks".to_string())) {
        Some(Value::List(items)) if !items.is_empty() => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };

    let model_params = match effect.get(&Value::Str("params".to_string())) {
        Some(Value::Dict(params)) => Some(crate::compile::params::build_tool_params(
            params,
            name,
            effect_path,
            &BTreeSet::new(),
        )?),
        _ => None,
    };

    let timeout_ms = get_truthy(effect, "timeout_ms")
        .map(py_int)
        .map(|n| n.max(0) as u64);
    let deterministic = get_bool_default(effect, "deterministic", false);

    let inputs = match effect.get(&Value::Str("inputs".to_string())) {
        Some(Value::Dict(inputs)) => Some(build_use_inputs(
            inputs,
            name,
            effect_path,
            &BTreeSet::new(),
        )?),
        _ => None,
    };

    let mut assets = Vec::new();
    if let Some(Value::List(items)) = effect.get(&Value::Str("assets".to_string())) {
        if !items.is_empty() {
            for (index, item) in items.iter().enumerate() {
                let Value::Dict(asset_dict) = item else {
                    continue;
                };
                let kind = optional_str(asset_dict, "kind").unwrap_or_default();
                let reference = optional_str(asset_dict, "ref").unwrap_or_default();
                check_templates(
                    &Value::Str(reference.clone()),
                    effect_path,
                    &format!("assets[{index}].ref"),
                    false,
                )?;
                assets.push(AssetRef { kind, reference });
            }
        }
    }

    let retries = compile_retries(effect).unwrap_or_default();
    let on_error = normalize_on_error_default(effect);
    let description = optional_str(effect, "description");
    let group = get_truthy(effect, "group")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let op = PromptOp {
        content,
        prompt_type,
        schema,
        model,
        provider,
        provider_fallbacks,
        routing_override: None,
        model_params,
        timeout_ms,
        deterministic,
        inputs,
        assets,
        retries,
        description,
        group,
    };
    Ok(leaf_op(path.clone(), name, LeafKind::Prompt(op), on_error))
}

pub(crate) fn compile_tool(
    effect: &Dict,
    path: &EffectPath,
    name: &str,
    effect_path: &str,
    loop_names: &BTreeSet<String>,
) -> Result<Op, CompileError> {
    let provider = effect
        .get(&Value::Str("provider".to_string()))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(provider) = provider else {
        return Err(CompileError(format!(
            "Tool effect '{name}' at '{effect_path}' is missing required field 'provider'."
        )));
    };

    let empty_params = Dict::new();
    let params = match effect.get(&Value::Str("params".to_string())) {
        Some(Value::Dict(d)) => d,
        _ => &empty_params,
    };

    let params_json_raw = effect.get(&Value::Str("params_json".to_string()));
    let params_json = match params_json_raw {
        None | Some(Value::None) => None,
        Some(Value::Str(s)) => Some(s.clone()),
        Some(_) => {
            return Err(CompileError(format!(
                "Tool effect '{name}' at '{effect_path}' has invalid 'params_json': \
                 expected a string (a Mustache template that renders to JSON)."
            )));
        }
    };

    let prompt = optional_str(effect, "prompt");
    if let Some(prompt) = &prompt {
        check_templates(&Value::Str(prompt.clone()), effect_path, "prompt", true)?;
    }

    let params_node = build_tool_params(params, name, effect_path, loop_names)?;

    let params_json_text = match &params_json {
        Some(raw) => {
            check_templates(&Value::Str(raw.clone()), effect_path, "params_json", true)?;
            Some(electricity_bytecode::TemplateText::new(
                raw.clone(),
                true,
                Escape::Html,
            ))
        }
        None => None,
    };

    let model = optional_str(effect, "model");
    let timeout_ms = get_truthy(effect, "timeout_ms")
        .map(py_int)
        .map(|n| n.max(0) as u64);
    let on_error = normalize_on_error_default(effect);
    let description = optional_str(effect, "description");
    let retries = compile_retries(effect).unwrap_or_default();
    let expect = compile_expect(effect, "tool", effect_path, loop_names)?;
    let group = get_truthy(effect, "group")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let prompt_text =
        prompt.map(|raw| electricity_bytecode::TemplateText::new(raw, true, Escape::Html));

    let op = ToolOp {
        provider: provider.to_string(),
        params: params_node,
        params_json: params_json_text,
        prompt: prompt_text,
        model,
        timeout_ms,
        retries,
        expect,
        description,
        group,
    };
    Ok(leaf_op(path.clone(), name, LeafKind::Tool(op), on_error))
}

fn normalize_outputs(
    effect: &Dict,
    context: &str,
) -> Result<Option<IndexMap<String, String>>, CompileError> {
    let Some(raw) = get_truthy(effect, "outputs") else {
        return Ok(None);
    };
    let Value::Dict(dict) = raw else {
        return Err(CompileError(format!(
            "{context}: outputs must be a mapping of name -> {{path: <state path>}}, not {}.",
            raw.type_name()
        )));
    };
    let mut normalized = IndexMap::new();
    for (key, spec) in dict {
        let key_str = match key {
            Value::Str(s) => s.clone(),
            other => other.py_str(),
        };
        normalized.insert(
            key_str.clone(),
            normalize_one_output(spec, context, &key_str)?,
        );
    }
    Ok(Some(normalized))
}

fn normalize_one_output(spec: &Value, context: &str, key: &str) -> Result<String, CompileError> {
    match spec {
        Value::Str(s) => {
            let path = s.trim();
            if path.is_empty() {
                return Err(CompileError(format!(
                    "{context}: output '{key}' is an empty string. \
                     Give it a state path — {key}: {{path: prime.<effect>.value}}."
                )));
            }
            Ok(path.to_string())
        }
        Value::Dict(d) => {
            let declared = d
                .get(&Value::Str("path".to_string()))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let Some(declared) = declared else {
                return Err(CompileError(format!(
                    "{context}: output '{key}' has no 'path'. \
                     Canonical form is {key}: {{path: prime.<effect>.value}} \
                     (the bare string {key}: prime.<effect>.value is also accepted)."
                )));
            };
            Ok(declared.to_string())
        }
        other => Err(CompileError(format!(
            "{context}: output '{key}' must be an object with a 'path' key \
             (canonical) or a bare state-path string (shorthand); \
             got {}.",
            other.type_name()
        ))),
    }
}

pub(crate) fn compile_use(
    effect: &Dict,
    path: &EffectPath,
    name: &str,
    effect_path: &str,
    loop_names: &BTreeSet<String>,
) -> Result<Op, CompileError> {
    let field_str = |key: &str| -> Option<String> {
        effect
            .get(&Value::Str(key.to_string()))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let ref_field = field_str("ref");
    let path_field = field_str("path");
    let orchestration_field = field_str("orchestration");
    let inline_field = field_str("inline");

    let mut set_fields: Vec<&str> = Vec::new();
    if ref_field.is_some() {
        set_fields.push("ref");
    }
    if path_field.is_some() {
        set_fields.push("path");
    }
    if orchestration_field.is_some() {
        set_fields.push("orchestration");
    }
    if inline_field.is_some() {
        set_fields.push("inline");
    }

    if set_fields.is_empty() {
        return Err(CompileError(format!(
            "Use effect '{name}' at '{effect_path}' requires exactly one of \
             'ref' (curation library lookup), 'path' (filesystem), \
             'orchestration' (deprecated), or 'inline' (Mustache template yielding YAML)."
        )));
    }
    if set_fields.len() > 1 {
        return Err(CompileError(format!(
            "Use effect '{name}' at '{effect_path}' has multiple reference fields set \
             ({}). Specify exactly one of ref/path/orchestration/inline.",
            set_fields.join(", ")
        )));
    }

    // Documented divergence (DESIGN.md §4): electricity has no library-
    // name/remote-library resolution, so a document naming `ref:` alone
    // -- otherwise a perfectly ordinary field to Circuitry's own
    // compiler, which only resolves it at run time -- is rejected here
    // with electricity's own message instead of silently compiling into
    // an `Op` nothing can ever dispatch.
    if ref_field.is_some() {
        return Err(CompileError(format!(
            "Use effect '{name}' at '{effect_path}': library refs ('ref:') are not \
             supported by electricity; use 'path:' for a filesystem child instead."
        )));
    }

    let inputs_dict = match effect.get(&Value::Str("inputs".to_string())) {
        Some(Value::Dict(d)) => Some(d.clone()),
        _ => None,
    };

    if let Some(inline) = &inline_field {
        check_templates(&Value::Str(inline.clone()), effect_path, "inline", true)?;
    }

    let inputs = match &inputs_dict {
        Some(inputs) => Some(build_use_inputs(inputs, name, effect_path, loop_names)?),
        None => None,
    };

    let outputs = normalize_outputs(effect, &format!("Use effect '{name}'"))?;
    let validate_flag = get_bool_default(effect, "validate", true);
    let on_error = normalize_on_error_default(effect);
    let description = optional_str(effect, "description");
    let retries = compile_retries(effect).unwrap_or_default();
    let expect = compile_expect(effect, "use", effect_path, loop_names)?;

    let source = if let Some(path_value) = path_field {
        UseSource::Path(path_value)
    } else if let Some(orch_value) = orchestration_field {
        UseSource::Orchestration(orch_value)
    } else {
        let inline_value = inline_field.expect("exactly one of path/orchestration/inline set");
        UseSource::Inline(electricity_bytecode::TemplateText::new(
            inline_value,
            true,
            Escape::Html,
        ))
    };

    let op = UseOp {
        source,
        inputs,
        outputs,
        validate: validate_flag,
        retries,
        expect,
        description,
    };
    Ok(leaf_op(path.clone(), name, LeafKind::Use(op), on_error))
}

pub(crate) fn compile_yield(
    effect: &Dict,
    path: &EffectPath,
    name: &str,
    effect_path: &str,
) -> Result<Op, CompileError> {
    const FORBIDDEN_KEYS: [&str; 11] = [
        "prompt_type",
        "schema",
        "model",
        "provider",
        "provider_fallbacks",
        "params",
        "retries",
        "timeout_ms",
        "deterministic",
        "assets",
        "group",
    ];
    // "messages" is also forbidden, per `core/compiler.py`'s own
    // `_YIELD_FORBIDDEN_KEYS` (12 entries) -- split across two const
    // arrays only so clippy's `large_stack_arrays` never has anything
    // to say about it; checked together below.
    let forbidden: Vec<&str> = FORBIDDEN_KEYS
        .iter()
        .chain(["messages"].iter())
        .filter(|key| effect.contains_key(&Value::Str((**key).to_string())))
        .copied()
        .collect();
    if !forbidden.is_empty() {
        let quoted: Vec<String> = forbidden.iter().map(|k| format!("'{k}'")).collect();
        return Err(CompileError(format!(
            "Yield effect '{name}' does not accept {} — a yield effect never \
             calls a model.",
            quoted.join(", ")
        )));
    }

    let template_raw = effect.get(&Value::Str("template".to_string()));
    let Some(template_raw) = template_raw else {
        return Err(CompileError(format!(
            "Yield effect '{name}' must have 'template'."
        )));
    };
    let template = resolve_text_or_file(template_raw, &format!("{effect_path}.template"))?;
    if template.trim().is_empty() {
        return Err(CompileError(format!(
            "Yield effect '{name}': 'template' must not be empty."
        )));
    }
    let text = template_text(&template, effect_path, "template", true, Escape::None)?;

    let inputs = match effect.get(&Value::Str("inputs".to_string())) {
        Some(Value::Dict(d)) => Some(build_use_inputs(d, name, effect_path, &BTreeSet::new())?),
        _ => None,
    };

    let on_error = normalize_on_error_default(effect);
    let description = optional_str(effect, "description");

    let op = YieldOp {
        template: text,
        inputs,
        description,
    };
    Ok(leaf_op(path.clone(), name, LeafKind::Yield(op), on_error))
}

pub(crate) fn compile_reflector(
    ctx: &mut Ctx,
    effect: &Dict,
    path: &EffectPath,
    effect_path: &str,
    name: &str,
    loop_names: &BTreeSet<String>,
) -> Result<Op, CompileError> {
    let own_path = path.push_name(name);

    let flow = normalize_flow(
        crate::compile::coerce::first_truthy(&[
            get_truthy(effect, "flow"),
            get_truthy(effect, "strategy"),
        ])
        .map(Value::py_str)
        .as_deref(),
    )?;

    let inner_effects = crate::compile::coerce::first_truthy(&[
        get_truthy(effect, "effects"),
        get_truthy(effect, "steps"),
    ]);
    let empty = Value::List(Vec::new());
    let inner_effects = inner_effects.unwrap_or(&empty);

    let mut seen = BTreeMap::new();
    let compiled_inner = compile_effects_in_scope(
        ctx,
        inner_effects,
        &own_path,
        name,
        &format!("{effect_path}.effects"),
        loop_names,
        &mut seen,
    )?;

    let inner_region = match flow {
        electricity_bytecode::LoopFlow::Tree => Region::Parallel {
            branches: compiled_inner,
            max_concurrency: None,
            stop_on_error: false,
        },
        electricity_bytecode::LoopFlow::Chain => Region::Block {
            ops: compiled_inner,
            overlay: false,
        },
    };

    let plan_from_step = get_truthy(effect, "plan_from_step")
        .map(Value::py_str)
        .unwrap_or_else(|| electricity_bytecode::defaults::PLAN_FROM_STEP.to_string());
    let max_iterations = get_truthy(effect, "max_iterations")
        .map(py_int)
        .unwrap_or(electricity_bytecode::defaults::REFLECTOR_MAX_ITERATIONS as i64)
        .max(0) as u32;
    let generated_key = get_truthy(effect, "generated_key")
        .map(Value::py_str)
        .unwrap_or_else(|| electricity_bytecode::defaults::GENERATED_KEY.to_string());
    let stop_on_done = get_bool_default(effect, "stop_on_done", true);

    let prime_template_raw = optional_str(effect, "prime_template");
    let prime_template = match prime_template_raw {
        Some(raw) => template_text(&raw, effect_path, "prime_template", false, Escape::None)?,
        None => template_text(
            crate::reflector_prime::REFLECTOR_PRIME,
            effect_path,
            "prime_template",
            false,
            Escape::None,
        )?,
    };

    let max_effects = crate::compile::coerce::first_truthy(&[
        get_truthy(effect, "max_effects"),
        get_truthy(effect, "max_steps"),
    ])
    .map(py_int)
    .unwrap_or(electricity_bytecode::defaults::MAX_EFFECTS as i64)
    .max(0) as u32;

    let op = ReflectorOp {
        inner: Box::new(inner_region),
        plan_from_step,
        max_iterations,
        generated_key,
        stop_on_done,
        prime_template,
        max_effects,
    };
    Ok(Op {
        path: own_path,
        name: Some(name.to_string()),
        kind: NodeKind::Leaf(Box::new(LeafKind::Reflector(op))),
        on_error: OnError::Fail,
        labels: None,
        enabled: true,
    })
}
