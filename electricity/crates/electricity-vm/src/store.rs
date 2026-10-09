//! `Store`/`NodeRef`: the VM's own mutable state tree (DESIGN.md's store
//! design; issue #431's Lane B section). `NodeRef` is `Rc<RefCell<
//! IndexMap<Value, Value>>>` exactly as specified -- but *how* a child
//! container's own identity-aliasing (`node["last"]` sharing the same
//! object as `node["iter_2"]`, `Rc::ptr_eq`-visible, DESIGN.md §2.3) is
//! represented inside that `IndexMap<Value, Value>` is lane B's own
//! design decision, not fixed here: [`electricity_value::Value`] has no
//! variant of its own for "this is the same live node as that other
//! key", so lane B's `store.rs` is free to choose its own internal
//! convention for it (nothing outside this crate depends on one).
//!
//! Lane A stub: every operation beyond constructing an empty root always
//! errors.

use electricity_value::Value;
use indexmap::IndexMap;
use std::cell::RefCell;
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

/// [`Store`]'s own operations' error -- today, always "not implemented".
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
    /// own `node.setdefault(key, {})`-plus-type-coercion does.
    ///
    /// Lane A stub: always `Err`.
    pub fn ensure_dict(&self, parent: &NodeRef, key: Value) -> Result<NodeRef, StoreError> {
        let _ = (parent, key);
        Err(StoreError(
            "electricity_vm::Store::ensure_dict is not implemented yet (lane B, issue #431)"
                .to_string(),
        ))
    }

    /// *parent*'s existing child named *key*, if any.
    ///
    /// Lane A stub: always `Err`.
    pub fn child(&self, parent: &NodeRef, key: &Value) -> Result<Option<NodeRef>, StoreError> {
        let _ = (parent, key);
        Err(StoreError(
            "electricity_vm::Store::child is not implemented yet (lane B, issue #431)".to_string(),
        ))
    }

    /// *count* isolated branch stores for a tree `dynamic`/`each` loop --
    /// each a shallow snapshot of *parent* (DESIGN.md's own "shallow
    /// snapshot, isolated branch stores" rule, issue #431's Scope
    /// section), so a branch's own writes never leak into a sibling's.
    ///
    /// Lane A stub: always `Err`.
    pub fn parallel_branches(
        &self,
        parent: &NodeRef,
        count: usize,
    ) -> Result<Vec<NodeRef>, StoreError> {
        let _ = (parent, count);
        Err(StoreError(
            "electricity_vm::Store::parallel_branches is not implemented yet (lane B, issue #431)"
                .to_string(),
        ))
    }

    /// Merges *branches* back into *parent*, in index order (DESIGN.md's
    /// own tree-merge rule, issue #431's Scope section).
    ///
    /// Lane A stub: always `Err`.
    pub fn merge(&self, parent: &NodeRef, branches: Vec<NodeRef>) -> Result<(), StoreError> {
        let _ = (parent, branches);
        Err(StoreError(
            "electricity_vm::Store::merge is not implemented yet (lane B, issue #431)".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn ensure_dict_is_a_lane_b_stub() {
        let store = Store::new();
        let err = store
            .ensure_dict(&store.root, Value::Str("shots".to_string()))
            .unwrap_err();
        assert!(err.0.contains("lane B"));
    }
}
