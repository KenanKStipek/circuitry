//! Lane C: tool params rendering -- `ParamNode` -> `Value`, the
//! `{from: ...}` walk (with `default:`), Mustache leaves, and a tool's
//! `params_json` overlay (`core/tool.py::_render_params`/
//! `_render_params_json`/`_deep_merge_params`, ported exactly). Every
//! Mustache leaf renders through [`crate::compose::render_with_composition`]
//! rather than calling `render_template` directly (PR #454): a
//! document containing a `{{> name}}` partial is refused
//! before a run starts, so that function's own pass-through path is
//! this file's whole behaviour today, and lane P's own landing never
//! has to add *declared_prompts*/*effect_names* to this file's own
//! signatures -- they are already threaded through from
//! `RunContext`/`execute_tool`. A `use` effect's own `inputs:` follow a
//! different contract (`core/use.py::_render_inputs` -- no `default:`,
//! and a `{from:}` that resolves to `None` passes through as `None`
//! rather than raising) and isn't rendered here.
//!
//! `exec::tool::execute_tool` (this crate's own lane C stub, filled in
//! once lane B's `Store` lands) is the only caller: it renders a tool's
//! `params:` through [`render_params`], its `params_json:` (if any)
//! through [`render_params_json`], and deep-merges the two through
//! [`deep_merge_params`] -- the `params_json` override takes precedence,
//! same as `core/tool.py`.

use electricity_bytecode::{Escape, ParamNode};
use electricity_json::{Separators, WriteMode};
use electricity_template::{JsonAwareCtx, PlainCtx, ScopeRoot};
use electricity_value::{Dict, Value};
use indexmap::IndexMap;
use std::collections::BTreeSet;
use std::fmt;

/// What [`render_params`]/[`render_params_json`] render a tool's
/// `params:`/`params_json:` against: [`ScopeRoot`]'s own Mustache
/// lookup for a [`ParamNode::Template`] leaf (through
/// [`crate::compose::render_with_composition`]), plus [`resolve_from`]
/// for a [`ParamNode::From`] leaf's own `{from: ...}` walk -- a plain
/// nested-dict/list path lookup with none of Mustache's scope-chain
/// fallback or int/str/bytes-index rules, so it can't be expressed
/// through [`ScopeRoot::lookup`] alone (see [`resolve_from`]'s own doc
/// comment). [`Value`]'s own impl is exactly today's behaviour,
/// unchanged; a live, store-backed impl (issue #463) lives in
/// `exec::live_scope`, which alone knows what that store looks like.
pub trait ParamsCtx: ScopeRoot {
    /// `core/use.py::_resolve_reference`'s own walk, by *this* context
    /// rather than an already-materialized `Value` -- see this trait's
    /// own doc comment for why it's a separate method from
    /// [`ScopeRoot::lookup`].
    fn resolve_from(&self, path: &str) -> Option<Value>;
}

impl ParamsCtx for Value {
    fn resolve_from(&self, path: &str) -> Option<Value> {
        resolve_reference(self, path)
    }
}

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
    /// `{from: ..., as: json}` whose value JSON can't carry
    /// (`core/use.py::serialize_reference`'s own failure).
    NotSerializable {
        label: String,
        reference_path: String,
        reason: String,
    },
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
            RenderParamsError::NotSerializable {
                label,
                reference_path,
                reason,
            } => write!(
                f,
                "{label}: '{{from: {reference_path}, as: json}}' did not serialize to JSON: {reason}"
            ),
        }
    }
}

impl std::error::Error for RenderParamsError {}

/// `core/use.py::serialize_reference` for `as: json`: *value* as compact
/// JSON text (`json.dumps(value, ensure_ascii=False, separators=(",", ":"),
/// allow_nan=False)`). `Err` carries the reason a value JSON can't carry
/// (`NaN`/`inf`, bytes, a date) fails.
pub fn serialize_as_json(value: &Value) -> Result<String, String> {
    if contains_non_finite_float(value) {
        return Err("Out of range float values are not JSON compliant".to_string());
    }
    electricity_json::dumps(
        value,
        WriteMode {
            indent: None,
            sort_keys: false,
            ensure_ascii: false,
            separators: Separators::Compact,
        },
    )
    .map_err(|err| err.to_string())
}

