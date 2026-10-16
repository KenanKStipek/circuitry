//! Disabled effects (`core/disabled.py`) -- issue #518 (lane J2).
//!
//! [`electricity_bytecode::Op::enabled`] is set `false` by a profile's
//! own per-effect `effects.<path>.enabled: false`
//! (`electricity_compiler::overrides::apply_effect_overrides`), never by
//! orchestration YAML itself -- a run with no `--profile` (or a profile
//! with no `effects:` entries) therefore has `op.enabled == true`
//! everywhere, and the hook in [`super::execute_op`] that calls
//! [`skip_disabled`] never takes this module's own branch.

use super::dynamic::now_iso;
use crate::{NodeRef, Store, VmError};
use electricity_bytecode::Op;
use electricity_value::{Dict, Value};

/// `core/disabled.py::write_disabled_node` -- writes *op*'s own skip
/// node under *parent* at `op.name` (always `Some`; [`super::execute_op`]
/// only calls this for a named op) and leaves the balanced
/// `effect_start`/`effect_complete`/`write()` hooks to its own caller,
/// in Python's order (write the node, then start, then complete, then
/// `write()`) -- see that function's own doc comment.
///
/// Replaces any pre-existing `value`/`meta` whole (a value restored
/// from persisted state did not run *this* run, so its node must not
/// advertise a stale one); every other key already on the node (none
/// yet, in this milestone, but a future persistence resume could leave
/// one) is untouched. A fresh node's own key order is `value`, then
/// `meta`, since [`Store::ensure_dict`] always creates an empty map and
/// [`Store::set_leaf`] inserts in call order.
pub fn write_disabled_node(store: &Store, parent: &NodeRef, op: &Op) -> Result<(), VmError> {
    let name = op
        .name
        .as_ref()
        .expect("write_disabled_node: only ever called for a named op (see execute_op)");
    let node = store
        .ensure_dict(parent, Value::Str(name.clone()))
        .map_err(super::store_err)?;
    store.set_leaf(&node, Value::Str("value".to_string()), Value::None);
    let now = Value::Str(now_iso());
    let mut meta = Dict::new();
    meta.insert(Value::Str("disabled".to_string()), Value::Bool(true));
    meta.insert(Value::Str("created_at".to_string()), now.clone());
    meta.insert(Value::Str("completed_at".to_string()), now);
    store.set_leaf(&node, Value::Str("meta".to_string()), Value::Dict(meta));
    Ok(())
}

/// Whether *op* (or an ancestor container) is disabled -- a disabled
/// container disables its whole subtree (`core/disabled.py::
/// _skip_disabled_effect`'s own container rule). *op.enabled* already
/// carries that: [`electricity_compiler::overrides::apply_effect_overrides`]
/// sets `enabled: false` on every descendant of a disabled container, so
/// this check never has to walk up to an ancestor itself.
pub fn skip_disabled(op: &Op, _ctx: &Value) -> bool {
    !op.enabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_from_entries;
    use electricity_bytecode::{EffectPath, LeafKind, NodeKind, OnError};
    use indexmap::IndexMap;

    fn disabled_tool_op(name: &str) -> Op {
        use electricity_bytecode::{ParamNode, ToolOp};
        Op {
            path: EffectPath::root().push_name(name),
            name: Some(name.to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
                provider: "json".to_string(),
                params: ParamNode::Map(IndexMap::new()),
                params_json: None,
                prompt: None,
                model: None,
                timeout_ms: None,
                retries: Default::default(),
                expect: None,
                description: None,
                group: None,
                prompt_file: None,
            }))),
            on_error: OnError::Fail,
            labels: None,
            enabled: false,
        }
    }

    #[test]
    fn skip_disabled_is_true_only_for_a_disabled_op() {
        let store = Store::new();
        let op = disabled_tool_op("t");
        assert!(skip_disabled(&op, &Value::None));
        let mut enabled_op = disabled_tool_op("t");
        enabled_op.enabled = true;
        assert!(!skip_disabled(&enabled_op, &Value::None));
        drop(store);
    }

    #[test]
    fn write_disabled_node_shape_and_key_order() {
        let store = Store::new();
        let parent = node_from_entries(IndexMap::new());
        let op = disabled_tool_op("t");
        write_disabled_node(&store, &parent, &op).unwrap();
        let node = match store.child(&parent, &Value::Str("t".to_string())) {
            Ok(Some(n)) => n,
            _ => panic!("expected a written node"),
        };
        let keys: Vec<Value> = node.borrow().keys().cloned().collect();
        assert_eq!(
            keys,
            vec![
                Value::Str("value".to_string()),
                Value::Str("meta".to_string())
            ]
        );
        let snapshot = store.snapshot(&node);
        let dict = snapshot.as_dict().unwrap();
        assert_eq!(
            dict.get(&Value::Str("value".to_string())),
            Some(&Value::None)
        );
        let meta = dict
            .get(&Value::Str("meta".to_string()))
            .unwrap()
            .as_dict()
            .unwrap();
        assert_eq!(
            meta.get(&Value::Str("disabled".to_string())),
            Some(&Value::Bool(true))
        );
        assert!(meta.contains_key(&Value::Str("created_at".to_string())));
        assert_eq!(
            meta.get(&Value::Str("created_at".to_string())),
            meta.get(&Value::Str("completed_at".to_string()))
        );
    }

    #[test]
    fn write_disabled_node_replaces_an_existing_meta_whole_but_keeps_other_keys() {
        let store = Store::new();
        let parent = node_from_entries(IndexMap::new());
        let node = store
            .ensure_dict(&parent, Value::Str("t".to_string()))
            .unwrap();
        store.set_leaf(&node, Value::Str("value".to_string()), Value::from(7_i64));
        store.set_leaf(
            &node,
            Value::Str("other".to_string()),
            Value::Str("kept".to_string()),
        );
        let mut stale_meta = Dict::new();
        stale_meta.insert(
            Value::Str("stale_key".to_string()),
            Value::Str("gone".to_string()),
        );
        store.set_leaf(
            &node,
            Value::Str("meta".to_string()),
            Value::Dict(stale_meta),
        );

        let op = disabled_tool_op("t");
        write_disabled_node(&store, &parent, &op).unwrap();

        let snapshot = store.snapshot(&node);
        let dict = snapshot.as_dict().unwrap();
        assert_eq!(
            dict.get(&Value::Str("value".to_string())),
            Some(&Value::None)
        );
        assert_eq!(
            dict.get(&Value::Str("other".to_string())),
            Some(&Value::Str("kept".to_string()))
        );
        let meta = dict
            .get(&Value::Str("meta".to_string()))
            .unwrap()
            .as_dict()
            .unwrap();
        assert!(!meta.contains_key(&Value::Str("stale_key".to_string())));
        assert_eq!(
            meta.get(&Value::Str("disabled".to_string())),
            Some(&Value::Bool(true))
        );
    }
}
