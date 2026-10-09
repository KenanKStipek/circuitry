//! `Store`/`NodeRef`: the VM's own mutable state tree (DESIGN.md's store
//! design; issue #431's Lane B section).
//!
//! A container child's identity-aliasing (`node["last"]` sharing the
//! same object as `node["iter_2"]`, `Rc::ptr_eq`-visible, DESIGN.md
//! §2.3) has to be representable directly, so a plain `IndexMap<Value,
//! Value>` -- where `Value` has no variant of its own for "this is the
//! same live node as that other key" -- can't be this type's storage.
//! Instead, every map entry is a [`Slot`]: either an owned leaf
//! [`Value`], or a further live [`NodeRef`] shared by every key that
//! resolves to it. Two different [`Store::ensure_dict`]/[`Store::child`]
//! calls for the same (parent, key) pair always return the identical
//! `Slot::Node`'s `NodeRef` again (an earlier revision of this file kept
//! that identity in a side table keyed by a parent's raw `Rc` address
//! instead, which could hand back a node belonging to an already-freed
//! parent once the allocator reused that address -- see issue #431's
//! review discussion). A [`Slot::Node`] is simply dropped, like any
//! other map value, when its key is overwritten or its owning node goes
//! away; nothing outside the map it lives in ever names it by address.

use electricity_value::Value;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

/// One entry of a [`NodeRef`]'s own map: either an owned leaf value, or a
/// further live, shared node. See this module's own doc comment.
#[derive(Debug, Clone)]
pub enum Slot {
    Value(Value),
    Node(NodeRef),
}

/// A single mutable state node -- `Rc<RefCell<IndexMap<Value, Slot>>>`.
/// Shared so that two different paths can reference the identical live
/// node (`Rc::ptr_eq`), not a deep copy of it.
pub type NodeRef = Rc<RefCell<IndexMap<Value, Slot>>>;

/// A fresh, empty node.
pub fn new_node() -> NodeRef {
    Rc::new(RefCell::new(IndexMap::new()))
}

/// [`Store`]'s own operations' error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError(pub String);

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for StoreError {}

