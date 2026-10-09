//! Lane C: tool/`use` params rendering -- `ParamNode` -> `Value`, the
//! `{from: ...}` walk (with `default:`), Mustache leaves, and a tool's
//! `params_json` overlay (`core/tool.py::_render_params`/
//! `_render_params_json`/`_deep_merge_params`, ported exactly, minus the
//! `{{> name}}` prompt-composition splice: out of scope through M0-H --
//! a document containing one is refused before a run starts, so plain
//! Mustache rendering is `render_with_composition`'s own behaviour on
//! every document that reaches this point).
//!
//! `exec::tool::execute_tool` (this crate's own lane C stub, filled in
//! once lane B's `Store` lands) is the only caller: it renders a tool's
//! `params:` through [`render_params`], its `params_json:` (if any)
//! through [`render_params_json`], and deep-merges the two through
//! [`deep_merge_params`] -- the `params_json` override takes precedence,
//! same as `core/tool.py`.

use electricity_bytecode::ParamNode;
use electricity_json::WriteMode;
use electricity_template::{JsonAwareCtx, PlainCtx, render_template};
use electricity_value::{Dict, Value};
use indexmap::IndexMap;
use std::fmt;

/// `core/tool.py::_render_params`'s own failure: a `{from: ...}` leaf
/// with no `default:` that didn't resolve, or a Mustache leaf that
/// failed to render (a malformed template, or one that failed against
/// the data it was handed -- [`electricity_template::TemplateError`]'s
/// own two-tier message, unchanged).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderParamsError {
    UnresolvedReference {
        effect_name: String,
        path: String,
        reference_path: String,
    },
    Template(String),
}

impl fmt::Display for RenderParamsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderParamsError::UnresolvedReference {
                effect_name,
                path,
                reference_path,
            } => write!(
                f,
                "Tool effect '{effect_name}' param '{path}': '{{from: {reference_path}}}' did not resolve to a value."
            ),
            RenderParamsError::Template(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for RenderParamsError {}

/// Renders *params* (a tool effect's own `params:`, already compiled to
/// [`ParamNode`]) against *ctx* (the materialized state snapshot
/// `{from: ...}`/`{{ }}` read from) -- `core/tool.py::_render_params`.
/// *effect_name* names the tool effect, for
/// [`RenderParamsError::UnresolvedReference`]'s own message.
///
/// A `{from: ...}` leaf, at any depth, resolves typed and unrendered
/// (never passed through Mustache); a nested [`ParamNode::Map`] stays
/// keyed by [`Value`] throughout -- a non-`str` key (`yes:`/`1:`) is
/// never stringified here, only once (if ever) the rendered params
/// reach a JSON boundary (`electricity_json::stringify_key`, the `json`
/// tool's own output rule).
pub fn render_params(
    effect_name: &str,
    params: &IndexMap<Value, ParamNode>,
    ctx: &Value,
) -> Result<Dict, RenderParamsError> {
    let mut rendered = IndexMap::with_capacity(params.len());
    for (key, node) in params {
        let path = format!("params.{}", key.py_str());
        let value = render_node(effect_name, node, ctx, &path)?;
        rendered.insert(key.clone(), value);
    }
    Ok(rendered)
}

fn render_node(
    effect_name: &str,
    node: &ParamNode,
    ctx: &Value,
    path: &str,
) -> Result<Value, RenderParamsError> {
    match node {
        ParamNode::Literal(value) => Ok(value.clone()),
        ParamNode::Template(text) => render_template(&text.source, ctx, &PlainCtx, path)
            .map(Value::Str)
            .map_err(|err| RenderParamsError::Template(err.to_string())),
        ParamNode::From {
            path: reference_path,
            default,
        } => match resolve_reference(ctx, reference_path) {
            Some(value) => Ok(value),
            None => match default {
                Some(value) => Ok(value.clone()),
                None => Err(RenderParamsError::UnresolvedReference {
                    effect_name: effect_name.to_string(),
                    path: path.to_string(),
                    reference_path: reference_path.clone(),
                }),
            },
        },
        ParamNode::Map(entries) => {
            let mut map = IndexMap::with_capacity(entries.len());
            for (key, child) in entries {
                let child_path = format!("{path}.{}", key.py_str());
                let value = render_node(effect_name, child, ctx, &child_path)?;
                map.insert(key.clone(), value);
            }
            Ok(Value::Dict(map))
        }
        ParamNode::List(items) => {
            let mut list = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let child_path = format!("{path}[{index}]");
                list.push(render_node(effect_name, item, ctx, &child_path)?);
            }
            Ok(Value::List(list))
        }
    }
}

