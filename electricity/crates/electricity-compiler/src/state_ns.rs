//! Lane C: ports `core/state_ns.py` -- the root-namespace contract for
//! orchestration state. Checks a by-reference `{from: <path>}` leaf's
//! path, a loop `each.in` path, and a `mode: cel` expression's
//! `state.<key>` paths (via [`electricity_cel::state_paths`]) against
//! the enclosing loops' bindings, plus the bare-`{{name}}` reference
//! check against a document's declared `interface.inputs` names.
//!
//! `validate_cel_syntax` here is `core/cel_eval.py`'s own function of
//! the same name, not `core/state_ns.py`'s (Python re-exports it via
//! `core/compiler.py`'s own `from .cel_eval import validate_cel_syntax`
//! import) -- kept in this module since every call site in this crate
//! is itself a `state_ns`-flavored check (`_compile_expect`/
//! `_compile_conditional`/`_compile_loop` always pair it with
//! `validate_cel_expr` immediately beside it in `core/compiler.py`).

use crate::CompileError;
use electricity_value::Value;
use std::collections::BTreeSet;

/// `core/state_ns.py`'s `NAMESPACES` -- the only legal root namespaces.
pub(crate) const NAMESPACES: [&str; 3] = ["input", "prime", "runtime"];

fn is_namespace(root: &str) -> bool {
    NAMESPACES.contains(&root)
}

