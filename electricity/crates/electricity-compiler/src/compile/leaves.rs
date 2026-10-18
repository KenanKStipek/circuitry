//! Lane C: compiles `prompt`, `tool`, `use`, `yield` and `reflector`
//! into their [`electricity_bytecode::effects`] option structs -- every
//! effect type's own required-field checks, templates, params, `expect`
//! and `outputs`, and Python's coercions (`int(...)`, `float(...)`,
//! truthiness, an invalid `on_error` becoming `"fail"`).

use crate::compile::Ctx;
use crate::compile::coerce::{get_bool_default, get_truthy, py_int};
use crate::compile::containers::{check_resume_mode, compile_effects_in_scope, normalize_flow};
use crate::compile::expect::{compile_expect, compile_retries};
use crate::compile::params::{build_tool_params, build_use_inputs};
use crate::compile::templates::{check_templates, template_text};
use crate::prompt_files::compile_dispatch_file as compile_dispatch_file_dict;
use crate::state_ns::validate_reference_path;
use crate::{CompileError, DocumentOrigin, prompt_files};
use electricity_bytecode::effects::{
    AssetRef, DispatchFile, Message, PromptContent, PromptOp, PromptType, ReflectorOp, ReportLevel,
    ReportOp, Role, TimeoutFrom, TimeoutMs, ToolOp, UseOp, UseSource, YieldOp,
};
use electricity_bytecode::path::EffectPath;
use electricity_bytecode::{Escape, LeafKind, NodeKind, OnError, Op, ParamNode, Region};
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

/// `core/timeout_ref.py::_is_positive_integer`: a `bool` is not an integer
/// here, and `0` or a negative one is not positive.
fn is_positive_integer(value: &Value) -> bool {
    matches!(value, Value::Int(i) if !i.is_zero() && !i.is_negative())
}

/// `core/timeout_ref.py::compile_timeout_ms` (issue #564): a literal
/// `timeout_ms` as `int(...)` -- sign kept -- and a `{from: <path>,
/// default: <int>}` mapping as a [`TimeoutFrom`], its path rooted like
/// every other `{from:}` and its default a positive integer.
fn compile_timeout_ms(
    effect: &Dict,
    effect_type: &str,
    name: &str,
    effect_path: &str,
    loop_names: &BTreeSet<String>,
) -> Result<Option<TimeoutMs>, CompileError> {
    let Some(raw) = crate::compile::coerce::get_present(effect, "timeout_ms") else {
        return Ok(None);
    };
    let Value::Dict(raw) = raw else {
        return Ok(Some(TimeoutMs::Literal(py_int(raw))));
    };
    let mut capitalized = effect_type.to_string();
    capitalized[..1].make_ascii_uppercase();
    let where_ = format!("{capitalized} effect '{name}' at '{effect_path}'");
    let from_key = Value::Str("from".to_string());
    let default_key = Value::Str("default".to_string());
    let has_from = raw.contains_key(&from_key);
    if !has_from
        || raw
            .keys()
            .any(|key| key != &from_key && key != &default_key)
    {
        let mut keys: Vec<String> = raw.keys().map(Value::py_str).collect();
        keys.sort();
        let listed: Vec<String> = keys
            .iter()
            .map(|key| Value::Str(key.clone()).py_repr())
            .collect();
        return Err(CompileError(format!(
            "{where_}: 'timeout_ms' must be an integer, or {{from: <path>, default: \
             <positive integer>}}; got keys [{}].",
            listed.join(", ")
        )));
    }
    let Some(Value::Str(path)) = raw.get(&from_key) else {
        let got = raw.get(&from_key).map(Value::py_repr).unwrap_or_default();
        return Err(CompileError(format!(
            "{where_}: 'timeout_ms' 'from' must be a path string, got {got}."
        )));
    };
    let path = electricity_value::pycompat::py_strip(path).to_string();
    validate_reference_path(
        &path,
        &format!("{capitalized} effect '{name}' timeout_ms"),
        effect_path,
        loop_names,
    )?;
    let default = match raw.get(&default_key) {
        None => None,
        Some(value) if is_positive_integer(value) => Some(py_int(value)),
        Some(value) => {
            return Err(CompileError(format!(
                "{where_}: 'timeout_ms' 'default' must be a positive integer, got {}.",
                value.py_repr()
            )));
        }
    };
    Ok(Some(TimeoutMs::From(TimeoutFrom { path, default })))
}