/// `core/use.py::_resolve_reference` -- walks *path* (dot-separated)
/// through *ctx*: a mapping segment looks the next part up by exact key;
/// a list segment parses the next part as a Python-style (negative-
/// wraps) index. `None` on any missing key, out-of-range index, or a
/// segment that indexes into a non-container -- never an error, same as
/// an unset template path.
fn resolve_reference(ctx: &Value, path: &str) -> Option<Value> {
    let mut current = ctx.clone();
    for part in path.split('.') {
        current = match &current {
            Value::Dict(dict) => dict.get(&Value::Str(part.to_string()))?.clone(),
            Value::List(items) => {
                let index: i64 = part.parse().ok()?;
                let resolved = python_list_index(items.len(), index)?;
                items[resolved].clone()
            }
            _ => return None,
        };
    }
    Some(current)
}

/// Python list indexing (negative indices count from the end) -- `None`
/// on any out-of-range index, never a panic. Shared in spirit (not in
/// code: each crate ported from `core/tool.py`'s own small helpers
/// keeps its own copy rather than adding a cross-crate dependency for a
/// six-line function) with `electricity_tools::json`'s identical rule
/// for the `json` tool's own `extract` path walk.
fn python_list_index(len: usize, index: i64) -> Option<usize> {
    let len = len as i64;
    let normalized = if index < 0 { index + len } else { index };
    if normalized < 0 || normalized >= len {
        None
    } else {
        Some(normalized as usize)
    }
}

/// `core/tool.py::_render_params_json`'s own failure: a malformed/
/// unrenderable Mustache template, a result that isn't valid JSON, or
/// valid JSON that isn't an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderParamsJsonError {
    Template(String),
    /// Third-party JSON-decode text (DESIGN.md \u00a71/\u00a712): only has to
    /// fail at the same position CPython's `json.JSONDecodeError` would,
    /// not match it byte for byte.
    Json(String),
    NotObject(String),
}

impl fmt::Display for RenderParamsJsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderParamsJsonError::Template(message) | RenderParamsJsonError::Json(message) => {
                write!(f, "{message}")
            }
            RenderParamsJsonError::NotObject(type_name) => write!(
                f,
                "params_json must render to a JSON object (dict), got {type_name}"
            ),
        }
    }
}

impl std::error::Error for RenderParamsJsonError {}

fn json_aware_splice(value: &Value) -> Result<String, String> {
    electricity_json::dumps_default_str(
        value,
        WriteMode {
            indent: None,
            sort_keys: false,
            ensure_ascii: false,
        },
    )
    .map_err(|err| err.to_string())
}

/// Renders *template* (a tool effect's own `params_json:`) against
/// *ctx* through [`JsonAwareCtx`] (DESIGN.md \u00a73.3's Quirk Q2: a
/// `{{{...}}}` splice of a native list/dict value serializes as real
/// JSON, never falls back to `""` just because it's empty), then parses
/// the rendered text as JSON -- `core/tool.py::_render_params_json`. The
/// result must be a JSON object; anything else is
/// [`RenderParamsJsonError::NotObject`].
pub fn render_params_json(template: &str, ctx: &Value) -> Result<Dict, RenderParamsJsonError> {
    let serializer: &dyn Fn(&Value) -> Result<String, String> = &json_aware_splice;
    let json_ctx = JsonAwareCtx::new(serializer);
    let rendered_text = render_template(template, ctx, &json_ctx, "params_json")
        .map_err(|err| RenderParamsJsonError::Template(err.to_string()))?;
    let mut parsed = electricity_json::loads(&rendered_text).map_err(|err| {
        RenderParamsJsonError::Json(format!("params_json did not render to valid JSON: {err}"))
    })?;
    // `Value::Dict(dict) = parsed` would move `dict` out of a type that
    // implements `Drop` (electricity-value's iterative `Drop` impl),
    // which Rust never allows -- match on `&mut parsed` and `mem::take`
    // the container out instead (same fix as `electricity-redaction`'s
    // own `redact`, `electricity-template/src/render.rs`'s original).
    match &mut parsed {
        Value::Dict(dict) => Ok(std::mem::take(dict)),
        other => Err(RenderParamsJsonError::NotObject(
            other.type_name().to_string(),
        )),
    }
}

