//! `Store`/`NodeRef`: the VM's own mutable state tree (DESIGN.md's store
//! design; issue #431's Lane B section). `NodeRef` is `Rc<RefCell<
//! IndexMap<Value, Value>>>` exactly as specified -- but *how* a child
//! container's own identity-aliasing (`node["last"]` sharing the same
//! object as `node["iter_2"]`, `Rc::ptr_eq`-visible, DESIGN.md §2.3) is
//! represented inside that `IndexMap<Value, Value>` is lane B's own
//! design decision: `Value` has no variant of its own for "this is the
//! same live node as that other key" (`electricity_value::Value` is a
//! plain algebraic type with no `Rc` of its own), so a container child's
//! *real* `NodeRef` can never literally sit inside its parent's own
//! `IndexMap<Value, Value>` the way a Python dict's nested dict does.
//!
//! This `Store` instead keeps every container child's real `NodeRef`
//! reachable by identity in a side table (`children`, below), keyed by
//! the parent node's own `Rc` address plus the child's key -- the parent
//! node's own map holds only a placeholder `Value::Dict` at that key
//! (reserving the key's insertion-order position, and letting a caller
//! that only wants the *data* -- `Store::materialize` -- recurse through
//! the registry transparently). `ensure_dict`/`child` always resolve a
//! (parent, key) pair to that one canonical `NodeRef`, so two different
//! calls for the same key return `Rc::ptr_eq` siblings, and so does an
//! explicit [`Store::alias`] (the `last`-aliasing convention: see its own
//! doc comment). Every container node stays reachable (and so is never
//! freed, so its address is never reused by an unrelated allocation) for
//! as long as the owning [`Store`] lives, since the registry entry that
//! names it holds the only strong `Rc` a freshly created child starts
//! with.

use electricity_value::Value;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

/// A single mutable state node -- `Rc<RefCell<IndexMap<Value, Value>>>`
/// (issue #431's own seam specification), shared so that two different
/// paths can reference the identical live node (`Rc::ptr_eq`), not a
/// deep copy of it.
pub type NodeRef = Rc<RefCell<IndexMap<Value, Value>>>;

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

/// A node's `Rc` address -- the first half of [`Store`]'s own `children`
/// registry key. Two different `NodeRef`s are never equal under this,
/// even if their current contents happen to be equal, which is exactly
/// what a registry keyed on *identity* (not value) needs.
fn node_identity(node: &NodeRef) -> usize {
    Rc::as_ptr(node) as usize
}