/// `core/state_ns.py::migrate_legacy_state`'s own `input`-namespace
/// rule, applied to *inline* (the CLI's own `-e` entries, already
/// JSON-sniffed and string-restored -- `pipeline::cli_input_namespace`'s
/// own input): an `input` key, if present, wins outright -- every other
/// key in *inline* is ignored, and the returned namespace is that
/// key's own value if it is a dict, or empty otherwise (Python's
/// `if INPUT_NS in state: return state` followed by
/// `cli/runtime_shim.py::run`'s own `if not isinstance(input_ns, dict):
/// input_ns = {}`). Otherwise every key that is neither a namespace
/// name (`NAMESPACES`) nor `_`-prefixed is lifted into the returned
/// namespace -- a key named `prime`/`runtime`, or `_`-prefixed, is
/// never lifted and so can never satisfy a declared `interface.inputs`
/// entry of the same name via `-e` (`core/state_ns.py`'s own
/// `key not in NAMESPACES and not key.startswith("_")`).
pub fn migrate_legacy_input_namespace(
    inline: &indexmap::IndexMap<String, Value>,
) -> indexmap::IndexMap<String, Value> {
    if let Some(value) = inline.get("input") {
        return match value {
            Value::Dict(dict) => dict
                .iter()
                .filter_map(|(k, v)| match k {
                    Value::Str(s) => Some((s.clone(), v.clone())),
                    _ => None,
                })
                .collect(),
            _ => indexmap::IndexMap::new(),
        };
    }
    inline
        .iter()
        .filter(|(key, _)| !is_namespace(key) && !key.starts_with('_'))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Hard-errors unless a by-reference `{from: <path>}` leaf has a legal
/// root: rooted at `input.`/`prime.`/`runtime.`, or a binding of an
/// enclosing loop (`each.as`, `iter`). Ports
/// `core/state_ns.py::validate_reference_path`.
pub(crate) fn validate_reference_path(
    path: &str,
    label: &str,
    effect_path: &str,
    loop_names: &BTreeSet<String>,
) -> Result<(), CompileError> {
    let where_ = format!("{label} at '{effect_path}'");
    if path.is_empty() {
        return Err(CompileError(format!(
            "{where_}: '{{from: ...}}' needs a non-empty path."
        )));
    }
    let (root, rest) = split_first_segment(path);
    if is_namespace(root) || loop_names.contains(root) {
        return Ok(());
    }
    if root == "state" {
        let rest_root = split_first_segment(rest).0;
        if is_namespace(rest_root) {
            return Err(CompileError(format!(
                "{where_}: 'state.' is a CEL-only binding; outside CEL, paths are \
                 root-relative. Write '{rest}' instead of '{path}'."
            )));
        }
    }
    let bindings = loop_bindings_desc(loop_names);
    Err(CompileError(format!(
        "{where_}: '{path}' is not rooted at a state namespace or an enclosing \
         loop binding (loop bindings in scope: {bindings}). Write \
         'input.{path}' for caller-supplied values or 'prime.<effect>.value' \
         for effect outputs."
    )))
}

/// Hard-errors unless a loop `each.in` path is rooted at a namespace.
/// Ports `core/state_ns.py::validate_each_in_path`.
pub(crate) fn validate_each_in_path(
    path: &str,
    effect_path: &str,
    loop_names: &BTreeSet<String>,
) -> Result<(), CompileError> {
    let (root, rest) = split_first_segment(path);
    if is_namespace(root) || loop_names.contains(root) {
        return Ok(());
    }
    if root == "state" {
        let rest_root = split_first_segment(rest).0;
        if is_namespace(rest_root) {
            return Err(CompileError(format!(
                "Loop each.in at '{effect_path}': 'state.' is a CEL-only \
                 binding; outside CEL, paths are root-relative. \
                 Write '{rest}' instead of '{path}'."
            )));
        }
        let suffix = if rest.is_empty() { "<key>" } else { rest };
        return Err(CompileError(format!(
            "Loop each.in at '{effect_path}': '{path}' is not rooted at a \
             state namespace. Write 'input.{suffix}' for caller-supplied \
             values or 'prime.{suffix}' for effect outputs."
        )));
    }
    let bindings = loop_bindings_desc(loop_names);
    if path.is_empty() {
        return Err(CompileError(format!(
            "Loop each.in at '{effect_path}' must be a dot path rooted at \
             'input.', 'prime.', or 'runtime.' (e.g. 'input.items'), or an \
             enclosing loop binding (loop bindings in scope: {bindings})."
        )));
    }
    Err(CompileError(format!(
        "Loop each.in at '{effect_path}': bare key '{path}' is not rooted \
         at a state namespace or an enclosing loop binding (loop bindings \
         in scope: {bindings}). Write 'input.{path}' for caller-supplied \
         values or 'prime.{path}' for effect outputs."
    )))
}

/// `path.partition(".")` -- the first dot-separated segment, and
/// everything after the first `.` (empty if there is none).
fn split_first_segment(path: &str) -> (&str, &str) {
    match path.split_once('.') {
        Some((root, rest)) => (root, rest),
        None => (path, ""),
    }
}

fn loop_bindings_desc(loop_names: &BTreeSet<String>) -> String {
    if loop_names.is_empty() {
        "none here".to_string()
    } else {
        loop_names.iter().cloned().collect::<Vec<_>>().join(", ")
    }
}

/// Hard-errors on `state.<key>` where `<key>` is not a namespace, taken
/// off the CEL parse tree -- a quoted `'state.topic'` is a string
/// literal, not a violation. An expression that does not parse is left
/// alone here (`core/cel_eval.py::validate_cel_syntax` reports that
/// separately, with the parser's own message). Ports
/// `core/state_ns.py::validate_cel_expr`.
pub(crate) fn validate_cel_expr(
    expr: &str,
    effect_path: &str,
    extra_names: &BTreeSet<String>,
) -> Result<(), CompileError> {
    let paths = match electricity_cel::state_paths(expr) {
        Ok(paths) => paths,
        Err(_) => return Ok(()),
    };
    for path in paths {
        let key = path.split('.').nth(1).unwrap_or("");
        if !is_namespace(key) && !extra_names.contains(key) {
            let extra_desc = if extra_names.is_empty() {
                String::new()
            } else {
                format!(
                    " Names bound by an enclosing loop here: {}.",
                    extra_names.iter().cloned().collect::<Vec<_>>().join(", ")
                )
            };
            return Err(CompileError(format!(
                "CEL expression at '{effect_path}': 'state.{key}' does not \
                 name a state namespace ('state' binds to the state root). \
                 Write 'state.input.{key}' for caller-supplied values or \
                 'state.prime.{key}' for effect outputs.{extra_desc}"
            )));
        }
    }
    Ok(())
}

/// Compile-time check for a single `mode: cel` expression -- ports
/// `core/cel_eval.py::validate_cel_syntax` (re-exported, not redefined,
/// by `core/compiler.py`), wrapping
/// [`electricity_cel::validate_syntax`]'s bare error with this crate's
/// own `effect_path`/`effect_name`/`label` addressing context, exactly
/// the way Python's version does.
pub(crate) fn validate_cel_syntax(
    expr: &str,
    effect_path: &str,
    effect_name: Option<&str>,
    label: &str,
) -> Result<(), CompileError> {
    let mut where_ = format!("{label} at '{effect_path}'");
    if let Some(name) = effect_name {
        if !name.is_empty() {
            where_ = format!("{where_} (effect '{name}')");
        }
    }
    match electricity_cel::validate_syntax(expr) {
        Ok(()) => Ok(()),
        Err(err) if err.is_parse_error() => Err(CompileError(format!(
            "{where_}: {err} Expression: {}",
            Value::Str(expr.to_string()).py_repr()
        ))),
        Err(err) => Err(CompileError(format!("{where_}: {err}"))),
    }
}

/// The input names a document declares in its `interface.inputs`.
/// Ports `core/state_ns.py::declared_input_names`.
fn declared_input_names(document: &Value) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let Some(interface) = document
        .as_dict()
        .and_then(|d| d.get(&Value::Str("interface".to_string())))
        .and_then(Value::as_dict)
    else {
        return names;
    };
    let Some(inputs) = interface
        .get(&Value::Str("inputs".to_string()))
        .and_then(Value::as_dict)
    else {
        return names;
    };
    for key in inputs.keys() {
        if let Value::Str(s) = key {
            names.insert(s.clone());
        }
    }
    names
}

