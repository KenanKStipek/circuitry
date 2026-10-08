//! Lane A: `ParamNode`, the IR for a tool's `params`/`params_json` and a
//! `use`'s `inputs` (issue #408's Scope section).

use crate::template::TemplateText;
use electricity_value::Value;
use indexmap::IndexMap;
use serde::Serialize;

/// One node of a compiled `params`/`inputs` tree.
///
/// A raw YAML/JSON mapping or list under `params:`/`inputs:` is walked at
/// compile time into this shape: a literal value stays a literal, a
/// string containing `{{ }}` becomes a `Template`, a `{from: ...}`
/// mapping becomes `From`, and a nested mapping/list becomes `Map`/`List`
/// of the same walk applied to its children — matching
/// `core/templates.py`'s own recursive params-walk (lane C ports the
/// walk itself; this lane only fixes its output shape).
#[derive(Debug, Clone, Serialize)]
pub enum ParamNode {
    Literal(Value),
    Template(TemplateText),
    From {
        path: String,
        default: Option<Value>,
    },
    Map(IndexMap<String, ParamNode>),
    List(Vec<ParamNode>),
}