/// The run's mutable state tree, rooted at `prime`.
pub struct Store {
    pub root: NodeRef,
    /// Every container child ever created by [`Store::ensure_dict`]/
    /// [`Store::child`]/[`Store::alias`], keyed by its parent's own
    /// identity (see [`node_identity`]) and its own key under that
    /// parent -- this crate's own representation of the identity-sharing
    /// a plain `Value` can't express (this module's own doc comment).
    /// A child, once registered, is kept alive here for the `Store`'s
    /// whole lifetime (this VM never needs to free an interior node
    /// early), which is also what keeps its parent's own `Rc` address
    /// from ever being reused by an unrelated node while an entry naming
    /// it still exists.
    children: RefCell<HashMap<(usize, Value), NodeRef>>,
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Store {
    pub fn new() -> Self {
        Store {
            root: new_node(),
            children: RefCell::new(HashMap::new()),
        }
    }

    /// Gets *parent*'s existing dict-valued child named *key*, or creates
    /// one -- overwriting a non-dict value at that key the way Python's
    /// own `node.setdefault(key, {})`-plus-type-coercion does
    /// (`core/store/store.py::Store.ensure_dict`). A key already tracked
    /// in [`Store::children`] (by a previous `ensure_dict`/`child`/
    /// `alias` call for the same parent and key) always returns that
    /// exact same `NodeRef` again -- the Rust shape of Python's `node[key]`
    /// being the identical dict object on every lookup.
    pub fn ensure_dict(&self, parent: &NodeRef, key: Value) -> Result<NodeRef, StoreError> {
        let identity = (node_identity(parent), key.clone());
        if let Some(existing) = self.children.borrow().get(&identity) {
            return Ok(Rc::clone(existing));
        }
        let initial = {
            let mut parent_map = parent.borrow_mut();
            let coerced = match parent_map.get_mut(&key) {
                Some(Value::Dict(existing)) => std::mem::take(existing),
                _ => IndexMap::new(),
            };
            // Reserves *key*'s own insertion-order position (a no-op if
            // it already held a dict -- `IndexMap::insert` never moves an
            // existing key) with a placeholder; `materialize` always
            // reads the real content back out of `children`, never this
            // placeholder, once a registry entry exists for this key.
            parent_map.insert(key.clone(), Value::Dict(IndexMap::new()));
            coerced
        };
        let child = Rc::new(RefCell::new(initial));
        self.children
            .borrow_mut()
            .insert(identity, Rc::clone(&child));
        Ok(child)
    }

    /// *parent*'s existing child named *key*, if any -- never creates one
    /// (unlike [`Store::ensure_dict`]). A dict-valued key that isn't yet
    /// tracked in [`Store::children`] (data merged in from a finished
    /// parallel branch, say -- see [`Store::merge`]) is promoted into a
    /// tracked node on this call, exactly as `ensure_dict` would, so a
    /// later call for the same key returns that identical `NodeRef`.
    pub fn child(&self, parent: &NodeRef, key: &Value) -> Result<Option<NodeRef>, StoreError> {
        let identity = (node_identity(parent), key.clone());
        if let Some(existing) = self.children.borrow().get(&identity) {
            return Ok(Some(Rc::clone(existing)));
        }
        let is_dict = matches!(parent.borrow().get(key), Some(Value::Dict(_)));
        if !is_dict {
            return Ok(None);
        }
        Ok(Some(self.ensure_dict(parent, key.clone())?))
    }

    /// Writes *value* at *key* under *parent* -- the Rust shape of the
    /// last segment of Python's own `parent[key] = value`
    /// (`core/store/store.py::Store.set`, after `ensure_dict` has already
    /// walked every ancestor segment -- that part is the caller's own
    /// job, one `ensure_dict` call per segment, exactly like `child`).
    /// Overwrites a plain value or a previously `ensure_dict`-ed
    /// container alike: any [`Store::children`] registry entry for this
    /// exact (parent, key) pair is dropped first, so a key that *was* a
    /// live-tracked container doesn't go on returning its old content
    /// through [`Store::materialize`] once something else has been
    /// written there -- including a brand new `Value::Dict`, which is
    /// never the same live node as whatever used to be at that key.
    /// Writing straight through `NodeRef`'s own `RefCell` instead of this
    /// method is exactly how that staleness bug happens; every real
    /// caller (this crate's own tests aside) should use this.
    pub fn set_leaf(&self, parent: &NodeRef, key: Value, value: Value) {
        self.children
            .borrow_mut()
            .remove(&(node_identity(parent), key.clone()));
        parent.borrow_mut().insert(key, value);
    }

    /// Makes *alias_key* resolve to the exact same live node that
    /// *target_key* already does under *parent* -- the Rust shape of
    /// Python's `node["last"] = node["iter_3"]` (a loop's own
    /// `last`-tracking, `core/loop.py::LoopRuntime._link_last`,
    /// DESIGN.md §6.7). `target_key` must already be a tracked child
    /// (via a prior `ensure_dict`/`child`/`alias` call); this is the one
    /// way this crate represents that two keys are the *same* node, since
    /// `Value` has no variant of its own for it (this module's own doc
    /// comment) -- `Store::materialize` is what later recognizes the
    /// alias (by `Rc::ptr_eq`) and compacts it to `{"$ref": "iter_3"}` on
    /// serialization. Out of scope for the M0-H interpreter (`loop` isn't
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
            .insert(alias_key.clone(), Value::Dict(IndexMap::new()));
        self.children
            .borrow_mut()
            .insert((node_identity(parent), alias_key), Rc::clone(&target));
        Ok(target)
    }

