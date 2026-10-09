//! Lane C: builds [`ParamNode`] trees for a tool effect's `params`
//! (`_check_param_leaves`/`_check_security_sensitive_param_leaf`,
//! `core/compiler.py`) and a `use` effect's `inputs`
//! (`_compile_use`'s own, shallower template/reference checks).
//!
//! `allowed_commands` (and any other
//! [`SECURITY_SENSITIVE_PARAM_KEYS`] key) must be a literal value
//! everywhere it appears inside a tool's `params` -- a by-reference
//! `{from: ...}` leaf anywhere inside it is rejected at compile time
//! (`core/tool.py::_reject_templated_security_params`'s compile-time
//! mirror).
//!
//! `use.inputs` is deliberately *not* walked the same way `params` is:
//! `core/compiler.py::_compile_use` only template-checks/reference-
//! checks a **top-level** string/`{from: ...}` value in `inputs`; a
//! nested dict/list value is neither template-checked nor recursed
//! into. Recursing into it here (reusing [`build_tool_params`]'s own
//! deeper walk) would reject a nested malformed template Circuitry's
//! own compiler accepts -- a stricter check than Python's own, which
//! the repository's own rule requires calling out rather than silently
//! introducing. [`build_use_inputs`] mirrors that shallowness exactly:
//! a nested dict/list value becomes a single [`ParamNode::Literal`]
//! carrying the raw value wholesale, unexamined.

use crate::CompileError;
use crate::compile::templates::check_templates;
use crate::state_ns::validate_reference_path;
use electricity_bytecode::{Escape, ParamNode, TemplateText};
use electricity_value::{Dict, Value};
use indexmap::IndexMap;
use std::collections::BTreeSet;

/// `core/tool.py::_SECURITY_SENSITIVE_PARAM_KEYS`.
pub(crate) const SECURITY_SENSITIVE_PARAM_KEYS: [&str; 1] = ["allowed_commands"];

/// A `{from: <path>}` (optionally with `default:`) leaf's path/default,
/// or `None` for any other value -- ports `core/tool.py::param_reference`.
/// Only a mapping with exactly the keys `{"from"}` or `{"from",
/// "default"}`, and a string `from` value, is a reference.
fn param_reference(value: &Value) -> Option<(String, Option<Value>)> {
    let Value::Dict(dict) = value else {
        return None;
    };
    let from_key = Value::Str("from".to_string());
    let default_key = Value::Str("default".to_string());
    match dict.len() {
        1 => {
            let path = dict.get(&from_key)?.as_str()?;
            Some((path.trim().to_string(), None))
        }
        2 if dict.contains_key(&from_key) && dict.contains_key(&default_key) => {
            let path = dict.get(&from_key)?.as_str()?;
            Some((
                path.trim().to_string(),
                Some(dict.get(&default_key)?.clone()),
            ))
        }
        _ => None,
    }
}

/// A `{from: <path>}` leaf (no `default:` support) for a `use` effect's
/// `inputs` -- ports `core/use.py::reference_path`: only a mapping with
/// exactly the one key `from` and a string value is a reference.
fn reference_path_only(value: &Value) -> Option<String> {
    let Value::Dict(dict) = value else {
        return None;
    };
    if dict.len() != 1 {
        return None;
    }
    let path = dict.get(&Value::Str("from".to_string()))?.as_str()?;
    Some(path.trim().to_string())
}