/// Mustache tag keys: `{{name}}`, `{{{name}}}`, `{{&name}}`, and the
/// section forms `{{#name}}`/`{{^name}}`/`{{/name}}`. Comments and
/// partials never match because `!`/`>` are in neither character class.
fn mustache_tag_regex() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"\{\{\{?\s*[#^/&]?\s*([A-Za-z0-9_.\-]+)\s*\}?\}\}").unwrap()
    })
}

/// Hard-errors on bare `{{name}}` refs to this document's declared
/// inputs. Ports `core/state_ns.py::validate_bare_input_refs`.
pub(crate) fn validate_bare_input_refs(document: &Value) -> Result<(), CompileError> {
    let declared = declared_input_names(document);
    if declared.is_empty() {
        return Ok(());
    }
    let Some(dict) = document.as_dict() else {
        return Ok(());
    };
    let effects = dict_get_truthy_or(dict, "effects", "steps");
    if let Some(Value::List(items)) = effects {
        walk_bare_refs(items, &declared, "effects")?;
    }
    if let Some(Value::List(items)) = dict.get(&Value::Str("finally".to_string())) {
        walk_bare_refs(items, &declared, "finally")?;
    }
    Ok(())
}

fn dict_get_truthy_or<'a>(
    dict: &'a electricity_value::Dict,
    primary: &str,
    fallback: &str,
) -> Option<&'a Value> {
    let primary_val = dict.get(&Value::Str(primary.to_string()));
    if primary_val.is_some_and(is_truthy) {
        return primary_val;
    }
    dict.get(&Value::Str(fallback.to_string()))
}