/// The run's mutable state tree, rooted at `prime`.
pub struct Store {
    pub root: NodeRef,
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Store {
    pub fn new() -> Self {
        Store { root: new_node() }
    }

    /// Gets *parent*'s existing dict-valued child named *key*, or creates
    /// one -- overwriting a non-dict value at that key the way Python's
    /// own `node.setdefault(key, {})`-plus-type-coercion does
    /// (`core/store/store.py::Store.ensure_dict`). A key already holding
    /// a [`Slot::Node`] always returns that exact same `NodeRef` again --
    /// the Rust shape of Python's `node[key]` being the identical dict
    /// object on every lookup. A key holding a plain `Value::Dict` (data
    /// written as a whole leaf, e.g. a tool's own JSON output) is
    /// promoted into a tracked node in place, keeping its own contents
    /// and the key's insertion-order position -- Python descends into
    /// *any* dict, not only ones it created through `ensure_dict` itself.
    pub fn ensure_dict(&self, parent: &NodeRef, key: Value) -> Result<NodeRef, StoreError> {
        let mut parent_map = parent.borrow_mut();
        if let Some(Slot::Node(existing)) = parent_map.get(&key) {
            return Ok(Rc::clone(existing));
        }
        let initial = match parent_map.get_mut(&key) {
            Some(Slot::Value(Value::Dict(existing))) => std::mem::take(existing),
            _ => IndexMap::new(),
        };
        let converted: IndexMap<Value, Slot> = initial
            .into_iter()
            .map(|(k, v)| (k, Slot::Value(v)))
            .collect();
        let child = Rc::new(RefCell::new(converted));
        // `insert` on an already-present key overwrites the value
        // without moving the key's position (`IndexMap`'s own contract),
        // and inserts at the end for a key that wasn't there yet -- the
        // same either way for a brand new node or a promoted dict.
        parent_map.insert(key, Slot::Node(Rc::clone(&child)));
        Ok(child)
    }

    /// *parent*'s existing child named *key*, if any -- never creates one
    /// (unlike [`Store::ensure_dict`]). A plain `Value::Dict` leaf that
    /// isn't yet a tracked [`Slot::Node`] (data merged in from a finished
    /// parallel branch, say -- see [`Store::merge`]) is promoted on this
    /// call, exactly as `ensure_dict` would, so a later call for the same
    /// key returns that identical `NodeRef`.
    pub fn child(&self, parent: &NodeRef, key: &Value) -> Result<Option<NodeRef>, StoreError> {
        let is_dict_value = {
            let parent_map = parent.borrow();
            match parent_map.get(key) {
                Some(Slot::Node(existing)) => return Ok(Some(Rc::clone(existing))),
                Some(Slot::Value(Value::Dict(_))) => true,
                _ => false,
            }
        };
        if !is_dict_value {
            return Ok(None);
        }
        Ok(Some(self.ensure_dict(parent, key.clone())?))
    }

    /// Writes *value* at *key* under *parent* -- the Rust shape of the
    /// last segment of Python's own `parent[key] = value`
    /// (`core/store/store.py::Store.set`, after `ensure_dict` has already
    /// walked every ancestor segment -- that part is the caller's own
    /// job, one `ensure_dict` call per segment, exactly like `child`).
    /// Overwrites a plain value or a previously tracked [`Slot::Node`]
    /// alike: whatever [`Slot`] was at this key (including a shared node
    /// another key might still be aliased to) is simply dropped and
    /// replaced, exactly like any other `IndexMap` write -- there is no
    /// separate registry entry to go stale, unlike an address-keyed side
    /// table would leave behind.
    pub fn set_leaf(&self, parent: &NodeRef, key: Value, value: Value) {
        parent.borrow_mut().insert(key, Slot::Value(value));
    }

    /// Makes *alias_key* resolve to the exact same live node that
    /// *target_key* already does under *parent* -- the Rust shape of
    /// Python's `node["last"] = node["iter_3"]` (a loop's own
    /// `last`-tracking, `core/loop.py::LoopRuntime._link_last`,
    /// DESIGN.md §6.7). *target_key* must already be a tracked child
    /// (via a prior `ensure_dict`/`child`/`alias` call); this is the one
    /// way this crate represents that two keys are the *same* node, since
    /// `Value` has no variant of its own for it (this module's own doc
    /// comment) -- [`Store::saved`] is what later recognizes the alias
    /// (by `Rc::ptr_eq`) and compacts it to `{"$ref": "iter_3"}` for the
    /// saved form. Out of scope for the M0-H interpreter (`loop` isn't
    /// implemented yet), but exercised directly by this crate's own
    /// tests so it's ready once a later milestone's loop body calls it.
    pub fn alias(
        &self,
        parent: &NodeRef,
        alias_key: Value,
        target_key: &Value,
    ) -> Result<NodeRef, StoreError> {
        let target = self.child(parent, target_key)?.ok_or_else(|| {
            StoreError(format!(
                "electricity_vm::Store::alias: no such key {} to alias to",
                target_key.py_repr()
            ))
        })?;
        parent
            .borrow_mut()
            .insert(alias_key, Slot::Node(Rc::clone(&target)));
        Ok(target)
    }

    /// *count* isolated branch stores for a tree `dynamic`/`each` loop --
    /// each starting **empty**, exactly as `core/store/store.py::
    /// Store.parallel_branches` does (`state={}`, not a copy of *parent*):
    /// a branch never sees *parent*'s existing keys, and concurrent
    /// branches never see each other's writes either, until
    /// [`Store::merge`] folds them back in.
    pub fn parallel_branches(
        &self,
        parent: &NodeRef,
        count: usize,
    ) -> Result<Vec<NodeRef>, StoreError> {
        let _ = parent;
        Ok((0..count).map(|_| new_node()).collect())
    }

    /// Merges *branches* back into *parent*, in index order
    /// (`core/store/store.py`'s own tree-merge rule: `for key, value in
    /// isolated_store.state.items(): node[key] = value`, `core/dynamic.
    /// py:543-545`/`core/loop.py:709-711`). Each branch's own top-level
    /// [`Slot`]s move into *parent* directly, keeping identity -- a
    /// branch's own container child (its own [`Slot::Node`]) becomes
    /// *parent*'s child by the same live `NodeRef`, not a flattened copy,
    /// so an alias a branch created survives the merge as the same alias,
    /// not a literal `{"$ref": ...}` dict (an earlier revision of this
    /// method materialized each branch first, which lost exactly that).
    ///
    /// Key order follows `dict.update`'s own rule, which `IndexMap::
    /// insert` already implements: a key *parent* (or an earlier branch)
    /// already has keeps its original position and only its slot is
    /// overwritten; a key no earlier source had is appended at the
    /// position its first-introducing branch reaches it. On a collision
    /// between branches, the **later** (higher-index) branch's slot wins
    /// -- branches are applied to *parent* in ascending index order, so
    /// each later merge overwrites whatever an earlier one wrote.
    pub fn merge(&self, parent: &NodeRef, branches: Vec<NodeRef>) -> Result<(), StoreError> {
        for branch in branches {
            let entries = std::mem::take(&mut *branch.borrow_mut());
            for (key, slot) in entries {
                parent.borrow_mut().insert(key, slot);
            }
        }
        Ok(())
    }

    /// An owned [`Value`] snapshot of *node* -- recursively resolving
    /// every [`Slot::Node`] child in place of the live node it names, so
    /// the result reflects every live write regardless of how deep it
    /// happened, with **no** alias compaction: a `last` that is
    /// [`Store::alias`]ed to a sibling comes back as that sibling's own
    /// full content, not `{"$ref": ...}`. This is what a template/CEL
    /// evaluation's own `ctx` is built from (DESIGN.md §6.7's own
    /// `scope_ctx`/`local_writes` overlay rule on top of this snapshot is
    /// lane B2's own interpreter's job, not this crate's) -- Circuitry's
    /// templates and CEL expressions read the *live* dicts, never a
    /// compacted saved form (`docs/orchestration-reference.md:545`,
    /// `core/saved_state.py`/`cli/runtime_shim.py`/`mcp/server.py` only
    /// ever call `compact_last_aliases` -- [`Store::saved`], below -- on
    /// a value about to be serialized, never on a live ctx).
    pub fn snapshot(&self, node: &NodeRef) -> Value {
        let map = node.borrow();
        let mut result = IndexMap::with_capacity(map.len());
        for (key, slot) in map.iter() {
            let value = match slot {
                Slot::Value(v) => v.clone(),
                Slot::Node(child) => self.snapshot(child),
            };
            result.insert(key.clone(), value);
        }
        Value::Dict(result)
    }

    /// *node*'s saved form -- the one `--out`/`--live-state` actually
    /// write, exactly as `core/saved_state.py::compact_last_aliases`
    /// builds it: a `last` key whose [`Slot::Node`] is `Rc::ptr_eq` to
    /// one of its own `iter_<N>` siblings' own `Slot::Node` is replaced
    /// by `{"$ref": "iter_<N>"}`; everything else -- including a `last`
    /// that holds its own independent content, or one that was never a
    /// tracked node at all -- is kept exactly as [`Store::snapshot`]
    /// would render it. Alias detection is by identity, the same as
    /// Python's own `child is last` check, never by key name alone.
    ///
    /// When more than one `iter_<N>` sibling happens to be `Rc::ptr_eq`
    /// to `last` (only possible if something else already aliased one
    /// `iter_<N>` to another), the sibling closest to the end wins --
    /// `core/saved_state.py::_aliased_iter_key`'s own reverse scan.
    ///
    /// Python's own function also recurses into plain lists and
    /// dict-valued leaves, since every Python dict is a potential alias
    /// target. This crate's own [`Slot::Value`] can never hold a "this
    /// is the same live node as..." reference of its own -- only a
    /// [`Slot::Node`] in a parent's own map can be `Rc::ptr_eq` to
    /// another -- so a plain value nested inside a list, or inside a
    /// dict that was never promoted to a [`Slot::Node`], can never be an
    /// alias target or source; recursing into it for compaction would
    /// only ever find nothing, so this method only ever recurses into a
    /// [`Slot::Node`] child, which already reaches every alias that could
    /// possibly exist.
    pub fn saved(&self, node: &NodeRef) -> Value {
        let map = node.borrow();
        let last_key = Value::Str("last".to_string());
        let last_alias_target: Option<String> = match map.get(&last_key) {
            Some(Slot::Node(last_node)) => map.iter().rev().find_map(|(key, slot)| {
                let Value::Str(name) = key else { return None };
                if key == &last_key || !is_iter_key(key) {
                    return None;
                }
                let Slot::Node(sibling) = slot else {
                    return None;
                };
                if Rc::ptr_eq(last_node, sibling) {
                    Some(name.clone())
                } else {
                    None
                }
            }),
            _ => None,
        };
        let mut result = IndexMap::with_capacity(map.len());
        for (key, slot) in map.iter() {
            if key == &last_key {
                if let Some(target) = &last_alias_target {
                    let mut reference = IndexMap::with_capacity(1);
                    reference.insert(Value::Str("$ref".to_string()), Value::Str(target.clone()));
                    result.insert(key.clone(), Value::Dict(reference));
                    continue;
                }
            }
            let value = match slot {
                Slot::Value(v) => v.clone(),
                Slot::Node(child) => self.saved(child),
            };
            result.insert(key.clone(), value);
        }
        Value::Dict(result)
    }
}

/// `true` for a `Value::Str` matching `iter_<digits>` (`core/saved_state.
/// py`'s own `_ITER_KEY` pattern), the only sibling shape [`Store::saved`]
/// ever treats a `last` as aliasing.
fn is_iter_key(key: &Value) -> bool {
    let Value::Str(s) = key else {
        return false;
    };
    let Some(rest) = s.strip_prefix("iter_") else {
        return false;
    };
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> Value {
        Value::Str(s.to_string())
    }

    fn set(parent: &NodeRef, key: Value, value: Value) {
        parent.borrow_mut().insert(key, Slot::Value(value));
    }

    fn get_value(parent: &NodeRef, key: &Value) -> Option<Value> {
        match parent.borrow().get(key) {
            Some(Slot::Value(v)) => Some(v.clone()),
            _ => None,
        }
    }

    fn is_node(parent: &NodeRef, key: &Value) -> bool {
        matches!(parent.borrow().get(key), Some(Slot::Node(_)))
    }

    #[test]
    fn a_fresh_store_has_an_empty_root() {
        let store = Store::new();
        assert!(store.root.borrow().is_empty());
    }

    #[test]
    fn new_node_instances_are_each_their_own_node() {
        let a = new_node();
        let b = new_node();
        assert!(!Rc::ptr_eq(&a, &b));
    }

    #[test]
    fn ensure_dict_creates_an_empty_child_the_first_time() {
        let store = Store::new();
        let child = store.ensure_dict(&store.root, key("shots")).unwrap();
        assert!(child.borrow().is_empty());
        assert!(is_node(&store.root, &key("shots")));
    }

    #[test]
    fn ensure_dict_returns_the_same_node_on_a_second_call() {
        let store = Store::new();
        let first = store.ensure_dict(&store.root, key("shots")).unwrap();
        first
            .borrow_mut()
            .insert(key("x"), Slot::Value(Value::Int(1.into())));
        let second = store.ensure_dict(&store.root, key("shots")).unwrap();
        assert!(Rc::ptr_eq(&first, &second));
        assert_eq!(get_value(&second, &key("x")), Some(Value::Int(1.into())));
    }

    #[test]
    fn ensure_dict_coerces_a_non_dict_value_to_an_empty_dict() {
        let store = Store::new();
        set(&store.root, key("count"), Value::Int(5.into()));
        let child = store.ensure_dict(&store.root, key("count")).unwrap();
        assert!(child.borrow().is_empty());
        assert!(is_node(&store.root, &key("count")));
    }

    #[test]
    fn ensure_dict_preserves_an_existing_plain_dicts_own_contents() {
        let store = Store::new();
        let mut seed = IndexMap::new();
        seed.insert(key("a"), Value::Int(1.into()));
        set(&store.root, key("cfg"), Value::Dict(seed));
        let child = store.ensure_dict(&store.root, key("cfg")).unwrap();
        assert_eq!(get_value(&child, &key("a")), Some(Value::Int(1.into())));
    }

    #[test]
    fn ensure_dict_descends_through_a_nested_plain_dict_value() {
        // A whole tool-output-shaped value written once as a leaf, then
        // walked into by segment -- the way a caller handles any
        // multi-segment `set` path, including through data that was
        // never created via `ensure_dict` itself.
        let store = Store::new();
        let mut inner = IndexMap::new();
        inner.insert(key("x"), Value::Int(1.into()));
        let mut outer = IndexMap::new();
        outer.insert(key("nested"), Value::Dict(inner));
        set(&store.root, key("tool_output"), Value::Dict(outer));
        let tool_output = store.ensure_dict(&store.root, key("tool_output")).unwrap();
        let nested = store.ensure_dict(&tool_output, key("nested")).unwrap();
        assert_eq!(get_value(&nested, &key("x")), Some(Value::Int(1.into())));
        store.set_leaf(&nested, key("y"), Value::Int(2.into()));
        let snapshot = store.snapshot(&store.root);
        let Value::Dict(ref root) = snapshot else {
            panic!("expected a dict");
        };
        let Some(Value::Dict(tool_output)) = root.get(&key("tool_output")) else {
            panic!("expected tool_output to be a dict");
        };
        let Some(Value::Dict(nested)) = tool_output.get(&key("nested")) else {
            panic!("expected nested to be a dict");
        };
        assert_eq!(nested.get(&key("x")), Some(&Value::Int(1.into())));
        assert_eq!(nested.get(&key("y")), Some(&Value::Int(2.into())));
    }

    #[test]
    fn child_never_creates_one() {
        let store = Store::new();
        assert!(store.child(&store.root, &key("missing")).unwrap().is_none());
        assert!(store.root.borrow().is_empty());
    }

    #[test]
    fn child_finds_an_existing_tracked_node() {
        let store = Store::new();
        let created = store.ensure_dict(&store.root, key("shots")).unwrap();
        let found = store.child(&store.root, &key("shots")).unwrap().unwrap();
        assert!(Rc::ptr_eq(&created, &found));
    }

    #[test]
    fn child_promotes_an_untracked_dict_value() {
        let store = Store::new();
        let mut seed = IndexMap::new();
        seed.insert(key("a"), Value::Int(1.into()));
        set(&store.root, key("cfg"), Value::Dict(seed));
        let found = store.child(&store.root, &key("cfg")).unwrap().unwrap();
        assert_eq!(get_value(&found, &key("a")), Some(Value::Int(1.into())));
        let found_again = store.child(&store.root, &key("cfg")).unwrap().unwrap();
        assert!(Rc::ptr_eq(&found, &found_again));
    }

    #[test]
    fn child_reports_none_for_a_non_dict_value() {
        let store = Store::new();
        set(&store.root, key("count"), Value::Int(5.into()));
        assert!(store.child(&store.root, &key("count")).unwrap().is_none());
    }

    #[test]
    fn set_leaf_drops_a_previously_tracked_container_at_that_key() {
        let store = Store::new();
        let shots = store.ensure_dict(&store.root, key("shots")).unwrap();
        store.set_leaf(&shots, key("n"), Value::Int(1.into()));
        store.set_leaf(
            &store.root,
            key("shots"),
            Value::Str("now a string".to_string()),
        );
        assert_eq!(
            get_value(&store.root, &key("shots")),
            Some(Value::Str("now a string".to_string()))
        );
        // A fresh `ensure_dict` at the same key must never resurrect the
        // old node's content -- there is no side table for it to survive
        // in.
        let fresh = store.ensure_dict(&store.root, key("shots")).unwrap();
        assert!(fresh.borrow().is_empty());
    }

    #[test]
    fn alias_shares_the_same_node_as_its_target() {
        let store = Store::new();
        let iter_2 = store.ensure_dict(&store.root, key("iter_2")).unwrap();
        store.set_leaf(&iter_2, key("value"), Value::Str("hi".to_string()));
        let last = store
            .alias(&store.root, key("last"), &key("iter_2"))
            .unwrap();
        assert!(Rc::ptr_eq(&iter_2, &last));
        let via_child = store.child(&store.root, &key("last")).unwrap().unwrap();
        assert!(Rc::ptr_eq(&iter_2, &via_child));
    }

    #[test]
    fn alias_errors_for_an_unknown_target() {
        let store = Store::new();
        let err = store
            .alias(&store.root, key("last"), &key("iter_9"))
            .unwrap_err();
        assert!(err.0.contains("iter_9"));
    }

    #[test]
    fn parallel_branches_start_empty_regardless_of_the_parents_own_state() {
        let store = Store::new();
        set(&store.root, key("seen_before"), Value::Bool(true));
        let branches = store.parallel_branches(&store.root, 3).unwrap();
        assert_eq!(branches.len(), 3);
        for branch in &branches {
            assert!(branch.borrow().is_empty());
        }
    }

    #[test]
    fn parallel_branches_are_each_their_own_node() {
        let store = Store::new();
        let branches = store.parallel_branches(&store.root, 2).unwrap();
        assert!(!Rc::ptr_eq(&branches[0], &branches[1]));
    }

    #[test]
    fn two_dispatches_in_a_row_never_see_each_others_state() {
        // Regression for the address-reuse bug an address-keyed side
        // table had: branches (and a fresh `ensure_dict`) from a second
        // dispatch must never see content left behind by the first.
        let store = Store::new();
        {
            let branches = store.parallel_branches(&store.root, 2).unwrap();
            store.set_leaf(&branches[0], key("x"), Value::Int(1.into()));
            store.set_leaf(&branches[1], key("y"), Value::Int(2.into()));
            store.merge(&store.root, branches).unwrap();
        }
        let second = store.parallel_branches(&store.root, 2).unwrap();
        for branch in &second {
            assert!(branch.borrow().is_empty());
        }
        let shots = store.ensure_dict(&store.root, key("shots")).unwrap();
        assert!(shots.borrow().is_empty());
    }

    #[test]
    fn merge_applies_branches_in_index_order_last_writer_wins() {
        let store = Store::new();
        let branches = store.parallel_branches(&store.root, 2).unwrap();
        store.set_leaf(
            &branches[0],
            key("x"),
            Value::Str("from branch 0".to_string()),
        );
        store.set_leaf(
            &branches[1],
            key("x"),
            Value::Str("from branch 1".to_string()),
        );
        store.merge(&store.root, branches).unwrap();
        assert_eq!(
            get_value(&store.root, &key("x")),
            Some(Value::Str("from branch 1".to_string()))
        );
    }

    #[test]
    fn merge_appends_new_keys_and_keeps_an_existing_keys_position() {
        let store = Store::new();
        set(&store.root, key("existing"), Value::Int(1.into()));
        let branches = store.parallel_branches(&store.root, 2).unwrap();
        store.set_leaf(&branches[0], key("existing"), Value::Int(2.into()));
        store.set_leaf(&branches[0], key("from_0"), Value::Bool(true));
        store.set_leaf(&branches[1], key("from_1"), Value::Bool(false));
        store.merge(&store.root, branches).unwrap();
        let root = store.root.borrow();
        let positions: Vec<&Value> = root.keys().collect();
        assert_eq!(
            positions,
            vec![&key("existing"), &key("from_0"), &key("from_1")]
        );
        drop(root);
        assert_eq!(
            get_value(&store.root, &key("existing")),
            Some(Value::Int(2.into()))
        );
    }

    #[test]
    fn merge_keeps_a_branchs_own_container_child_by_identity() {
        let store = Store::new();
        let branches = store.parallel_branches(&store.root, 1).unwrap();
        let nested = store.ensure_dict(&branches[0], key("sub")).unwrap();
        store.set_leaf(&nested, key("y"), Value::Int(9.into()));
        store.merge(&store.root, branches).unwrap();
        let merged = store.child(&store.root, &key("sub")).unwrap().unwrap();
        assert!(Rc::ptr_eq(&nested, &merged));
        assert_eq!(get_value(&merged, &key("y")), Some(Value::Int(9.into())));
    }

    #[test]
    fn merge_keeps_an_alias_a_branch_created_as_an_alias() {
        let store = Store::new();
        let branches = store.parallel_branches(&store.root, 1).unwrap();
        let iter_0 = store.ensure_dict(&branches[0], key("iter_0")).unwrap();
        store.set_leaf(&iter_0, key("value"), Value::Int(1.into()));
        store
            .alias(&branches[0], key("last"), &key("iter_0"))
            .unwrap();
        store.merge(&store.root, branches).unwrap();
        let merged_iter_0 = store.child(&store.root, &key("iter_0")).unwrap().unwrap();
        let merged_last = store.child(&store.root, &key("last")).unwrap().unwrap();
        assert!(Rc::ptr_eq(&merged_iter_0, &merged_last));
    }

    #[test]
    fn snapshot_reflects_a_live_write_through_a_tracked_child() {
        let store = Store::new();
        let child = store.ensure_dict(&store.root, key("shots")).unwrap();
        store.set_leaf(&child, key("n"), Value::Int(3.into()));
        let snapshot = store.snapshot(&store.root);
        let Value::Dict(ref root) = snapshot else {
            panic!("expected a dict");
        };
        match root.get(&key("shots")) {
            Some(Value::Dict(shots)) => {
                assert_eq!(shots.get(&key("n")), Some(&Value::Int(3.into())))
            }
            other => panic!("expected a materialized dict, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_never_compacts_an_alias() {
        let store = Store::new();
        let iter_2 = store.ensure_dict(&store.root, key("iter_2")).unwrap();
        store.set_leaf(&iter_2, key("value"), Value::Str("done".to_string()));
        store
            .alias(&store.root, key("last"), &key("iter_2"))
            .unwrap();
        let snapshot = store.snapshot(&store.root);
        let Value::Dict(ref root) = snapshot else {
            panic!("expected a dict");
        };
        match root.get(&key("last")) {
            Some(Value::Dict(d)) => {
                assert_eq!(d.get(&key("value")), Some(&Value::Str("done".to_string())))
            }
            other => panic!("expected last's own full content, got {other:?}"),
        }
    }

    #[test]
    fn saved_compacts_an_aliased_last_to_a_ref() {
        let store = Store::new();
        let iter_2 = store.ensure_dict(&store.root, key("iter_2")).unwrap();
        store.set_leaf(&iter_2, key("value"), Value::Str("done".to_string()));
        store
            .alias(&store.root, key("last"), &key("iter_2"))
            .unwrap();
        let saved = store.saved(&store.root);
        let Value::Dict(ref root) = saved else {
            panic!("expected a dict");
        };
        let mut expected_ref = IndexMap::new();
        expected_ref.insert(key("$ref"), Value::Str("iter_2".to_string()));
        assert_eq!(root.get(&key("last")), Some(&Value::Dict(expected_ref)));
        match root.get(&key("iter_2")) {
            Some(Value::Dict(d)) => {
                assert_eq!(d.get(&key("value")), Some(&Value::Str("done".to_string())))
            }
            other => panic!("expected the real iter_2 node, got {other:?}"),
        }
    }

    #[test]
    fn saved_leaves_an_unaliased_last_untouched() {
        let store = Store::new();
        set(
            &store.root,
            key("last"),
            Value::Str("not an alias".to_string()),
        );
        let saved = store.saved(&store.root);
        let Value::Dict(ref root) = saved else {
            panic!("expected a dict");
        };
        assert_eq!(
            root.get(&key("last")),
            Some(&Value::Str("not an alias".to_string()))
        );
    }

    #[test]
    fn saved_leaves_a_tracked_last_untouched_when_it_matches_no_sibling() {
        let store = Store::new();
        // `last` is its own tracked child (not a plain value), but it was
        // never `alias`ed to a sibling `iter_<N>` -- compaction must not
        // fire just because the key is named `last`.
        let last = store.ensure_dict(&store.root, key("last")).unwrap();
        store.set_leaf(&last, key("value"), Value::Int(1.into()));
        let saved = store.saved(&store.root);
        let Value::Dict(ref root) = saved else {
            panic!("expected a dict");
        };
        match root.get(&key("last")) {
            Some(Value::Dict(d)) => assert_eq!(d.get(&key("value")), Some(&Value::Int(1.into()))),
            other => panic!("expected the real last node, got {other:?}"),
        }
    }

    #[test]
    fn saved_picks_the_last_matching_sibling_when_several_alias_the_same_node() {
        let store = Store::new();
        let iter_2 = store.ensure_dict(&store.root, key("iter_2")).unwrap();
        store.set_leaf(&iter_2, key("value"), Value::Int(1.into()));
        // `iter_5` is deliberately aliased to the exact same node as
        // `iter_2` (not a realistic loop shape, but the generic case
        // `_aliased_iter_key`'s own reverse scan has to break the tie on).
        store
            .alias(&store.root, key("iter_5"), &key("iter_2"))
            .unwrap();
        store
            .alias(&store.root, key("last"), &key("iter_2"))
            .unwrap();
        let saved = store.saved(&store.root);
        let Value::Dict(ref root) = saved else {
            panic!("expected a dict");
        };
        let mut expected_ref = IndexMap::new();
        expected_ref.insert(key("$ref"), Value::Str("iter_5".to_string()));
        assert_eq!(root.get(&key("last")), Some(&Value::Dict(expected_ref)));
    }

    #[test]
    fn is_iter_key_matches_only_the_exact_shape() {
        assert!(is_iter_key(&key("iter_0")));
        assert!(is_iter_key(&key("iter_42")));
        assert!(!is_iter_key(&key("iter_")));
        assert!(!is_iter_key(&key("iter_a")));
        assert!(!is_iter_key(&key("not_iter_1")));
        assert!(!is_iter_key(&Value::Int(1.into())));
    }
}