fn contains_non_finite_float(value: &Value) -> bool {
    match value {
        Value::Float(f) => !f.is_finite(),
        Value::List(items) => items.iter().any(contains_non_finite_float),
        Value::Dict(dict) => dict.values().any(contains_non_finite_float),
        _ => false,
    }
}

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
pub fn render_params<R: ParamsCtx>(
    effect_name: &str,
    params: &IndexMap<Value, ParamNode>,
    ctx: &R,
    declared_prompts: &IndexMap<String, String>,
    effect_names: &BTreeSet<String>,
) -> Result<Dict, RenderParamsError> {
    let mut rendered = IndexMap::with_capacity(params.len());
    for (key, node) in params {
        let path = format!("params.{}", key.py_str());
        let value = render_node(
            effect_name,
            node,
            ctx,
            &path,
            declared_prompts,
            effect_names,
        )?;
        rendered.insert(key.clone(), value);
    }
    Ok(rendered)
}

fn render_node<R: ParamsCtx>(
    effect_name: &str,
    node: &ParamNode,
    ctx: &R,
    path: &str,
    declared_prompts: &IndexMap<String, String>,
    effect_names: &BTreeSet<String>,
) -> Result<Value, RenderParamsError> {
    match node {
        ParamNode::Literal(value) => Ok(value.clone()),
        ParamNode::Template(text) => crate::compose::render_with_composition(
            &text.source,
            ctx,
            declared_prompts,
            effect_names,
            path,
            &PlainCtx,
            matches!(text.escape, Escape::Html),
        )
        .map(Value::Str)
        .map_err(RenderParamsError::Template),
        ParamNode::From {
            path: reference_path,
            default,
            as_json,
        } => {
            let resolved = match ctx.resolve_from(reference_path) {
                // `core/tool.py::_render_params`'s own `_render_value` checks
                // `if resolved is None:`, not `if ref is missing:` -- a path
                // that resolves *to* `None` counts as unresolved exactly like
                // one that doesn't resolve at all, so both fall through to
                // `default:` (or the same error) rather than rendering `null`.
                Some(value) if !matches!(value, Value::None) => value,
                _ => match default {
                    Some(value) => value.clone(),
                    None => {
                        return Err(RenderParamsError::UnresolvedReference {
                            effect_name: effect_name.to_string(),
                            path: path.to_string(),
                            reference_path: reference_path.clone(),
                        });
                    }
                },
            };
            if *as_json {
                serialize_as_json(&resolved)
                    .map(Value::Str)
                    .map_err(|reason| RenderParamsError::NotSerializable {
                        label: format!("Tool effect '{effect_name}' param '{path}'"),
                        reference_path: reference_path.clone(),
                        reason,
                    })
            } else {
                Ok(resolved)
            }
        }
        ParamNode::Map(entries) => {
            let mut map = IndexMap::with_capacity(entries.len());
            for (key, child) in entries {
                let child_path = format!("{path}.{}", key.py_str());
                let value = render_node(
                    effect_name,
                    child,
                    ctx,
                    &child_path,
                    declared_prompts,
                    effect_names,
                )?;
                map.insert(key.clone(), value);
            }
            Ok(Value::Dict(map))
        }
        ParamNode::List(items) => {
            let mut list = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let child_path = format!("{path}[{index}]");
                list.push(render_node(
                    effect_name,
                    item,
                    ctx,
                    &child_path,
                    declared_prompts,
                    effect_names,
                )?);
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
    let mut current = ctx;
    for part in path.split('.') {
        current = match current {
            Value::Dict(dict) => dict.get(&Value::Str(part.to_string()))?,
            Value::List(items) => {
                let index: i64 = part.parse().ok()?;
                let resolved = python_list_index(items.len(), index)?;
                &items[resolved]
            }
            _ => return None,
        };
    }
    Some(current.clone())
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
            separators: Separators::Default,
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
pub fn render_params_json<R: ParamsCtx>(
    template: &str,
    ctx: &R,
    declared_prompts: &IndexMap<String, String>,
    effect_names: &BTreeSet<String>,
) -> Result<Dict, RenderParamsJsonError> {
    let serializer: &dyn Fn(&Value) -> Result<String, String> = &json_aware_splice;
    let json_ctx = JsonAwareCtx::new(serializer);
    let rendered_text = crate::compose::render_with_composition(
        template,
        ctx,
        declared_prompts,
        effect_names,
        "params_json",
        &json_ctx,
        true,
    )
    .map_err(RenderParamsJsonError::Template)?;
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

    /// Every render site needs an environment argument
    /// ([`render_with_composition`]'s own *declared_prompts*/
    /// *effect_names*) now even though nothing in this module's own
    /// tests exercises composition itself (review finding 1 on PR
    /// #454) -- empty is the correct value for every document with no
    /// `prompts:` map, same as [`crate::compose`]'s own tests use.
    fn empty_env() -> (IndexMap<String, String>, BTreeSet<String>) {
        (IndexMap::new(), BTreeSet::new())
    }

    #[test]
    fn a_literal_passes_through_unrendered() {
        let mut params = IndexMap::new();
        params.insert(
            Value::Str("count".to_string()),
            ParamNode::Literal(Value::from(3i64)),
        );
        let (declared_prompts, effect_names) = empty_env();
        let rendered = render_params(
            "fetch",
            &params,
            &Value::None,
            &declared_prompts,
            &effect_names,
        )
        .unwrap();
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
        let (declared_prompts, effect_names) = empty_env();
        let rendered =
            render_params("fetch", &params, &ctx, &declared_prompts, &effect_names).unwrap();
        assert_eq!(
            rendered.get(&Value::Str("greeting".to_string())),
            Some(&Value::from("hi world"))
        );
    }

    #[test]
    fn a_partial_leaf_runs_through_composition_not_templates_own_rejection() {
        // Review finding 1's own probe on PR #454: `render_node` always
        // renders a template leaf through `render_with_composition`,
        // never `render_template` directly, so a `{{> name}}` leaf
        // reaches real `{{> name}}` expansion (M1-P, #487) rather than
        // `electricity-template`'s own unconditional partial rejection
        // -- the walker keeps a document with one refused before a
        // real run even starts; this file's own render path has never
        // needed to special-case it.
        let mut params = IndexMap::new();
        params.insert(
            Value::Str("greeting".to_string()),
            ParamNode::Template(TemplateText::new("{{> greeting}}", true, Escape::Html)),
        );
        let (declared_prompts, effect_names) = empty_env();
        let err = render_params(
            "fetch",
            &params,
            &Value::None,
            &declared_prompts,
            &effect_names,
        )
        .unwrap_err();
        let message = err.to_string();
        assert_eq!(
            message,
            "params.greeting: '{{> greeting}}' does not name a declared prompt or effect."
        );
        assert!(
            !message.contains("partials are not supported"),
            "a partial leaf must reach render_with_composition's own \
             expansion, not electricity-template's partial rejection: {message}"
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
                as_json: false,
            },
        );
        let ctx = ctx_from(vec![("input", ctx_from(vec![("n", Value::from(5i64))]))]);
        let (declared_prompts, effect_names) = empty_env();
        let rendered =
            render_params("fetch", &params, &ctx, &declared_prompts, &effect_names).unwrap();
        assert_eq!(
            rendered.get(&Value::Str("n".to_string())),
            Some(&Value::from(5i64))
        );
    }

    #[test]
    fn a_from_reference_resolves_through_a_list_index() {
        let mut params = IndexMap::new();
        params.insert(
            Value::Str("n".to_string()),
            ParamNode::From {
                path: "input.items.1".to_string(),
                default: None,
                as_json: false,
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
        let (declared_prompts, effect_names) = empty_env();
        let rendered =
            render_params("fetch", &params, &ctx, &declared_prompts, &effect_names).unwrap();
        assert_eq!(
            rendered.get(&Value::Str("n".to_string())),
            Some(&Value::from(2i64))
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
                as_json: false,
            },
        );
        let ctx = ctx_from(vec![("input", ctx_from(vec![]))]);
        let (declared_prompts, effect_names) = empty_env();
        let err =
            render_params("fetch", &params, &ctx, &declared_prompts, &effect_names).unwrap_err();
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
                as_json: false,
            },
        );
        let ctx = ctx_from(vec![("input", ctx_from(vec![]))]);
        let (declared_prompts, effect_names) = empty_env();
        let rendered =
            render_params("fetch", &params, &ctx, &declared_prompts, &effect_names).unwrap();
        assert_eq!(
            rendered.get(&Value::Str("n".to_string())),
            Some(&Value::from(7i64))
        );
    }

    #[test]
    fn a_from_reference_resolving_to_null_falls_back_to_default() {
        // Matches `core/tool.py::_render_params`'s `if resolved is None:`
        // probe: `_render_params({"n": {"from": "input.x", "default": 5}},
        // {"input": {"x": None}}, name="t")` returns `{"n": 5}`.
        let mut params = IndexMap::new();
        params.insert(
            Value::Str("n".to_string()),
            ParamNode::From {
                path: "input.x".to_string(),
                default: Some(Value::from(5i64)),
                as_json: false,
            },
        );
        let ctx = ctx_from(vec![("input", ctx_from(vec![("x", Value::None)]))]);
        let (declared_prompts, effect_names) = empty_env();
        let rendered = render_params("t", &params, &ctx, &declared_prompts, &effect_names).unwrap();
        assert_eq!(
            rendered.get(&Value::Str("n".to_string())),
            Some(&Value::from(5i64))
        );
    }

    #[test]
    fn a_from_reference_resolving_to_null_with_no_default_raises() {
        let mut params = IndexMap::new();
        params.insert(
            Value::Str("n".to_string()),
            ParamNode::From {
                path: "input.x".to_string(),
                default: None,
                as_json: false,
            },
        );
        let ctx = ctx_from(vec![("input", ctx_from(vec![("x", Value::None)]))]);
        let (declared_prompts, effect_names) = empty_env();
        let err = render_params("t", &params, &ctx, &declared_prompts, &effect_names).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Tool effect 't' param 'params.n': '{from: input.x}' did not resolve to a value."
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
                as_json: false,
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
        let (declared_prompts, effect_names) = empty_env();
        let rendered =
            render_params("fetch", &params, &ctx, &declared_prompts, &effect_names).unwrap();
        assert_eq!(
            rendered.get(&Value::Str("last".to_string())),
            Some(&Value::from(3i64))
        );
    }

    fn as_json_leaf(path: &str, default: Option<Value>) -> ParamNode {
        ParamNode::From {
            path: path.to_string(),
            default,
            as_json: true,
        }
    }

    fn render_one(node: ParamNode, ctx: &Value) -> Result<Value, RenderParamsError> {
        let mut params = IndexMap::new();
        params.insert(Value::Str("v".to_string()), node);
        let (declared_prompts, effect_names) = empty_env();
        render_params("call", &params, ctx, &declared_prompts, &effect_names)
            .map(|mut rendered| rendered.shift_remove(&Value::Str("v".to_string())).unwrap())
    }

    #[test]
    fn an_as_json_reference_is_compact_json_text_in_key_order() {
        let mut meta = Dict::new();
        meta.insert(Value::Str("zeta".to_string()), Value::from(1i64));
        meta.insert(
            Value::Str("alpha".to_string()),
            Value::List(vec![Value::from(1i64), Value::None]),
        );
        meta.insert(Value::Str("name".to_string()), Value::from("caf\u{e9}"));
        let ctx = ctx_from(vec![("input", ctx_from(vec![("meta", Value::Dict(meta))]))]);
        let rendered = render_one(as_json_leaf("input.meta", None), &ctx).unwrap();
        assert_eq!(
            rendered,
            Value::from("{\"zeta\":1,\"alpha\":[1,null],\"name\":\"caf\u{e9}\"}")
        );
    }

    #[test]
    fn an_as_json_reference_applies_the_default_before_serializing() {
        let ctx = ctx_from(vec![("input", ctx_from(vec![]))]);
        let rendered = render_one(
            as_json_leaf("input.missing", Some(Value::Dict(Dict::new()))),
            &ctx,
        )
        .unwrap();
        assert_eq!(rendered, Value::from("{}"));
    }

    #[test]
    fn an_as_json_reference_inside_a_list_serializes_only_that_item() {
        let mut params = IndexMap::new();
        params.insert(
            Value::Str("args".to_string()),
            ParamNode::List(vec![
                ParamNode::Literal(Value::from("record")),
                as_json_leaf("input.meta", None),
            ]),
        );
        let ctx = ctx_from(vec![(
            "input",
            ctx_from(vec![("meta", Value::List(vec![Value::from(1i64)]))]),
        )]);
        let (declared_prompts, effect_names) = empty_env();
        let rendered =
            render_params("call", &params, &ctx, &declared_prompts, &effect_names).unwrap();
        assert_eq!(
            rendered.get(&Value::Str("args".to_string())),
            Some(&Value::List(vec![
                Value::from("record"),
                Value::from("[1]")
            ]))
        );
    }

    #[test]
    fn an_as_json_reference_without_a_default_still_fails_when_unresolved() {
        let ctx = ctx_from(vec![("input", ctx_from(vec![]))]);
        let err = render_one(as_json_leaf("input.missing", None), &ctx).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Tool effect 'call' param 'params.v': '{from: input.missing}' did not resolve to a value."
        );
    }

    #[test]
    fn an_as_json_reference_to_an_unserializable_value_names_the_path() {
        for (value, reason) in [
            (
                Value::Float(f64::NAN),
                "Out of range float values are not JSON compliant",
            ),
            (
                Value::List(vec![Value::Float(f64::INFINITY)]),
                "Out of range float values are not JSON compliant",
            ),
            (
                Value::Bytes(vec![1, 2]),
                "Object of type bytes is not JSON serializable",
            ),
        ] {
            let ctx = ctx_from(vec![("input", ctx_from(vec![("x", value)]))]);
            let err = render_one(as_json_leaf("input.x", None), &ctx).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    "Tool effect 'call' param 'params.v': '{{from: input.x, as: json}}' \
                     did not serialize to JSON: {reason}"
                )
            );
        }
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
        let (declared_prompts, effect_names) = empty_env();
        let rendered = render_params(
            "fetch",
            &params,
            &Value::None,
            &declared_prompts,
            &effect_names,
        )
        .unwrap();
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
        let (declared_prompts, effect_names) = empty_env();
        let rendered = render_params_json(
            "{\"tickers\": {{{prime.symbols}}}}",
            &ctx,
            &declared_prompts,
            &effect_names,
        )
        .unwrap();
        assert_eq!(
            rendered.get(&Value::Str("tickers".to_string())),
            Some(&Value::List(vec![Value::from("AAPL"), Value::from("MSFT")]))
        );
    }

    #[test]
    fn params_json_escapes_an_html_special_character_value() {
        // #487's own settled question: `_render_params_json` calls
        // `render_with_composition` with no `escape=` override, so its
        // default `True` applies -- `params_json`'s own `TemplateText`
        // is `Escape::Html`. A raw `&`/`<`/`>`/`"` value is HTML-escaped
        // before the JSON parse, same as Python; the parsed value keeps
        // the escaped text literally (JSON never re-decodes it).
        let ctx = ctx_from(vec![(
            "input",
            ctx_from(vec![("x", Value::from("<b>&\""))]),
        )]);
        let (declared_prompts, effect_names) = empty_env();
        let rendered = render_params_json(
            "{\"x\": \"{{input.x}}\"}",
            &ctx,
            &declared_prompts,
            &effect_names,
        )
        .unwrap();
        assert_eq!(
            rendered.get(&Value::Str("x".to_string())),
            Some(&Value::from("&lt;b&gt;&amp;&quot;"))
        );
    }

    #[test]
    fn params_json_rejects_a_non_object_result() {
        let (declared_prompts, effect_names) = empty_env();
        let err = render_params_json("[1, 2]", &Value::None, &declared_prompts, &effect_names)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "params_json must render to a JSON object (dict), got list"
        );
    }

    #[test]
    fn params_json_rejects_malformed_json_naming_the_failure() {
        let (declared_prompts, effect_names) = empty_env();
        let err = render_params_json("not json", &Value::None, &declared_prompts, &effect_names)
            .unwrap_err();
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
