//! Lane C: compiles `dynamic`, `if`/`conditional` and `loop` into
//! [`electricity_bytecode::Region`] -- names and scopes, duplicate-name
//! checks per scope, `finally:` sharing the body's scope, and the
//! `if`/`while` mode checks.

use crate::CompileError;
use crate::compile::Ctx;
use crate::compile::coerce::{first_truthy, get_bool_default, get_truthy, py_float, py_int};
use crate::compile::names::validate_name;
use crate::compile::templates::template_text;
use crate::state_ns::{validate_cel_expr, validate_cel_syntax, validate_each_in_path};
use electricity_bytecode::path::EffectPath;
use electricity_bytecode::{Condition, Escape, LoopFlow, LoopSpec, NodeKind, OnError, Op, Region};
use electricity_value::{Dict, Value};
use std::collections::BTreeMap;
use std::collections::BTreeSet;

fn normalize_on_error_dyn(dict: &Dict) -> OnError {
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

fn normalize_on_error_conditional(dict: &Dict) -> OnError {
    match crate::compile::coerce::normalize_choice(
        dict,
        "on_error",
        "fail",
        &["fail", "continue", "skip"],
        "fail",
    )
    .as_str()
    {
        "continue" => OnError::Continue,
        "skip" => OnError::Skip,
        _ => OnError::Fail,
    }
}

fn normalize_on_error_loop(dict: &Dict) -> OnError {
    match crate::compile::coerce::normalize_choice(
        dict,
        "on_error",
        "fail",
        &["fail", "break", "continue"],
        "fail",
    )
    .as_str()
    {
        "break" => OnError::Break,
        "continue" => OnError::Continue,
        _ => OnError::Fail,
    }
}

const VALID_FLOWS: [(&str, &str); 6] = [
    ("chain", "chain"),
    ("chain_of_thought", "chain"),
    ("cot", "chain"),
    ("tree", "tree"),
    ("tree_of_thought", "tree"),
    ("tot", "tree"),
];

/// Ports `core/compiler.py::_normalize_flow`.
pub(crate) fn normalize_flow(raw: Option<&str>) -> Result<LoopFlow, CompileError> {
    // Python reprs the *un*-trimmed, un-lowercased argument it was
    // called with (`flow!r`), not the normalized `key` the match below
    // uses.
    let flow = raw.unwrap_or("chain");
    let key = flow.trim().to_lowercase();
    for (alias, canonical) in VALID_FLOWS {
        if key == alias {
            return Ok(if canonical == "tree" {
                LoopFlow::Tree
            } else {
                LoopFlow::Chain
            });
        }
    }
    let mut valid: Vec<&str> = VALID_FLOWS.iter().map(|(a, _)| *a).collect();
    valid.sort_unstable();
    Err(CompileError(format!(
        "Unknown flow value {}. Valid values are: {}.",
        Value::Str(flow.to_string()).py_repr(),
        valid.join(", ")
    )))
}

fn labels_of(dict: &Dict) -> Option<Value> {
    match dict.get(&Value::Str("labels".to_string())) {
        Some(value @ Value::Dict(_)) => Some(value.clone()),
        _ => None,
    }
}

/// Ports `core/compiler.py::_scope_child`.
pub(crate) fn scope_child(scope_path: &str, child_name: &str) -> String {
    if scope_path.is_empty() {
        child_name.to_string()
    } else {
        format!("{scope_path}.{child_name}")
    }
}

/// Ports `core/compiler.py::_compile_effects_in_scope`.
/// How many levels deep [`compile_effects_in_scope`] recurses into
/// itself (through `dynamic`/`if`/`loop`/a reflector's inner effects)
/// before bailing with a distinct error instead of risking a stack
/// overflow -- see [`Ctx`]'s own docs for how this number was chosen.
/// Comfortably below both the ~210-level debug-build overflow point a
/// 2 MiB stack hits in practice, and `electricity_yaml::MAX_DEPTH`
/// (512, which already bounds how deep a *parsed* document's effect
/// containers can possibly nest at roughly half that many levels, two
/// YAML-structural-depth units per level).
const MAX_COMPILE_DEPTH: usize = 128;

/// Increments/decrements [`Ctx`]'s own nesting-depth counter around
/// [`compile_effects_in_scope_inner`]'s own body -- a plain wrapper
/// rather than an RAII guard borrowing `ctx.depth` specifically, since
/// that borrow would outlive the inner call's own (separate) uses of
/// `ctx` as a whole.
pub(crate) fn compile_effects_in_scope(
    ctx: &mut Ctx,
    effects: &Value,
    path: &EffectPath,
    scope_path: &str,
    container_path: &str,
    loop_names: &BTreeSet<String>,
    seen_names: &mut BTreeMap<String, String>,
) -> Result<Vec<Op>, CompileError> {
    ctx.depth += 1;
    if ctx.depth > MAX_COMPILE_DEPTH {
        ctx.depth -= 1;
        return Err(CompileError(format!(
            "{container_path}: effect nesting is too deep to compile \
             (over {MAX_COMPILE_DEPTH} levels)."
        )));
    }
    let result = compile_effects_in_scope_inner(
        ctx,
        effects,
        path,
        scope_path,
        container_path,
        loop_names,
        seen_names,
    );
    ctx.depth -= 1;
    result
}

fn compile_effects_in_scope_inner(
    ctx: &mut Ctx,
    effects: &Value,
    path: &EffectPath,
    scope_path: &str,
    container_path: &str,
    loop_names: &BTreeSet<String>,
    seen_names: &mut BTreeMap<String, String>,
) -> Result<Vec<Op>, CompileError> {
    let Value::List(items) = effects else {
        return Err(CompileError(format!(
            "{container_path} must be a list of effects."
        )));
    };

    let mut compiled = Vec::with_capacity(items.len());
    for (idx, effect) in items.iter().enumerate() {
        let effect_path = format!("{container_path}[{idx}]");
        let Value::Dict(dict) = effect else {
            return Err(CompileError(format!(
                "Effect at '{effect_path}' must be an object/mapping, got {}.",
                effect.type_name()
            )));
        };

        let effect_type = dict
            .get(&Value::Str("type".to_string()))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if effect_type.is_empty() {
            return Err(CompileError(format!(
                "Effect at '{effect_path}' is missing required field 'type'."
            )));
        }

        if let Some(raw_name) = dict.get(&Value::Str("name".to_string())) {
            let valid_name = validate_name(raw_name, &effect_type, scope_path, &effect_path)?;
            if let Some(first_seen) = seen_names.get(&valid_name) {
                return Err(CompileError(format!(
                    "Duplicate effect name '{valid_name}' in scope '{scope_path}'. \
                     Seen at '{first_seen}' and '{effect_path}'."
                )));
            }
            seen_names.insert(valid_name, effect_path.clone());
        }

        compiled.push(compile_effect(
            ctx,
            dict,
            &effect_type,
            path,
            scope_path,
            &effect_path,
            loop_names,
        )?);
    }
    Ok(compiled)
}

fn compile_effect(
    ctx: &mut Ctx,
    effect: &Dict,
    effect_type: &str,
    path: &EffectPath,
    scope_path: &str,
    effect_path: &str,
    loop_names: &BTreeSet<String>,
) -> Result<Op, CompileError> {
    let name = effect.get(&Value::Str("name".to_string()));

    if effect_type != "dynamic" && effect.contains_key(&Value::Str("finally".to_string())) {
        let label = if effect_type.is_empty() {
            "unknown"
        } else {
            effect_type
        };
        return Err(CompileError(format!(
            "{label} effect at '{effect_path}': 'finally' is \
             only allowed on a 'dynamic' effect or the document root."
        )));
    }

    match effect_type {
        "prompt" => {
            let Some(name) = name else {
                return Err(CompileError(format!(
                    "Prompt effect at '{effect_path}' is missing required field 'name'."
                )));
            };
            let valid_name = validate_name(name, "prompt", scope_path, effect_path)?;
            crate::compile::leaves::compile_prompt(
                effect,
                &path.push_name(&valid_name),
                &valid_name,
                effect_path,
            )
        }
        "dynamic" => {
            let Some(name) = name else {
                return Err(CompileError(format!(
                    "Dynamic effect at '{effect_path}' is missing required field 'name'."
                )));
            };
            let valid_name = validate_name(name, "dynamic", scope_path, effect_path)?;
            compile_dynamic(
                ctx,
                effect,
                path,
                scope_path,
                effect_path,
                &valid_name,
                loop_names,
            )
        }
        "conditional" | "if" => {
            compile_conditional(ctx, effect, path, scope_path, effect_path, loop_names)
        }
        "loop" => compile_loop(ctx, effect, path, scope_path, effect_path, loop_names),
        "tool" => {
            let Some(name) = name else {
                return Err(CompileError(format!(
                    "Tool effect at '{effect_path}' is missing required field 'name'."
                )));
            };
            let valid_name = validate_name(name, "tool", scope_path, effect_path)?;
            crate::compile::leaves::compile_tool(
                effect,
                &path.push_name(&valid_name),
                &valid_name,
                effect_path,
                loop_names,
            )
        }
        "use" => {
            let Some(name) = name else {
                return Err(CompileError(format!(
                    "Use effect at '{effect_path}' is missing required field 'name'."
                )));
            };
            let valid_name = validate_name(name, "use", scope_path, effect_path)?;
            crate::compile::leaves::compile_use(
                effect,
                &path.push_name(&valid_name),
                &valid_name,
                effect_path,
                loop_names,
            )
        }
        "yield" => {
            let Some(name) = name else {
                return Err(CompileError(format!(
                    "Yield effect at '{effect_path}' is missing required field 'name'."
                )));
            };
            let valid_name = validate_name(name, "yield", scope_path, effect_path)?;
            crate::compile::leaves::compile_yield(
                effect,
                &path.push_name(&valid_name),
                &valid_name,
                effect_path,
            )
        }
        "reflector" => {
            let Some(name) = name else {
                return Err(CompileError(format!(
                    "Reflector effect at '{effect_path}' is missing required field 'name'."
                )));
            };
            let valid_name = validate_name(name, "reflector", scope_path, effect_path)?;
            crate::compile::leaves::compile_reflector(
                ctx,
                effect,
                path,
                scope_path,
                effect_path,
                &valid_name,
                loop_names,
            )
        }
        other => Err(CompileError(format!(
            "Unsupported effect type at '{effect_path}': {}",
            Value::Str(other.to_string()).py_repr()
        ))),
    }
}

fn compile_dynamic(
    ctx: &mut Ctx,
    effect: &Dict,
    path: &EffectPath,
    scope_path: &str,
    effect_path: &str,
    valid_name: &str,
    loop_names: &BTreeSet<String>,
) -> Result<Op, CompileError> {
    let flow = normalize_flow(
        first_truthy(&[get_truthy(effect, "flow"), get_truthy(effect, "strategy")])
            .map(Value::py_str)
            .as_deref(),
    )?;

    let own_path = path.push_name(valid_name);
    let child_scope = scope_child(scope_path, valid_name);
    let child_effects = first_truthy(&[get_truthy(effect, "effects"), get_truthy(effect, "steps")]);
    let empty = Value::List(Vec::new());
    let child_effects = child_effects.unwrap_or(&empty);

    let mut seen = BTreeMap::new();
    let compiled_children = compile_effects_in_scope(
        ctx,
        child_effects,
        &own_path,
        &child_scope,
        &format!("{effect_path}.effects"),
        loop_names,
        &mut seen,
    )?;

    let finally_val = effect.get(&Value::Str("finally".to_string()));
    let finally_present = finally_val.is_some_and(crate::state_ns::is_truthy);
    let compiled_finally = if finally_present {
        compile_effects_in_scope(
            ctx,
            finally_val.unwrap(),
            &own_path,
            &child_scope,
            &format!("{effect_path}.finally"),
            loop_names,
            &mut seen,
        )?
    } else {
        Vec::new()
    };

    let max_concurrency = get_truthy(effect, "max_concurrency")
        .map(py_int)
        .map(|n| n.max(0) as u32);
    let stop_on_error = get_bool_default(effect, "stop_on_error", false);
    let on_error = normalize_on_error_dyn(effect);
    let labels = labels_of(effect);

    let body_region = match flow {
        LoopFlow::Tree => Region::Parallel {
            branches: compiled_children,
            max_concurrency,
            stop_on_error,
        },
        LoopFlow::Chain => Region::Block {
            ops: compiled_children,
            overlay: false,
        },
    };
    let region = if compiled_finally.is_empty() {
        body_region
    } else {
        Region::TryFinally {
            body: Box::new(body_region),
            finally: Box::new(Region::Block {
                ops: compiled_finally,
                overlay: false,
            }),
        }
    };

    Ok(Op {
        path: own_path,
        name: Some(valid_name.to_string()),
        kind: NodeKind::Control(region),
        on_error,
        labels,
        enabled: true,
    })
}

fn compile_conditional(
    ctx: &mut Ctx,
    effect: &Dict,
    path: &EffectPath,
    scope_path: &str,
    effect_path: &str,
    loop_names: &BTreeSet<String>,
) -> Result<Op, CompileError> {
    let validated_name = match effect.get(&Value::Str("name".to_string())) {
        Some(name) => Some(validate_name(name, "conditional", scope_path, effect_path)?),
        None => None,
    };

    let Some(Value::Dict(if_def)) = effect.get(&Value::Str("if".to_string())) else {
        return Err(CompileError(format!(
            "Conditional at '{effect_path}' must have an 'if' field with \
             condition definition."
        )));
    };
    // The field label for the mode-check error messages is "Conditional",
    // the "if." prefix is applied inside `compile_condition` via `field`.
    let condition = compile_condition_labeled(
        if_def,
        effect_path,
        "Conditional",
        "if",
        "CEL expression",
        loop_names,
        validated_name.as_deref(),
    )?;

    let own_path = match &validated_name {
        Some(n) => path.push_name(n),
        None => path.clone(),
    };
    let branch_scope = match &validated_name {
        Some(n) => scope_child(scope_path, n),
        None => scope_path.to_string(),
    };

    let then_val = effect.get(&Value::Str("then".to_string()));
    let then_present = then_val.is_some_and(crate::state_ns::is_truthy);
    let empty = Value::List(Vec::new());
    let then_list = if then_present {
        then_val.unwrap()
    } else {
        &empty
    };
    let mut then_seen = BTreeMap::new();
    let compiled_then = compile_effects_in_scope(
        ctx,
        then_list,
        &own_path,
        &branch_scope,
        &format!("{effect_path}.then"),
        loop_names,
        &mut then_seen,
    )?;

    let else_val = effect.get(&Value::Str("else".to_string()));
    let else_present = else_val.is_some_and(crate::state_ns::is_truthy);
    let compiled_else = if else_present {
        let mut else_seen = BTreeMap::new();
        compile_effects_in_scope(
            ctx,
            else_val.unwrap(),
            &own_path,
            &branch_scope,
            &format!("{effect_path}.else"),
            loop_names,
            &mut else_seen,
        )?
    } else {
        Vec::new()
    };

    let threshold = get_truthy(effect, "threshold").map(py_float).unwrap_or(0.5);
    let on_error = normalize_on_error_conditional(effect);
    let labels = labels_of(effect);

    let region = Region::If {
        cond: condition,
        then_: Box::new(Region::Block {
            ops: compiled_then,
            overlay: true,
        }),
        else_: if compiled_else.is_empty() && !else_present {
            None
        } else {
            Some(Box::new(Region::Block {
                ops: compiled_else,
                overlay: true,
            }))
        },
        threshold,
    };

    Ok(Op {
        path: own_path,
        name: validated_name,
        kind: NodeKind::Control(region),
        on_error,
        labels,
        enabled: true,
    })
}

/// Like [`compile_condition`], but matching `_compile_conditional`'s own
/// error wording (`"Conditional at '{path}': ..."`) rather than the
/// generic `"{field} at '{path}': ..."` shape -- Python repeats the
/// mode-check logic once per container with each one's own fixed
/// message text instead of a shared helper, so this does too via
/// *message_prefix*.
fn compile_condition_labeled(
    def: &Dict,
    effect_path: &str,
    message_prefix: &str,
    field: &str,
    cel_label: &str,
    loop_names: &BTreeSet<String>,
    effect_name: Option<&str>,
) -> Result<Condition, CompileError> {
    let mode = get_truthy(def, "mode")
        .map(Value::py_str)
        .unwrap_or_else(|| "model".to_string())
        .trim()
        .to_lowercase();
    if mode == "cel" {
        let expr = def
            .get(&Value::Str("expr".to_string()))
            .filter(|v| crate::state_ns::is_truthy(v));
        let Some(expr) = expr else {
            return Err(CompileError(format!(
                "{message_prefix} at '{effect_path}': mode 'cel' requires an 'expr' field."
            )));
        };
        let expr = expr.py_str();
        validate_cel_expr(&expr, effect_path, loop_names)?;
        validate_cel_syntax(&expr, effect_path, effect_name, cel_label)?;
        let strict = get_bool_default(def, "strict", false);
        Ok(Condition::Cel { expr, strict })
    } else {
        let template = def
            .get(&Value::Str("template".to_string()))
            .filter(|v| crate::state_ns::is_truthy(v));
        let Some(template) = template else {
            return Err(CompileError(format!(
                "{message_prefix} at '{effect_path}': mode 'model' requires a 'template' field."
            )));
        };
        let template = template.py_str();
        let text = template_text(
            &template,
            effect_path,
            &format!("{field}.template"),
            false,
            Escape::Html,
        )?;
        Ok(Condition::Model { template: text })
    }
}

fn tree_loop_prev_references(effects: &Value, name: &str) -> bool {
    let pattern = format!("prime.{name}.prev");
    fn scan(value: &Value, pattern: &str) -> bool {
        match value {
            Value::Str(s) => word_contains(s, pattern),
            Value::Dict(d) => d.values().any(|v| scan(v, pattern)),
            Value::List(items) => items.iter().any(|v| scan(v, pattern)),
            _ => false,
        }
    }
    scan(effects, &pattern)
}

/// `\bprime\.{name}\.prev\b` with a regex-free, ASCII-word-boundary
/// check: the pattern is a fixed, already-escaped literal (`name` comes
/// from a validated effect name, which cannot itself contain regex
/// metacharacters), so a boundary check before/after the match is
/// enough to reproduce `\b` without a regex engine.
fn word_contains(haystack: &str, needle: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    if needle_bytes.is_empty() || needle_bytes.len() > bytes.len() {
        return false;
    }
    for start in 0..=(bytes.len() - needle_bytes.len()) {
        if &bytes[start..start + needle_bytes.len()] != needle_bytes {
            continue;
        }
        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word(c));
        let after_ok = haystack[start + needle_bytes.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_word(c));
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

fn compile_loop(
    ctx: &mut Ctx,
    effect: &Dict,
    path: &EffectPath,
    scope_path: &str,
    effect_path: &str,
    loop_names: &BTreeSet<String>,
) -> Result<Op, CompileError> {
    let validated_name = match effect.get(&Value::Str("name".to_string())) {
        Some(name) => Some(validate_name(name, "loop", scope_path, effect_path)?),
        None => None,
    };

    let has_while = effect.contains_key(&Value::Str("while".to_string()));
    let has_each = effect.contains_key(&Value::Str("each".to_string()));
    let mode_count = usize::from(has_while) + usize::from(has_each);
    if mode_count != 1 {
        let found = if mode_count == 0 { "neither" } else { "both" };
        return Err(CompileError(format!(
            "Loop at '{effect_path}' must set exactly one of 'while' or 'each' \
             (found {found})."
        )));
    }
    let mode_key = if has_while { "while" } else { "each" };
    let Some(Value::Dict(_)) = effect.get(&Value::Str(mode_key.to_string())) else {
        return Err(CompileError(format!(
            "Loop at '{effect_path}': '{mode_key}' must be a mapping."
        )));
    };

    let mut body_loop_names = loop_names.clone();
    body_loop_names.insert("iter".to_string());
    let mut each_as_name: Option<String> = None;
    if has_each {
        if let Some(Value::Dict(each_config)) = effect.get(&Value::Str("each".to_string())) {
            let as_name = get_truthy(each_config, "as")
                .map(Value::py_str)
                .unwrap_or_else(|| "item".to_string());
            body_loop_names.insert(as_name.clone());
            each_as_name = Some(as_name);
        }
    }

    let own_path = match &validated_name {
        Some(n) => path.push_name(n),
        None => path.clone(),
    };
    let body_scope = match &validated_name {
        Some(n) => scope_child(scope_path, n),
        None => scope_path.to_string(),
    };

    let body_val = effect.get(&Value::Str("body".to_string()));
    let body_present = body_val.is_some_and(crate::state_ns::is_truthy);
    let empty = Value::List(Vec::new());
    let body_list = if body_present {
        body_val.unwrap()
    } else {
        &empty
    };

    // The loop gets its own LoopId (for the body's path placeholder)
    // only once it is confirmed named -- an unnamed loop is transparent
    // and never needs one.
    let loop_path = match &validated_name {
        Some(_) => own_path.push_pass(ctx.next_loop_id()),
        None => own_path.clone(),
    };

    let mut body_seen = BTreeMap::new();
    let compiled_body = compile_effects_in_scope(
        ctx,
        body_list,
        &loop_path,
        &body_scope,
        &format!("{effect_path}.body"),
        &body_loop_names,
        &mut body_seen,
    )?;

    let mut while_def = None;
    let mut each_def = None;

    if has_while {
        if let Some(Value::Dict(while_config)) = effect.get(&Value::Str("while".to_string())) {
            let condition = compile_condition_labeled(
                while_config,
                effect_path,
                "Loop while",
                "while",
                "Loop while CEL expression",
                &body_loop_names,
                validated_name.as_deref(),
            )?;
            while_def = Some(LoopSpec::While(condition));
        }
    }

    if has_each {
        if let Some(Value::Dict(each_config)) = effect.get(&Value::Str("each".to_string())) {
            let in_path = get_truthy(each_config, "in")
                .map(Value::py_str)
                .unwrap_or_default();
            validate_each_in_path(&in_path, effect_path, loop_names)?;
            let truncate = get_bool_default(each_config, "truncate", false);
            each_def = Some(LoopSpec::Each {
                in_path,
                as_name: each_as_name.clone().unwrap_or_else(|| "item".to_string()),
                truncate,
            });
        }
    }

    let spec = while_def
        .or(each_def)
        .expect("exactly one mode checked above");

    let max_iterations = get_truthy(effect, "max_iterations")
        .map(py_int)
        .map(|n| n.max(0) as u32);
    let min_iterations = get_truthy(effect, "min_iterations")
        .map(py_int)
        .unwrap_or(0)
        .max(0) as u32;
    let on_error = normalize_on_error_loop(effect);

    let collect_raw = effect.get(&Value::Str("collect".to_string()));
    let collect = match collect_raw {
        Some(value) if !value.is_none() => {
            let text = value.py_str().trim().to_string();
            Some(text)
        }
        _ => None,
    };
    if let Some(collect_name) = &collect {
        if !collect_name.is_empty() && validated_name.is_none() {
            return Err(CompileError(format!(
                "Loop at '{effect_path}' sets 'collect: {collect_name}' but has no \
                 'name': collected values are written under the loop's own node, \
                 which an unnamed loop has none of. Give the loop a 'name' to fix \
                 this."
            )));
        }
    }

    let flow = normalize_flow(get_truthy(effect, "flow").map(Value::py_str).as_deref())?;

    if let (Some(name), LoopFlow::Tree, LoopSpec::Each { .. }) = (&validated_name, flow, &spec) {
        if tree_loop_prev_references(body_list, name) {
            return Err(CompileError(format!(
                "Loop '{name}' at '{effect_path}': \
                 'prime.{name}.prev' is not defined in flow: tree — \
                 tree passes run in parallel, so there is no previous pass to \
                 read. Use flow: chain (the default) if the body needs the \
                 previous pass, or remove the reference."
            )));
        }
    }

    let max_concurrency = get_truthy(effect, "max_concurrency")
        .map(py_int)
        .map(|n| n.max(0) as u32);
    let labels = labels_of(effect);

    let region = Region::Loop {
        spec,
        body: Box::new(Region::Block {
            ops: compiled_body,
            overlay: true,
        }),
        flow,
        max_concurrency,
        max_iterations,
        min_iterations,
        collect,
    };

    Ok(Op {
        path: own_path,
        name: validated_name,
        kind: NodeKind::Control(region),
        on_error,
        labels,
        enabled: true,
    })
}

#[cfg(test)]
mod depth_tests {
    use super::MAX_COMPILE_DEPTH;
    use crate::DocumentOrigin;
    use crate::compile::compile_document;
    use electricity_value::{Dict, Value};
    use std::path::PathBuf;

    fn origin() -> DocumentOrigin {
        DocumentOrigin::File {
            document_dir: PathBuf::from("/doc"),
            confinement_root: PathBuf::from("/doc"),
        }
    }

    /// `n` levels of nested, named `dynamic` effects, bottoming out in
    /// an empty effects list -- built directly as [`Value`] (not
    /// parsed YAML text), so this test's own stack usage stays
    /// trivial regardless of `n`.
    fn nested_dynamics(n: usize) -> Value {
        let mut inner = Value::List(Vec::new());
        for i in 0..n {
            let mut dict = Dict::new();
            dict.insert(
                Value::Str("type".to_string()),
                Value::Str("dynamic".to_string()),
            );
            dict.insert(Value::Str("name".to_string()), Value::Str(format!("n{i}")));
            dict.insert(Value::Str("effects".to_string()), inner);
            inner = Value::List(vec![Value::Dict(dict)]);
        }
        let mut root = Dict::new();
        root.insert(Value::Str("effects".to_string()), inner);
        Value::Dict(root)
    }

    #[test]
    fn exactly_at_the_depth_limit_compiles() {
        // `n` nested `dynamic` effects need `n + 1`
        // `compile_effects_in_scope` calls (the root `effects:` list,
        // plus one per dynamic's own child list, including the
        // innermost one's empty list) -- `n = MAX_COMPILE_DEPTH - 1`
        // is exactly at the limit.
        let document = nested_dynamics(MAX_COMPILE_DEPTH - 1);
        compile_document(&document, &origin())
            .unwrap_or_else(|err| panic!("expected success at exactly the limit, got: {}", err.0));
    }

    #[test]
    fn one_past_the_depth_limit_is_a_distinct_depth_error() {
        let document = nested_dynamics(MAX_COMPILE_DEPTH + 1);
        let err = compile_document(&document, &origin()).unwrap_err();
        assert!(
            err.0.contains("effect nesting is too deep to compile"),
            "unexpected error: {}",
            err.0
        );
    }
}
