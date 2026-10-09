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
///
/// `Map` is keyed by [`Value`], not `String`: a YAML/JSON mapping key
/// need not be a string (`yes:`/`1:` parse as `Value::Bool`/`Value::Int`,
/// same as any other mapping Circuitry reads), and Python keeps that key
/// as-is in `ToolDefinition.params`/`UseDefinition.inputs` -- it is never
/// stringified. A compiler that stringified a non-string key here would
/// collapse two distinct keys that stringify alike (`1` and `"1"`) into
/// one, and would render a bool key as `"True"`/`"False"` in the `json`
/// tool's output where Python's own `json.dumps` writes `"true"`/
/// `"false"` (`electricity_json::stringify_key`, the key-stringification
/// rule a renderer must use instead of `Value::py_str` when it finally
/// turns this tree into JSON).
#[derive(Debug, Clone, Serialize)]
pub enum ParamNode {
    Literal(Value),
    Template(TemplateText),
    From {
        path: String,
        default: Option<Value>,
    },
    Map(IndexMap<Value, ParamNode>),
    List(Vec<ParamNode>),
}