    /// *count* isolated branch stores for a tree `dynamic`/`each` loop --
    /// each starting **empty**, exactly as `core/store/store.py::
    /// Store.parallel_branches` does (`state={}`, not a copy of *parent*):
    /// a branch never sees *parent*'s existing keys, and concurrent
    /// branches never see each other's writes either, until
    /// [`Store::merge`] folds them back in. Brand new, untracked nodes --
    /// nothing registers them in [`Store::children`] until (if ever) a
    /// caller does that explicitly, since they're scratch stores a tree
    /// dispatch populates and then hands straight to `merge`.
    pub fn parallel_branches(
        &self,
        parent: &NodeRef,
        count: usize,
    ) -> Result<Vec<NodeRef>, StoreError> {
        let _ = parent;
        Ok((0..count).map(|_| new_node()).collect())
    }

    /// Merges *branches* back into *parent*, in index order
    /// (`core/store/store.py`'s own tree-merge rule: `node = dict(self.
    /// state); for snapshot in latest: node.update(snapshot)`). Each
    /// branch is [`Store::materialize`]d first -- a branch's own writes
    /// may include container children of its own (tracked under that
    /// branch's own identity in [`Store::children`]), and only the
    /// materialized, fully-flattened snapshot is accurate once the
    /// branch's own `NodeRef` is about to be discarded.
    ///
    /// Key order follows `dict.update`'s own rule: a key *parent* (or an
    /// earlier branch) already has keeps its original position and only
    /// its value is overwritten; a key no earlier source had is appended
    /// at the position its first-introducing branch reaches it. On a
    /// collision between branches, the **later** (higher-index) branch's
    /// value wins -- `latest` is applied to `node` in ascending index
    /// order, so each later `update` call overwrites whatever an earlier
    /// one wrote.
    pub fn merge(&self, parent: &NodeRef, branches: Vec<NodeRef>) -> Result<(), StoreError> {
        for branch in branches {
            let mut snapshot = self.materialize(&branch);
            let Value::Dict(ref mut entries) = snapshot else {
                unreachable!("materialize always returns a Value::Dict for a NodeRef");
            };
            let entries = std::mem::take(entries);
            for (key, value) in entries {
                self.set_leaf(parent, key, value);
            }
        }
        Ok(())
    }

    /// An owned [`Value`] snapshot of *node* -- recursively materializing
    /// every container child tracked in [`Store::children`] in place of
    /// the placeholder its parent's own map holds for it (this module's
    /// own doc comment), so the result reflects every live write
    /// regardless of how deep it happened. What a template/CEL
    /// evaluation's own `ctx` is built from (DESIGN.md §6.7's own
    /// `scope_ctx`/`local_writes` overlay rule on top of this snapshot is
    /// lane B2's own interpreter's job, not this crate's -- this method
    /// only ever produces a *plain* snapshot of one node, with no overlay
    /// of its own).
    ///
    /// In the same pass, compacts a `last` key that is [`Store::alias`]ed
    /// (`Rc::ptr_eq`) to one of its own `iter_<N>` siblings into
    /// `{"$ref": "iter_<N>"}`, exactly as `core/saved_state.py::
    /// compact_last_aliases` does for Python's own in-memory aliasing --
    /// the alias information only exists on the *live* `NodeRef` tree (an
    /// owned `Value` has no identity of its own to check once two
    /// branches have already been cloned apart), so this has to happen
    /// during materialization, not as a later pass over the result
    /// (DESIGN.md §6.7). A `last` that is a tracked child but *not*
    /// `Rc::ptr_eq` to any `iter_<N>` sibling -- or isn't tracked as a
    /// container child at all -- is materialized like any other key,
    /// untouched, exactly as Python leaves a `last` holding an
    /// independent value alone (checked by identity, never by key name
    /// alone).
    pub fn materialize(&self, node: &NodeRef) -> Value {
        let identity = node_identity(node);
        let children = self.children.borrow();
        let map = node.borrow();
        let last_key = Value::Str("last".to_string());
        let last_alias_target: Option<String> = children
            .get(&(identity, last_key.clone()))
            .and_then(|last_child| {
                map.iter().rev().find_map(|(key, _)| {
                    let Value::Str(name) = key else { return None };
                    if key == &last_key || !is_iter_key(key) {
                        return None;
                    }
                    let sibling = children.get(&(identity, key.clone()))?;
                    if Rc::ptr_eq(last_child, sibling) {
                        Some(name.clone())
                    } else {
                        None
                    }
                })
            });
        let mut result = IndexMap::with_capacity(map.len());
        for (key, value) in map.iter() {
            if key == &last_key {
                if let Some(target) = &last_alias_target {
                    let mut reference = IndexMap::with_capacity(1);
                    reference.insert(Value::Str("$ref".to_string()), Value::Str(target.clone()));
                    result.insert(key.clone(), Value::Dict(reference));
                    continue;
                }
            }
            let materialized = match children.get(&(identity, key.clone())) {
                Some(child) => self.materialize(child),
                None => value.clone(),
            };
            result.insert(key.clone(), materialized);
        }
        Value::Dict(result)
    }
}