pub(crate) fn is_truthy(value: &Value) -> bool {
    match value {
        Value::None => false,
        Value::Bool(b) => *b,
        Value::Int(i) => !i.is_zero(),
        Value::Float(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::Bytes(b) => !b.is_empty(),
        Value::List(items) => !items.is_empty(),
        Value::Dict(d) => !d.is_empty(),
        Value::Date(_) | Value::DateTime(..) => true,
    }
}

const CHILD_LISTS: [&str; 6] = ["effects", "steps", "body", "then", "else", "finally"];

fn walk_bare_refs(
    effects: &[Value],
    declared: &BTreeSet<String>,
    container_path: &str,
) -> Result<(), CompileError> {
    for (idx, effect) in effects.iter().enumerate() {
        let Some(dict) = effect.as_dict() else {
            continue;
        };
        let effect_path = format!("{container_path}[{idx}]");
        for (field, text) in iter_template_strings(dict) {
            check_bare_refs(&text, declared, &format!("{effect_path}.{field}"))?;
        }
        for field in CHILD_LISTS {
            if let Some(Value::List(children)) = dict.get(&Value::Str(field.to_string())) {
                walk_bare_refs(children, declared, &format!("{effect_path}.{field}"))?;
            }
        }
    }
    Ok(())
}

/// Every (field-label, text) pair the runtime Mustache-renders -- ports
/// `core/state_ns.py::_iter_template_strings`.
fn iter_template_strings(effect: &electricity_value::Dict) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for field in ["template", "prompt", "inline", "params_json"] {
        if let Some(Value::Str(s)) = effect.get(&Value::Str(field.to_string())) {
            found.push((field.to_string(), s.clone()));
        }
    }
    if let Some(Value::List(messages)) = effect.get(&Value::Str("messages".to_string())) {
        for (i, message) in messages.iter().enumerate() {
            if let Some(Value::Str(content)) = message
                .as_dict()
                .and_then(|d| d.get(&Value::Str("content".to_string())))
            {
                found.push((format!("messages[{i}].content"), content.clone()));
            }
        }
    }
    for field in ["inputs", "params"] {
        if let Some(value) = effect.get(&Value::Str(field.to_string())) {
            found.extend(nested_strings(value, field));
        }
    }
    for field in ["while", "if"] {
        if let Some(Value::Str(template)) = effect
            .get(&Value::Str(field.to_string()))
            .and_then(Value::as_dict)
            .and_then(|d| d.get(&Value::Str("template".to_string())))
        {
            found.push((format!("{field}.template"), template.clone()));
        }
    }
    found
}

fn nested_strings(value: &Value, prefix: &str) -> Vec<(String, String)> {
    match value {
        Value::Str(s) => vec![(prefix.to_string(), s.clone())],
        Value::Dict(d) => d
            .iter()
            .flat_map(|(key, child)| {
                let key_str = match key {
                    Value::Str(s) => s.clone(),
                    other => other.py_str(),
                };
                nested_strings(child, &format!("{prefix}.{key_str}"))
            })
            .collect(),
        Value::List(items) => items
            .iter()
            .enumerate()
            .flat_map(|(i, child)| nested_strings(child, &format!("{prefix}[{i}]")))
            .collect(),
        _ => Vec::new(),
    }
}