/// [`prompt_files::compile_dispatch_file`] on a `template`/`content` *value*
/// already known to be a mapping.
fn compile_dispatch_file(
    value: &Value,
    field: &str,
    effect_path: &str,
    loop_names: &BTreeSet<String>,
    origin: &DocumentOrigin,
) -> Result<DispatchFile, CompileError> {
    let Value::Dict(dict) = value else {
        unreachable!("is_dispatch_file already checked the value is a mapping");
    };
    compile_dispatch_file_dict(dict, field, effect_path, loop_names, origin)
}

fn optional_str(dict: &Dict, key: &str) -> Option<String> {
    match dict.get(&Value::Str(key.to_string())) {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

pub(crate) fn compile_prompt(
    effect: &Dict,
    path: &EffectPath,
    name: &str,
    effect_path: &str,
    origin: &DocumentOrigin,
    loop_names: &BTreeSet<String>,
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
    // `template: {file: {from: ...}, within: ...}` (issue #578), read when
    // the effect dispatches; `template` is then absent, as in Python.
    let mut template_file: Option<DispatchFile> = None;
    let template = match template_raw {
        Some(value) if prompt_files::is_dispatch_file(value) => {
            template_file = Some(compile_dispatch_file(
                value,
                &format!("{effect_path}.template"),
                effect_path,
                loop_names,
                origin,
            )?);
            None
        }
        Some(value) if !value.is_none() => Some(prompt_files::resolve_text_or_file(
            value,
            &format!("{effect_path}.template"),
            origin,
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
                let content_field = format!("{effect_path}.messages[{index}].content");
                let (content, file) = if prompt_files::is_dispatch_file(&content_raw) {
                    let file = compile_dispatch_file(
                        &content_raw,
                        &content_field,
                        effect_path,
                        loop_names,
                        origin,
                    )?;
                    (String::new(), Some(file))
                } else {
                    (
                        prompt_files::resolve_text_or_file(&content_raw, &content_field, origin)?,
                        None,
                    )
                };
                messages.push(Message {
                    role,
                    // `composable: true`, `Escape::None` (prompt text,
                    // #397) -- the real syntax check runs a few lines
                    // below, against every message's `content` at once.
                    content: electricity_bytecode::TemplateText::new(content, true, Escape::None),
                    file,
                });
            }
        }
    }

    let template_truthy = template.as_deref().is_some_and(|t| !t.is_empty());
    let messages_truthy = !messages.is_empty();
    if !template_truthy && template_file.is_none() && !messages_truthy {
        return Err(CompileError(format!(
            "Prompt '{name}' must have 'template' or 'messages'."
        )));
    }

    if let Some(template) = &template {
        check_templates(&Value::Str(template.clone()), effect_path, "template", true)?;
    }
    for (index, message) in messages.iter().enumerate() {
        if message.file.is_some() {
            continue;
        }
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
    } else if let Some(file) = template_file {
        PromptContent::TemplateFile(Box::new(file))
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

    // Python's `_compile_prompt` keeps `params`/`inputs` as the raw
    // dict (an `isinstance(dict)` filter only): neither is
    // template-checked, reference-checked, or walked for a
    // security-sensitive leaf -- `prompt.py`/`yield_effect.py` merge
    // `inputs` into the run-time context as-is, and `prompt.py` passes
    // `params` straight to the adapter. Unlike a tool's `params` or a
    // `use` effect's `inputs`, these are never Mustache fields.
    let model_params = match effect.get(&Value::Str("params".to_string())) {
        Some(value @ Value::Dict(_)) => Some(ParamNode::Literal(value.clone())),
        _ => None,
    };

    // Sign kept through compilation -- a negative `timeout_ms` is still
    // Python-truthy (`if self.defn.timeout_ms:`), resolved at run time
    // (issue #536).
    let timeout_ms = compile_timeout_ms(effect, "prompt", name, effect_path, loop_names)?;
    let deterministic = get_bool_default(effect, "deterministic", false);

    let inputs = match effect.get(&Value::Str("inputs".to_string())) {
        Some(value @ Value::Dict(_)) => Some(ParamNode::Literal(value.clone())),
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

    // `if retries_raw and isinstance(retries_raw, dict)` (`core/compiler.py:
    // 1582-1589`): a non-empty mapping compiles through `compile_retries`
    // (which, called with a *present* `retries:` at all, happily returns
    // `Some(RetryPolicy::default())` for an *empty* one too -- `ToolOp`/
    // `UseOp` want exactly that `{}` -> defaults behaviour, but a prompt's
    // own `{}` must stay `None` so `runtime.default_prompt_retries` applies
    // instead).
    let retries = match effect.get(&Value::Str("retries".to_string())) {
        Some(Value::Dict(d)) if !d.is_empty() => compile_retries(effect),
        _ => None,
    };
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
    origin: &DocumentOrigin,
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

    let mut prompt_file: Option<DispatchFile> = None;
    let params_without_prompt: Dict;
    let mut params = params;
    if electricity_value::pycompat::py_strip(provider).to_lowercase() == "agent" {
        prompt_file = compile_agent_prompt_file(
            params,
            params_json.as_deref(),
            effect_path,
            origin,
            loop_names,
        )?;
        if prompt_file.is_some() {
            params_without_prompt = params
                .iter()
                .filter(|(key, _)| **key != Value::Str("prompt".to_string()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            params = &params_without_prompt;
        }
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
    // Sign kept through compilation -- see the `PromptOp` branch above.
    let timeout_ms = compile_timeout_ms(effect, "tool", name, effect_path, loop_names)?;
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
        prompt_file,
    };
    Ok(leaf_op(path.clone(), name, LeafKind::Tool(op), on_error))
}

/// `core/compiler.py::_compile_agent_prompt_file` (issue #591): the `agent`
/// tool's `params.prompt` of `{file: {from: ...}, within: ...}`, compiled
/// like a prompt's `template` of that form under this document's trust.
/// `params_json` never supplies the form nor replaces a prompt read from a
/// file; a raw `params_json` that parses as JSON is checked for both.
fn compile_agent_prompt_file(
    params: &Dict,
    params_json: Option<&str>,
    effect_path: &str,
    origin: &DocumentOrigin,
    loop_names: &BTreeSet<String>,
) -> Result<Option<DispatchFile>, CompileError> {
    let prompt_key = Value::Str("prompt".to_string());
    let prompt_file = match params.get(&prompt_key) {
        Some(value) if prompt_files::is_dispatch_file(value) => Some(compile_dispatch_file(
            value,
            &format!("{effect_path}.params.prompt"),
            effect_path,
            loop_names,
            origin,
        )?),
        _ => None,
    };
    let overlay = params_json.and_then(|raw| electricity_json::loads(raw).ok());
    if let Some(Value::Dict(overlay)) = &overlay {
        if let Some(prompt) = overlay.get(&prompt_key) {
            if prompt_files::is_dispatch_file(prompt) {
                return Err(CompileError(format!(
                    "{effect_path}: params_json must not set params.prompt to {{file: {{from: \
                     <state path>}}, within: <dir>}}: a prompt file is only honoured from a \
                     document's literal params block."
                )));
            }
            if prompt_file.is_some() {
                return Err(CompileError(format!(
                    "{effect_path}: params_json must not set params.prompt: this effect reads \
                     params.prompt from a file ({{file: {{from: <state path>}}, within: <dir>}}), \
                     which params_json cannot replace."
                )));
            }
        }
    }
    Ok(prompt_file)
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
    origin: &DocumentOrigin,
) -> Result<Op, CompileError> {
    check_resume_mode(effect, "Use", effect_path)?;
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

    if inline_field.is_some() {
        // Python's own syntax check (`_check_templates`) runs against
        // the *raw*, untrimmed `inline` value -- only `inline_field`
        // (trimmed, for the IR) is stripped; a leading blank line
        // would otherwise shift a "malformed ... at line N" message's
        // own line number (finding 20).
        let raw_inline = effect.get(&Value::Str("inline".to_string())).cloned();
        if let Some(raw_inline) = raw_inline {
            check_templates(&raw_inline, effect_path, "inline", true)?;
        }
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
        parent_trusted: matches!(origin, DocumentOrigin::File { .. }),
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
    origin: &DocumentOrigin,
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

    // `effect.get("template")` is `None` for both an absent key and an
    // explicit `template: null` -- either way Python's own
    // `template_raw is None` check raises the same "must have
    // 'template'" message, not `resolve_text_or_file`'s shape error.
    let template_raw = effect
        .get(&Value::Str("template".to_string()))
        .filter(|v| !v.is_none());
    let Some(template_raw) = template_raw else {
        return Err(CompileError(format!(
            "Yield effect '{name}' must have 'template'."
        )));
    };
    let template = prompt_files::resolve_text_or_file(
        template_raw,
        &format!("{effect_path}.template"),
        origin,
    )?;
    if template.trim().is_empty() {
        return Err(CompileError(format!(
            "Yield effect '{name}': 'template' must not be empty."
        )));
    }
    let text = template_text(&template, effect_path, "template", true, Escape::None)?;

    // `_compile_yield` keeps `inputs` raw, same as `_compile_prompt`'s
    // (this module's own doc comment on `compile_prompt` explains why).
    let inputs = match effect.get(&Value::Str("inputs".to_string())) {
        Some(value @ Value::Dict(_)) => Some(ParamNode::Literal(value.clone())),
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

/// `core/compiler.py::_compile_report` (#605): `message` is required,
/// `name` is optional (unlike every other leaf type here), `level`
/// must be `info`/`warning` when present, and `data` is walked by
/// [`crate::compile::params::build_report_data`]'s own narrower
/// `{from:}` grammar. The schema's own `additionalProperties: false`
/// (mirrored in `structural.rs`'s `EFFECT_DEFS`) rejects every other
/// key, so there is no forbidden-keys list here the way `compile_yield`
/// has one.
pub(crate) fn compile_report(
    effect: &Dict,
    path: &EffectPath,
    scope_path: &str,
    effect_path: &str,
    loop_names: &BTreeSet<String>,
) -> Result<Op, CompileError> {
    let validated_name = match effect
        .get(&Value::Str("name".to_string()))
        .filter(|v| !v.is_none())
    {
        Some(name) => Some(crate::compile::names::validate_name(
            name,
            "report",
            scope_path,
            effect_path,
        )?),
        None => None,
    };

    let message_raw = effect
        .get(&Value::Str("message".to_string()))
        .filter(|v| !v.is_none());
    let Some(Value::Str(message_raw)) = message_raw else {
        return Err(CompileError(format!(
            "Report effect at '{effect_path}' must have a non-empty 'message'."
        )));
    };
    if message_raw.trim().is_empty() {
        return Err(CompileError(format!(
            "Report effect at '{effect_path}' must have a non-empty 'message'."
        )));
    }
    let message = template_text(message_raw, effect_path, "message", true, Escape::None)?;

    let level = match effect
        .get(&Value::Str("level".to_string()))
        .filter(|v| !v.is_none())
    {
        None => ReportLevel::Info,
        Some(Value::Str(level)) if level.trim().eq_ignore_ascii_case("info") => ReportLevel::Info,
        Some(Value::Str(level)) if level.trim().eq_ignore_ascii_case("warning") => {
            ReportLevel::Warning
        }
        Some(other) => {
            return Err(CompileError(format!(
                "Report effect at '{effect_path}' has invalid 'level' {} \
                 -- expected 'info' or 'warning'.",
                other.py_repr()
            )));
        }
    };

    let data = match effect
        .get(&Value::Str("data".to_string()))
        .filter(|v| !v.is_none())
    {
        None => None,
        Some(Value::Dict(data_dict)) => Some(crate::compile::params::build_report_data(
            &Value::Dict(data_dict.clone()),
            validated_name.as_deref(),
            effect_path,
            "data",
            loop_names,
        )?),
        Some(_) => {
            return Err(CompileError(format!(
                "Report effect at '{effect_path}': 'data' must be an object."
            )));
        }
    };

    let description = optional_str(effect, "description");

    let own_path = match &validated_name {
        Some(n) => path.push_name(n),
        None => path.clone(),
    };

    let op = ReportOp {
        message,
        data,
        level,
        description,
    };
    Ok(Op {
        path: own_path,
        name: validated_name,
        kind: NodeKind::Leaf(Box::new(LeafKind::Report(op))),
        on_error: OnError::Fail,
        labels: None,
        enabled: true,
    })
}

pub(crate) fn compile_reflector(
    ctx: &mut Ctx,
    effect: &Dict,
    path: &EffectPath,
    scope_path: &str,
    effect_path: &str,
    name: &str,
    loop_names: &BTreeSet<String>,
) -> Result<Op, CompileError> {
    let own_path = path.push_name(name);
    let inner_scope = crate::compile::containers::scope_child(scope_path, name);

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
        &inner_scope,
        &format!("{effect_path}.effects"),
        loop_names,
        &mut seen,
    )?;

    let plan_from_step = get_truthy(effect, "plan_from_step")
        .map(Value::py_str)
        .unwrap_or_else(|| electricity_bytecode::defaults::PLAN_FROM_STEP.to_string());
    for inner_op in &compiled_inner {
        if inner_op.name.as_deref() != Some(plan_from_step.as_str()) {
            continue;
        }
        let NodeKind::Leaf(leaf) = &inner_op.kind else {
            continue;
        };
        let LeafKind::Prompt(prompt) = leaf.as_ref() else {
            continue;
        };
        let file_field = match &prompt.content {
            PromptContent::TemplateFile(_) => Some("template".to_string()),
            PromptContent::Messages(messages) => messages
                .iter()
                .position(|message| message.file.is_some())
                .map(|index| format!("messages[{index}].content")),
            PromptContent::Template(_) => None,
        };
        if let Some(field) = file_field {
            return Err(CompileError(format!(
                "Reflector effect '{name}' at '{effect_path}': its plan prompt \
                 '{plan_from_step}' reads {field} from a file named in state \
                 ('file: {{from: ...}}'), but a reflector builds its planning prompt from \
                 the prompt's own text and would drop the file. Write the text in the \
                 prompt, or name the file with 'file: <path>'."
            )));
        }
    }
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

    let max_iterations = get_truthy(effect, "max_iterations")
        .map(py_int)
        .unwrap_or(electricity_bytecode::defaults::REFLECTOR_MAX_ITERATIONS as i64)
        .clamp(0, i64::from(u32::MAX)) as u32;
    let generated_key = get_truthy(effect, "generated_key")
        .map(Value::py_str)
        .unwrap_or_else(|| electricity_bytecode::defaults::GENERATED_KEY.to_string());
    let stop_on_done = get_bool_default(effect, "stop_on_done", true);

    // `prime_template` is a Python `str.format` string (reflector.py),
    // never a Mustache template -- `_compile_reflector` does
    // `str(prime_template)` with no syntax check at all, so neither
    // branch here runs one.
    let prime_template_raw = optional_str(effect, "prime_template");
    let prime_template = match prime_template_raw {
        Some(raw) => electricity_bytecode::TemplateText::new(raw, false, Escape::None),
        None => electricity_bytecode::TemplateText::new(
            crate::reflector_prime::REFLECTOR_PRIME,
            false,
            Escape::None,
        ),
    };

    let max_effects = crate::compile::coerce::first_truthy(&[
        get_truthy(effect, "max_effects"),
        get_truthy(effect, "max_steps"),
    ])
    .map(py_int)
    .unwrap_or(electricity_bytecode::defaults::MAX_EFFECTS as i64)
    .clamp(0, i64::from(u32::MAX)) as u32;

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
