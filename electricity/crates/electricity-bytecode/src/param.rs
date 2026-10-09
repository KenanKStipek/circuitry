//! Lane A: `ParamNode`, the IR for a tool's `params`/`params_json` and a
//! `use`'s `inputs` (issue #408's Scope section).

use crate::template::TemplateText;
use electricity_value::Value;
use indexmap::IndexMap;
use serde::ser::{Error as _, SerializeMap};
use serde::{Serialize, Serializer};

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
#[derive(Debug, Clone)]
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

/// Hand-written, not `#[derive(Serialize)]`: the default derive would
/// serialize [`ParamNode::Map`]'s `IndexMap<Value, ParamNode>` as a
/// serde map keyed by `Value` directly, and `serde_json`'s map-key
/// serializer rejects anything but a string/int/float/bool/char key --
/// `Value::None`/`Value::Bytes`/a `Value::List`/`Value::Dict` key would
/// make `--dump-ir` fail outright on a document a non-`--dump-ir` run
/// compiles and runs fine. Every variant below matches `#[derive(
/// Serialize)]`'s own externally-tagged shape (`{"Literal": ...}`,
/// `{"From": {"path": ..., "default": ...}}`, ...) byte for byte --
/// only [`ParamNode::Map`]'s inner map goes through [`electricity_json::
/// stringify_key`] first (Python's own `json.dumps` non-`str`-key rule:
/// `True`/`False` -> `"true"`/`"false"`, `None` -> `"null"`, a number ->
/// its decimal text), the same stringification the `json` tool's own
/// output already applies to a non-string `params`/`params_json` key
/// (issue #431's gate lane, item 3) -- so `--dump-ir` keeps the
/// `Value`-keyed map's shape readable as a JSON object instead of
/// failing on the one key shape JSON objects can't represent directly.
impl Serialize for ParamNode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            ParamNode::Literal(value) => {
                serializer.serialize_newtype_variant("ParamNode", 0, "Literal", value)
            }
            ParamNode::Template(text) => {
                serializer.serialize_newtype_variant("ParamNode", 1, "Template", text)
            }
            ParamNode::From { path, default } => {
                use serde::ser::SerializeStructVariant;
                let mut state = serializer.serialize_struct_variant("ParamNode", 2, "From", 2)?;
                state.serialize_field("path", path)?;
                state.serialize_field("default", default)?;
                state.end()
            }
            ParamNode::Map(entries) => serializer.serialize_newtype_variant(
                "ParamNode",
                3,
                "Map",
                &StringKeyedMap(entries),
            ),
            ParamNode::List(items) => {
                serializer.serialize_newtype_variant("ParamNode", 4, "List", items)
            }
        }
    }
}

/// [`ParamNode::Map`]'s own `Serialize`-only view: the same entries, in
/// the same order, with each key run through [`electricity_json::
/// stringify_key`] first.
struct StringKeyedMap<'a>(&'a IndexMap<Value, ParamNode>);

impl Serialize for StringKeyedMap<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
            let key_text = electricity_json::stringify_key(key).map_err(S::Error::custom)?;
            map.serialize_entry(&key_text, value)?;
        }
        map.end()
    }
}