fn check_bare_refs(
    text: &str,
    declared: &BTreeSet<String>,
    location: &str,
) -> Result<(), CompileError> {
    for capture in mustache_tag_regex().captures_iter(text) {
        let key = &capture[1];
        let root = key.split('.').next().unwrap_or(key);
        if is_namespace(root) {
            continue;
        }
        if declared.contains(root) {
            return Err(CompileError(format!(
                "Template at '{location}': bare '{{{{{key}}}}}' refers to \
                 declared interface input '{root}'. Caller inputs live \
                 under the 'input' namespace; write \
                 '{{{{input.{key}}}}}'."
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    fn names(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reference_path_accepts_a_namespace_root() {
        assert!(validate_reference_path("input.x", "label", "p", &BTreeSet::new()).is_ok());
        assert!(validate_reference_path("prime.x.value", "label", "p", &BTreeSet::new()).is_ok());
    }

    #[test]
    fn reference_path_accepts_a_loop_binding() {
        assert!(validate_reference_path("item.name", "label", "p", &names(&["item"])).is_ok());
    }

    #[test]
    fn reference_path_rejects_empty() {
        let err = validate_reference_path("", "Use effect 'x' input 'y'", "p", &BTreeSet::new())
            .unwrap_err();
        assert_eq!(
            err.0,
            "Use effect 'x' input 'y' at 'p': '{from: ...}' needs a non-empty path."
        );
    }

    #[test]
    fn reference_path_rejects_state_prefixed() {
        let err =
            validate_reference_path("state.input.x", "label", "p", &BTreeSet::new()).unwrap_err();
        assert!(err.0.contains("'state.' is a CEL-only binding"));
        assert!(err.0.contains("Write 'input.x' instead of 'state.input.x'"));
    }

    #[test]
    fn reference_path_rejects_bare_key() {
        let err = validate_reference_path("items", "label", "p", &BTreeSet::new()).unwrap_err();
        assert!(err.0.contains("not rooted at a state namespace"));
        assert!(err.0.contains("none here"));
    }

    #[test]
    fn each_in_path_accepts_namespace_and_loop_binding() {
        assert!(validate_each_in_path("input.items", "p", &BTreeSet::new()).is_ok());
        assert!(validate_each_in_path("item.rows", "p", &names(&["item"])).is_ok());
    }

    #[test]
    fn each_in_path_rejects_state_prefixed() {
        let err = validate_each_in_path("state.input.x", "p", &BTreeSet::new()).unwrap_err();
        assert!(err.0.contains("'state.' is a CEL-only binding"));
    }

    #[test]
    fn each_in_path_rejects_bare_key() {
        let err = validate_each_in_path("items", "p", &BTreeSet::new()).unwrap_err();
        assert!(err.0.contains("bare key 'items'"));
    }

    #[test]
    fn each_in_path_rejects_empty() {
        let err = validate_each_in_path("", "p", &BTreeSet::new()).unwrap_err();
        assert!(err.0.contains("must be a dot path rooted at"));
    }

    #[test]
    fn cel_expr_accepts_namespace_paths() {
        assert!(validate_cel_expr("state.input.x == 1", "p", &BTreeSet::new()).is_ok());
    }

    #[test]
    fn cel_expr_accepts_loop_binding() {
        assert!(validate_cel_expr("state.item.x == 1", "p", &names(&["item"])).is_ok());
    }

    #[test]
    fn cel_expr_rejects_non_namespace_key() {
        let err = validate_cel_expr("state.bogus.x == 1", "p", &BTreeSet::new()).unwrap_err();
        assert!(
            err.0
                .contains("'state.bogus' does not name a state namespace")
        );
    }

    #[test]
    fn cel_expr_swallows_a_parse_failure() {
        assert!(validate_cel_expr("state.a ==", "p", &BTreeSet::new()).is_ok());
    }

    #[test]
    fn cel_syntax_reports_empty() {
        let err = validate_cel_syntax("", "p", None, "CEL expression").unwrap_err();
        assert_eq!(err.0, "CEL expression at 'p': expression is empty.");
    }

    #[test]
    fn cel_syntax_reports_a_parse_failure_with_the_expression_repr() {
        let err =
            validate_cel_syntax("state.a ==", "p", Some("cond"), "CEL expression").unwrap_err();
        assert!(
            err.0
                .starts_with("CEL expression at 'p' (effect 'cond'): expression does not parse (")
        );
        assert!(err.0.ends_with("Expression: 'state.a =='"));
    }

    #[test]
    fn bare_input_refs_pass_when_nothing_declared() {
        let document = Value::Dict(Dict::new());
        assert!(validate_bare_input_refs(&document).is_ok());
    }

    #[test]
    fn bare_input_refs_rejects_a_declared_name() {
        let mut interface = Dict::new();
        let mut inputs = Dict::new();
        inputs.insert(Value::Str("topic".to_string()), Value::Dict(Dict::new()));
        interface.insert(Value::Str("inputs".to_string()), Value::Dict(inputs));

        let mut effect = Dict::new();
        effect.insert(
            Value::Str("type".to_string()),
            Value::Str("prompt".to_string()),
        );
        effect.insert(Value::Str("name".to_string()), Value::Str("p".to_string()));
        effect.insert(
            Value::Str("template".to_string()),
            Value::Str("{{topic}}".to_string()),
        );

        let mut document = Dict::new();
        document.insert(Value::Str("interface".to_string()), Value::Dict(interface));
        document.insert(
            Value::Str("effects".to_string()),
            Value::List(vec![Value::Dict(effect)]),
        );

        let err = validate_bare_input_refs(&Value::Dict(document)).unwrap_err();
        assert!(
            err.0
                .contains("bare '{{topic}}' refers to declared interface input 'topic'")
        );
        assert!(err.0.contains("write '{{input.topic}}'"));
    }

    #[test]
    fn bare_input_refs_allows_namespaced_refs() {
        let mut interface = Dict::new();
        let mut inputs = Dict::new();
        inputs.insert(Value::Str("topic".to_string()), Value::Dict(Dict::new()));
        interface.insert(Value::Str("inputs".to_string()), Value::Dict(inputs));

        let mut effect = Dict::new();
        effect.insert(
            Value::Str("type".to_string()),
            Value::Str("prompt".to_string()),
        );
        effect.insert(Value::Str("name".to_string()), Value::Str("p".to_string()));
        effect.insert(
            Value::Str("template".to_string()),
            Value::Str("{{input.topic}}".to_string()),
        );

        let mut document = Dict::new();
        document.insert(Value::Str("interface".to_string()), Value::Dict(interface));
        document.insert(
            Value::Str("effects".to_string()),
            Value::List(vec![Value::Dict(effect)]),
        );

        assert!(validate_bare_input_refs(&Value::Dict(document)).is_ok());
    }

    fn inline(pairs: Vec<(&str, Value)>) -> indexmap::IndexMap<String, Value> {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    #[test]
    fn plain_keys_are_lifted_under_input() {
        let result = migrate_legacy_input_namespace(&inline(vec![(
            "name",
            Value::Str("World".to_string()),
        )]));
        assert_eq!(result.get("name"), Some(&Value::Str("World".to_string())));
    }

    #[test]
    fn underscore_prefixed_keys_are_never_lifted() {
        let result =
            migrate_legacy_input_namespace(&inline(vec![("_token", Value::Str("x".to_string()))]));
        assert!(result.is_empty());
    }

    #[test]
    fn namespace_named_keys_are_never_lifted() {
        let result = migrate_legacy_input_namespace(&inline(vec![("prime", Value::from(5i64))]));
        assert!(result.is_empty());
    }

    #[test]
    fn an_input_key_wins_outright_and_nothing_else_is_lifted() {
        let mut nested = Dict::new();
        nested.insert(Value::Str("name".to_string()), Value::Str("W".to_string()));
        let result = migrate_legacy_input_namespace(&inline(vec![
            ("input", Value::Dict(nested)),
            ("other", Value::Str("ignored".to_string())),
        ]));
        assert_eq!(result.get("name"), Some(&Value::Str("W".to_string())));
        assert_eq!(result.get("other"), None);
    }

    #[test]
    fn a_non_dict_input_key_becomes_an_empty_namespace() {
        let result = migrate_legacy_input_namespace(&inline(vec![("input", Value::from(5i64))]));
        assert!(result.is_empty());
    }
}