/// `core/tool.py::_deep_merge_params` -- deep-merges *overlay* onto
/// *base*; *overlay* keys win, nested dicts recurse (only when *both*
/// sides are dicts at that key; anything else is a plain overwrite). An
/// overlay key new to *base* is appended, same as a plain
/// `dict.__setitem__` would; an existing key keeps its own position
/// (`IndexMap::insert` on an already-present key updates the value
/// in place, same as Python's `dict`).
pub fn deep_merge_params(mut base: Dict, overlay: Dict) -> Dict {
    for (key, mut value) in overlay {
        let existing_dict = match base.get(&key) {
            Some(Value::Dict(d)) => Some(d.clone()),
            _ => None,
        };
        // Same `Drop`-vs-partial-move fix as `render_params_json` above:
        // match on `&mut value` and `mem::take` the nested dict out,
        // rather than destructuring `value` itself by move.
        if let (Some(existing), Value::Dict(incoming)) = (existing_dict, &mut value) {
            let merged = deep_merge_params(existing, std::mem::take(incoming));
            base.insert(key, Value::Dict(merged));
        } else {
            base.insert(key, value);
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_bytecode::{Escape, TemplateText};

    fn ctx_from(pairs: Vec<(&str, Value)>) -> Value {
        let mut dict = Dict::new();
        for (k, v) in pairs {
            dict.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(dict)
    }

    #[test]
    fn a_literal_passes_through_unrendered() {
        let mut params = IndexMap::new();
        params.insert(
            Value::Str("count".to_string()),
            ParamNode::Literal(Value::from(3i64)),
        );
        let rendered = render_params("fetch", &params, &Value::None).unwrap();
        assert_eq!(
            rendered.get(&Value::Str("count".to_string())),
            Some(&Value::from(3i64))
        );
    }

    #[test]
    fn a_template_leaf_renders_mustache() {
        let mut params = IndexMap::new();
        params.insert(
            Value::Str("greeting".to_string()),
            ParamNode::Template(TemplateText::new("hi {{input.name}}", true, Escape::Html)),
        );
        let ctx = ctx_from(vec![(
            "input",
            ctx_from(vec![("name", Value::from("world"))]),
        )]);
        let rendered = render_params("fetch", &params, &ctx).unwrap();
        assert_eq!(
            rendered.get(&Value::Str("greeting".to_string())),
            Some(&Value::from("hi world"))
        );
    }

    #[test]
    fn a_from_reference_resolves_typed_and_unrendered() {
        let mut params = IndexMap::new();
        params.insert(
            Value::Str("n".to_string()),
            ParamNode::From {
                path: "input.n".to_string(),
                default: None,
            },
        );
        let ctx = ctx_from(vec![("input", ctx_from(vec![("n", Value::from(5i64))]))]);
        let rendered = render_params("fetch", &params, &ctx).unwrap();
        assert_eq!(
            rendered.get(&Value::Str("n".to_string())),
            Some(&Value::from(5i64))
        );
    }

    #[test]
    fn an_unresolved_reference_with_no_default_fails_with_the_exact_text() {
        let mut params = IndexMap::new();
        params.insert(
            Value::Str("n".to_string()),
            ParamNode::From {
                path: "input.missing".to_string(),
                default: None,
            },
        );
        let ctx = ctx_from(vec![("input", ctx_from(vec![]))]);
        let err = render_params("fetch", &params, &ctx).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Tool effect 'fetch' param 'params.n': '{from: input.missing}' did not resolve to a value."
        );
    }

    #[test]
    fn an_unresolved_reference_with_a_default_uses_it() {
        let mut params = IndexMap::new();
        params.insert(
            Value::Str("n".to_string()),
            ParamNode::From {
                path: "input.missing".to_string(),
                default: Some(Value::from(7i64)),
            },
        );
        let ctx = ctx_from(vec![("input", ctx_from(vec![]))]);
        let rendered = render_params("fetch", &params, &ctx).unwrap();
        assert_eq!(
            rendered.get(&Value::Str("n".to_string())),
            Some(&Value::from(7i64))
        );
    }

    #[test]
    fn a_from_reference_walks_a_list_by_negative_index() {
        let mut params = IndexMap::new();
        params.insert(
            Value::Str("last".to_string()),
            ParamNode::From {
                path: "input.items.-1".to_string(),
                default: None,
            },
        );
        let ctx = ctx_from(vec![(
            "input",
            ctx_from(vec![(
                "items",
                Value::List(vec![
                    Value::from(1i64),
                    Value::from(2i64),
                    Value::from(3i64),
                ]),
            )]),
        )]);
        let rendered = render_params("fetch", &params, &ctx).unwrap();
        assert_eq!(
            rendered.get(&Value::Str("last".to_string())),
            Some(&Value::from(3i64))
        );
    }

    #[test]
    fn a_non_string_key_survives_rendering_unstringified() {
        let mut inner = IndexMap::new();
        inner.insert(
            Value::Bool(true),
            ParamNode::Literal(Value::from("yes-value")),
        );
        let mut params = IndexMap::new();
        params.insert(Value::Str("input".to_string()), ParamNode::Map(inner));
        let rendered = render_params("fetch", &params, &Value::None).unwrap();
        let Some(Value::Dict(input)) = rendered.get(&Value::Str("input".to_string())) else {
            panic!("expected a rendered dict");
        };
        assert_eq!(
            input.get(&Value::Bool(true)),
            Some(&Value::from("yes-value"))
        );
    }

    #[test]
    fn params_json_renders_a_native_list_as_real_json_through_json_aware_ctx() {
        let ctx = ctx_from(vec![(
            "prime",
            ctx_from(vec![(
                "symbols",
                Value::List(vec![Value::from("AAPL"), Value::from("MSFT")]),
            )]),
        )]);
        let rendered = render_params_json("{\"tickers\": {{{prime.symbols}}}}", &ctx).unwrap();
        assert_eq!(
            rendered.get(&Value::Str("tickers".to_string())),
            Some(&Value::List(vec![Value::from("AAPL"), Value::from("MSFT")]))
        );
    }

    #[test]
    fn params_json_rejects_a_non_object_result() {
        let err = render_params_json("[1, 2]", &Value::None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "params_json must render to a JSON object (dict), got list"
        );
    }

    #[test]
    fn params_json_rejects_malformed_json_naming_the_failure() {
        let err = render_params_json("not json", &Value::None).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("params_json did not render to valid JSON:")
        );
    }

    #[test]
    fn deep_merge_overlays_a_nested_key_and_keeps_position() {
        let mut base = Dict::new();
        base.insert(Value::Str("a".to_string()), Value::from(1i64));
        let mut base_nested = Dict::new();
        base_nested.insert(Value::Str("x".to_string()), Value::from(1i64));
        base.insert(Value::Str("b".to_string()), Value::Dict(base_nested));

        let mut overlay_nested = Dict::new();
        overlay_nested.insert(Value::Str("y".to_string()), Value::from(2i64));
        let mut overlay = Dict::new();
        overlay.insert(Value::Str("b".to_string()), Value::Dict(overlay_nested));
        overlay.insert(Value::Str("c".to_string()), Value::from(3i64));

        let merged = deep_merge_params(base, overlay);
        let keys: Vec<&Value> = merged.keys().collect();
        assert_eq!(
            keys,
            vec![
                &Value::Str("a".to_string()),
                &Value::Str("b".to_string()),
                &Value::Str("c".to_string())
            ]
        );
        let Some(Value::Dict(merged_b)) = merged.get(&Value::Str("b".to_string())) else {
            panic!("expected a merged dict at 'b'");
        };
        assert_eq!(
            merged_b.get(&Value::Str("x".to_string())),
            Some(&Value::from(1i64))
        );
        assert_eq!(
            merged_b.get(&Value::Str("y".to_string())),
            Some(&Value::from(2i64))
        );
    }

    #[test]
    fn deep_merge_overwrites_a_non_dict_value_wholesale() {
        let mut base = Dict::new();
        base.insert(Value::Str("a".to_string()), Value::from(1i64));
        let mut overlay = Dict::new();
        overlay.insert(Value::Str("a".to_string()), Value::from(2i64));
        let merged = deep_merge_params(base, overlay);
        assert_eq!(
            merged.get(&Value::Str("a".to_string())),
            Some(&Value::from(2i64))
        );
    }
}