/// `true` for a `Value::Str` matching `iter_<digits>` (`core/saved_state.
/// py`'s own `_ITER_KEY` pattern), the only sibling shape `materialize`
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
        assert!(matches!(
            store.root.borrow().get(&key("shots")),
            Some(Value::Dict(_))
        ));
    }

    #[test]
    fn ensure_dict_returns_the_same_node_on_a_second_call() {
        let store = Store::new();
        let first = store.ensure_dict(&store.root, key("shots")).unwrap();
        first.borrow_mut().insert(key("x"), Value::Int(1.into()));
        let second = store.ensure_dict(&store.root, key("shots")).unwrap();
        assert!(Rc::ptr_eq(&first, &second));
        assert_eq!(second.borrow().get(&key("x")), Some(&Value::Int(1.into())));
    }

    #[test]
    fn ensure_dict_coerces_a_non_dict_value_to_an_empty_dict() {
        let store = Store::new();
        store
            .root
            .borrow_mut()
            .insert(key("count"), Value::Int(5.into()));
        let child = store.ensure_dict(&store.root, key("count")).unwrap();
        assert!(child.borrow().is_empty());
        assert!(matches!(
            store.root.borrow().get(&key("count")),
            Some(Value::Dict(_))
        ));
    }

    #[test]
    fn ensure_dict_preserves_an_existing_dicts_own_contents() {
        let store = Store::new();
        let mut seed = IndexMap::new();
        seed.insert(key("a"), Value::Int(1.into()));
        store
            .root
            .borrow_mut()
            .insert(key("cfg"), Value::Dict(seed));
        let child = store.ensure_dict(&store.root, key("cfg")).unwrap();
        assert_eq!(child.borrow().get(&key("a")), Some(&Value::Int(1.into())));
    }

    #[test]
    fn child_never_creates_one() {
        let store = Store::new();
        assert_eq!(store.child(&store.root, &key("missing")).unwrap(), None);
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
        store
            .root
            .borrow_mut()
            .insert(key("cfg"), Value::Dict(seed));
        let found = store.child(&store.root, &key("cfg")).unwrap().unwrap();
        assert_eq!(found.borrow().get(&key("a")), Some(&Value::Int(1.into())));
        let found_again = store.child(&store.root, &key("cfg")).unwrap().unwrap();
        assert!(Rc::ptr_eq(&found, &found_again));
    }

    #[test]
    fn child_reports_none_for_a_non_dict_value() {
        let store = Store::new();
        store
            .root
            .borrow_mut()
            .insert(key("count"), Value::Int(5.into()));
        assert_eq!(store.child(&store.root, &key("count")).unwrap(), None);
    }

    #[test]
    fn alias_shares_the_same_node_as_its_target() {
        let store = Store::new();
        let iter_2 = store.ensure_dict(&store.root, key("iter_2")).unwrap();
        iter_2
            .borrow_mut()
            .insert(key("value"), Value::Str("hi".to_string()));
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
        store
            .root
            .borrow_mut()
            .insert(key("seen_before"), Value::Bool(true));
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
    fn merge_applies_branches_in_index_order_last_writer_wins() {
        let store = Store::new();
        let branches = store.parallel_branches(&store.root, 2).unwrap();
        branches[0]
            .borrow_mut()
            .insert(key("x"), Value::Str("from branch 0".to_string()));
        branches[1]
            .borrow_mut()
            .insert(key("x"), Value::Str("from branch 1".to_string()));
        store.merge(&store.root, branches).unwrap();
        assert_eq!(
            store.root.borrow().get(&key("x")),
            Some(&Value::Str("from branch 1".to_string()))
        );
    }

    #[test]
    fn merge_appends_new_keys_and_keeps_an_existing_keys_position() {
        let store = Store::new();
        store
            .root
            .borrow_mut()
            .insert(key("existing"), Value::Int(1.into()));
        let branches = store.parallel_branches(&store.root, 2).unwrap();
        branches[0]
            .borrow_mut()
            .insert(key("existing"), Value::Int(2.into()));
        branches[0]
            .borrow_mut()
            .insert(key("from_0"), Value::Bool(true));
        branches[1]
            .borrow_mut()
            .insert(key("from_1"), Value::Bool(false));
        store.merge(&store.root, branches).unwrap();
        let root = store.root.borrow();
        let positions: Vec<&Value> = root.keys().collect();
        assert_eq!(
            positions,
            vec![&key("existing"), &key("from_0"), &key("from_1")]
        );
        assert_eq!(root.get(&key("existing")), Some(&Value::Int(2.into())));
    }

    #[test]
    fn merge_flattens_a_branchs_own_nested_container_children() {
        let store = Store::new();
        let branches = store.parallel_branches(&store.root, 1).unwrap();
        let nested = store.ensure_dict(&branches[0], key("sub")).unwrap();
        nested.borrow_mut().insert(key("y"), Value::Int(9.into()));
        store.merge(&store.root, branches).unwrap();
        let merged = store.root.borrow();
        match merged.get(&key("sub")) {
            Some(Value::Dict(d)) => assert_eq!(d.get(&key("y")), Some(&Value::Int(9.into()))),
            other => panic!("expected a materialized dict, got {other:?}"),
        }
    }

    #[test]
    fn materialize_reflects_a_live_write_through_a_tracked_child() {
        let store = Store::new();
        let child = store.ensure_dict(&store.root, key("shots")).unwrap();
        child.borrow_mut().insert(key("n"), Value::Int(3.into()));
        let snapshot = store.materialize(&store.root);
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
    fn materialize_compacts_an_aliased_last_to_a_ref() {
        let store = Store::new();
        let iter_2 = store.ensure_dict(&store.root, key("iter_2")).unwrap();
        iter_2
            .borrow_mut()
            .insert(key("value"), Value::Str("done".to_string()));
        store
            .alias(&store.root, key("last"), &key("iter_2"))
            .unwrap();
        let snapshot = store.materialize(&store.root);
        let Value::Dict(ref root) = snapshot else {
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
    fn materialize_leaves_an_unaliased_last_untouched() {
        let store = Store::new();
        store
            .root
            .borrow_mut()
            .insert(key("last"), Value::Str("not an alias".to_string()));
        let snapshot = store.materialize(&store.root);
        let Value::Dict(ref root) = snapshot else {
            panic!("expected a dict");
        };
        assert_eq!(
            root.get(&key("last")),
            Some(&Value::Str("not an alias".to_string()))
        );
    }

    #[test]
    fn materialize_leaves_a_tracked_last_untouched_when_it_matches_no_sibling() {
        let store = Store::new();
        // `last` is its own tracked child (not a plain value), but it was
        // never `alias`ed to a sibling `iter_<N>` -- compaction must not
        // fire just because the key is named `last`.
        let last = store.ensure_dict(&store.root, key("last")).unwrap();
        last.borrow_mut().insert(key("value"), Value::Int(1.into()));
        let snapshot = store.materialize(&store.root);
        let Value::Dict(ref root) = snapshot else {
            panic!("expected a dict");
        };
        match root.get(&key("last")) {
            Some(Value::Dict(d)) => assert_eq!(d.get(&key("value")), Some(&Value::Int(1.into()))),
            other => panic!("expected the real last node, got {other:?}"),
        }
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
