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

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Value;

    fn single_key_map(key: Value) -> ParamNode {
        let mut entries = IndexMap::new();
        entries.insert(key, ParamNode::Literal(Value::from(1i64)));
        ParamNode::Map(entries)
    }

    /// *node*'s own `Map` entry, serialized then re-parsed as a plain
    /// `serde_json::Map` -- the shape [`StringKeyedMap`] actually
    /// produces, keys included, not just "it didn't panic".
    fn serialized_map_keys(node: &ParamNode) -> Vec<String> {
        let json = serde_json::to_value(node).expect("serializes");
        json.get("Map")
            .and_then(|map| map.as_object())
            .expect("a Map variant with an object payload")
            .keys()
            .cloned()
            .collect()
    }

    #[test]
    fn a_null_key_serializes_as_the_literal_text_null() {
        assert_eq!(serialized_map_keys(&single_key_map(Value::None)), ["null"]);
    }

    #[test]
    fn a_bool_key_serializes_lowercase_not_pythons_capitalized_repr() {
        assert_eq!(
            serialized_map_keys(&single_key_map(Value::Bool(true))),
            ["true"]
        );
        assert_eq!(
            serialized_map_keys(&single_key_map(Value::Bool(false))),
            ["false"]
        );
    }

    #[test]
    fn a_nan_key_serializes_as_the_json_dumps_nan_token() {
        assert_eq!(
            serialized_map_keys(&single_key_map(Value::Float(f64::NAN))),
            ["NaN"]
        );
    }

    #[test]
    fn a_bytes_key_fails_to_serialize_matching_json_dumps_own_typeerror() {
        // `json.dumps` raises `TypeError` for a `bytes` dict key -- this
        // crate's own `stringify_key` does the same (`WriteError::
        // KeyNotStrIntFloatBoolNone`), so `--dump-ir` fails the same way
        // on a document a non-`--dump-ir` run would also refuse to
        // serialize through the `json` tool, rather than crashing or
        // silently dropping the key.
        let node = single_key_map(Value::Bytes(vec![1, 2, 3]));
        assert!(serde_json::to_value(&node).is_err());
    }
}