/// Ports `core/compiler.py::_check_security_sensitive_param_leaf`:
/// rejects a by-reference `{from: ...}` leaf anywhere inside a
/// security-sensitive param, recursively.
fn check_no_reference(value: &Value, name: &str, field: &str) -> Result<(), CompileError> {
    if param_reference(value).is_some() {
        return Err(CompileError(format!(
            "Tool effect '{name}' param '{field}': a by-reference '{{from: ...}}' \
             value is not honoured for this security-sensitive setting; it must \
             be a literal list of strings."
        )));
    }
    match value {
        Value::Dict(dict) => {
            for (key, item) in dict {
                let key_str = match key {
                    Value::Str(s) => s.clone(),
                    other => other.py_str(),
                };
                check_no_reference(item, name, &format!("{field}.{key_str}"))?;
            }
        }
        Value::List(items) => {
            for (index, item) in items.iter().enumerate() {
                check_no_reference(item, name, &format!("{field}[{index}]"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Ports `core/compiler.py::_check_param_leaves` plus the IR-building
/// half the Python reference has no counterpart for (it just keeps the
/// raw dict): a `{from: <path>}` leaf (at any depth) becomes
/// [`ParamNode::From`], checked against [`validate_reference_path`];
/// every other string leaf is checked as a (partials-allowed) template
/// and becomes [`ParamNode::Template`]; a nested dict/list becomes
/// [`ParamNode::Map`]/[`ParamNode::List`] of the same walk; anything
/// else is a [`ParamNode::Literal`].
fn build_param_node(
    value: &Value,
    name: &str,
    effect_path: &str,
    field: &str,
    loop_names: &BTreeSet<String>,
) -> Result<ParamNode, CompileError> {
    if let Some((path, default)) = param_reference(value) {
        validate_reference_path(
            &path,
            &format!("Tool effect '{name}' param '{field}'"),
            effect_path,
            loop_names,
        )?;
        return Ok(ParamNode::From { path, default });
    }
    match value {
        Value::Str(s) => {
            check_templates(value, effect_path, field, true)?;
            Ok(ParamNode::Template(TemplateText::new(
                s.clone(),
                true,
                Escape::Html,
            )))
        }
        Value::Dict(dict) => {
            let mut map = IndexMap::new();
            for (key, item) in dict {
                let key_label = match key {
                    Value::Str(s) => s.clone(),
                    other => other.py_str(),
                };
                let child = build_param_node(
                    item,
                    name,
                    effect_path,
                    &format!("{field}.{key_label}"),
                    loop_names,
                )?;
                map.insert(key.clone(), child);
            }
            Ok(ParamNode::Map(map))
        }
        Value::List(items) => {
            let mut list = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                list.push(build_param_node(
                    item,
                    name,
                    effect_path,
                    &format!("{field}[{index}]"),
                    loop_names,
                )?);
            }
            Ok(ParamNode::List(list))
        }
        other => Ok(ParamNode::Literal(other.clone())),
    }
}

/// Builds the [`ParamNode::Map`] for a tool effect's `params:`,
/// including the security-sensitive-key literal check.
pub(crate) fn build_tool_params(
    params: &Dict,
    name: &str,
    effect_path: &str,
    loop_names: &BTreeSet<String>,
) -> Result<ParamNode, CompileError> {
    // Python's `_compile_tool` runs `_check_param_leaves` (the template/
    // reference walk below) over the whole `params` dict first, then
    // `_check_security_sensitive_param_leaf` once per sensitive key --
    // so a malformed template anywhere in `params` is reported before a
    // by-reference `allowed_commands` leaf is.
    let node = build_param_node(
        &Value::Dict(params.clone()),
        name,
        effect_path,
        "params",
        loop_names,
    )?;
    for sensitive_key in SECURITY_SENSITIVE_PARAM_KEYS {
        if let Some(value) = params.get(&Value::Str(sensitive_key.to_string())) {
            check_no_reference(value, name, &format!("params.{sensitive_key}"))?;
        }
    }
    Ok(node)
}

/// Builds the [`ParamNode::Map`] for a `use` effect's `inputs:` -- see
/// this module's own docs for why this is deliberately shallower than
/// [`build_tool_params`].
pub(crate) fn build_use_inputs(
    inputs: &Dict,
    name: &str,
    effect_path: &str,
    loop_names: &BTreeSet<String>,
) -> Result<ParamNode, CompileError> {
    fn key_str(key: &Value) -> String {
        match key {
            Value::Str(s) => s.clone(),
            other => other.py_str(),
        }
    }

    // Python's `_compile_use` checks every input in two separate
    // passes over the whole `inputs` dict: every string value as a
    // template first, then every `{from: ...}` value's reference path
    // -- not interleaved key by key.
    for (key, value) in inputs {
        if let Value::Str(_) = value {
            check_templates(
                value,
                effect_path,
                &format!("inputs.{}", key_str(key)),
                true,
            )?;
        }
    }
    for (key, value) in inputs {
        if let Some(path) = reference_path_only(value) {
            validate_reference_path(
                &path,
                &format!("Use effect '{name}' input '{}'", key_str(key)),
                effect_path,
                loop_names,
            )?;
        }
    }

    let mut map = IndexMap::new();
    for (key, value) in inputs {
        if let Some(path) = reference_path_only(value) {
            map.insert(
                key.clone(),
                ParamNode::From {
                    path,
                    default: None,
                },
            );
            continue;
        }
        if let Value::Str(s) = value {
            map.insert(
                key.clone(),
                ParamNode::Template(TemplateText::new(s.clone(), true, Escape::Html)),
            );
            continue;
        }
        map.insert(key.clone(), ParamNode::Literal(value.clone()));
    }
    Ok(ParamNode::Map(map))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(pairs: Vec<(&str, Value)>) -> Dict {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        d
    }

    #[test]
    fn plain_literal_stays_literal() {
        let params = dict(vec![("n", Value::Int(5.into()))]);
        let node = build_tool_params(&params, "t", "p", &BTreeSet::new()).unwrap();
        match node {
            ParamNode::Map(m) => assert!(matches!(
                m.get(&Value::Str("n".to_string())),
                Some(ParamNode::Literal(Value::Int(_)))
            )),
            _ => panic!("expected a map"),
        }
    }

    #[test]
    fn non_string_keys_are_kept_as_value_not_stringified() {
        let mut params = Dict::new();
        params.insert(Value::Bool(true), Value::Int(1.into()));
        params.insert(Value::Int(5.into()), Value::Int(2.into()));
        params.insert(Value::Str("5".to_string()), Value::Int(3.into()));
        let node = build_tool_params(&params, "t", "p", &BTreeSet::new()).unwrap();
        match node {
            ParamNode::Map(m) => {
                assert!(matches!(
                    m.get(&Value::Bool(true)),
                    Some(ParamNode::Literal(Value::Int(_)))
                ));
                // `5` (int) and `"5"` (str) must stay distinct entries --
                // a `py_str`-stringifying compiler would collapse them
                // into one `"5"` key (Python's own numeric tower only
                // unifies a bool/int/float that compare equal, e.g.
                // `True`/`1`, never a number and its string spelling).
                assert_eq!(m.len(), 3);
                assert!(matches!(
                    m.get(&Value::Int(5.into())),
                    Some(ParamNode::Literal(Value::Int(_)))
                ));
                assert!(matches!(
                    m.get(&Value::Str("5".to_string())),
                    Some(ParamNode::Literal(Value::Int(_)))
                ));
            }
            _ => panic!("expected a map"),
        }
    }

    #[test]
    fn string_becomes_template() {
        let params = dict(vec![("s", Value::Str("hi {{x}}".to_string()))]);
        let node = build_tool_params(&params, "t", "p", &BTreeSet::new()).unwrap();
        match node {
            ParamNode::Map(m) => match m.get(&Value::Str("s".to_string())) {
                Some(ParamNode::Template(t)) => assert_eq!(t.source, "hi {{x}}"),
                _ => panic!("expected a template"),
            },
            _ => panic!("expected a map"),
        }
    }

    #[test]
    fn malformed_template_rejected() {
        let params = dict(vec![("s", Value::Str("{{#a}}".to_string()))]);
        assert!(build_tool_params(&params, "t", "p", &BTreeSet::new()).is_err());
    }

    #[test]
    fn from_reference_checked_against_namespaces() {
        let mut from_dict = Dict::new();
        from_dict.insert(
            Value::Str("from".to_string()),
            Value::Str("bogus".to_string()),
        );
        let params = dict(vec![("x", Value::Dict(from_dict))]);
        let err = build_tool_params(&params, "t", "p", &BTreeSet::new()).unwrap_err();
        assert!(err.0.contains("not rooted at a state namespace"));
    }

    #[test]
    fn from_reference_with_default_builds_from_node() {
        let mut from_dict = Dict::new();
        from_dict.insert(
            Value::Str("from".to_string()),
            Value::Str("input.x".to_string()),
        );
        from_dict.insert(Value::Str("default".to_string()), Value::Int(1.into()));
        let params = dict(vec![("x", Value::Dict(from_dict))]);
        let node = build_tool_params(&params, "t", "p", &BTreeSet::new()).unwrap();
        match node {
            ParamNode::Map(m) => match m.get(&Value::Str("x".to_string())) {
                Some(ParamNode::From { path, default }) => {
                    assert_eq!(path, "input.x");
                    assert_eq!(default, &Some(Value::Int(1.into())));
                }
                _ => panic!("expected a from-reference"),
            },
            _ => panic!("expected a map"),
        }
    }

    #[test]
    fn security_sensitive_rejects_a_reference_anywhere_inside() {
        let mut from_dict = Dict::new();
        from_dict.insert(
            Value::Str("from".to_string()),
            Value::Str("input.cmds".to_string()),
        );
        let params = dict(vec![(
            "allowed_commands",
            Value::List(vec![Value::Dict(from_dict)]),
        )]);
        let err = build_tool_params(&params, "t", "p", &BTreeSet::new()).unwrap_err();
        assert!(err.0.contains("security-sensitive setting"));
    }

    #[test]
    fn security_sensitive_literal_list_is_fine() {
        let params = dict(vec![(
            "allowed_commands",
            Value::List(vec![Value::Str("ls".to_string())]),
        )]);
        assert!(build_tool_params(&params, "t", "p", &BTreeSet::new()).is_ok());
    }

    #[test]
    fn use_inputs_does_not_recurse_into_nested_malformed_templates() {
        let mut nested = Dict::new();
        nested.insert(
            Value::Str("inner".to_string()),
            Value::Str("{{#bad}}".to_string()),
        );
        let inputs = dict(vec![("outer", Value::Dict(nested))]);
        // Deliberately not rejected: core/compiler.py's own _compile_use
        // never checks a nested value inside `inputs`.
        assert!(build_use_inputs(&inputs, "u", "p", &BTreeSet::new()).is_ok());
    }

    #[test]
    fn use_inputs_checks_a_top_level_string_template() {
        let inputs = dict(vec![("greeting", Value::Str("{{#bad}}".to_string()))]);
        assert!(build_use_inputs(&inputs, "u", "p", &BTreeSet::new()).is_err());
    }

    #[test]
    fn use_inputs_reference_has_no_default_support() {
        let mut from_dict = Dict::new();
        from_dict.insert(
            Value::Str("from".to_string()),
            Value::Str("input.x".to_string()),
        );
        from_dict.insert(Value::Str("default".to_string()), Value::Int(1.into()));
        let inputs = dict(vec![("x", Value::Dict(from_dict))]);
        // Two keys -> not a reference per core/use.py::reference_path -> literal passthrough.
        let node = build_use_inputs(&inputs, "u", "p", &BTreeSet::new()).unwrap();
        match node {
            ParamNode::Map(m) => assert!(matches!(
                m.get(&Value::Str("x".to_string())),
                Some(ParamNode::Literal(Value::Dict(_)))
            )),
            _ => panic!("expected a map"),
        }
    }
}
