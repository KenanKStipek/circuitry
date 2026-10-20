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

/// A node's own top-level map, plus an optional *fallback* node consulted
/// for any key *entries* doesn't itself carry (issue #484). Every node
/// [`Store`] itself creates (`ensure_dict`/`new_node`/`parallel_branches`)
/// has `fallback: None` -- only three `exec::loop_` builders
/// (`crate::exec::loop_`, not re-exported outside this crate) --
/// `plain_overlay`, `with_prev_overlay` and `step_overlay` -- build one
/// with `Some`, layering a pass's or step's own top-level overlay (and,
/// for `with_prev_overlay`, the detached `prime.<loop_name>` node a
/// chain-flow `each`/`while` pass's own ctx overlays) on top of the
/// loop's real, already-tracked node: *entries* carries only that
/// handful of local keys, so a fresh overlay never re-walks the real
/// node's ever-growing `iter_<N>` key set, and every other key --
/// `iter_<N>`, `meta`, an outer loop's own already-layered overlay one
/// level up -- still reaches the real node through *fallback*.
/// [`Node`] derefs to its own *entries* so every existing `.borrow()`/
/// `.borrow_mut()` call site that only ever reads/writes a node known to
/// never carry a *fallback* (every [`Store`] write-path method --
/// `ensure_dict`/`child`/`alias`/`merge` -- only ever takes one of
/// those as *parent*; every `#[cfg(test)]`-only direct-entry helper in
/// this crate only ever builds its own nodes through `Store`) keeps
/// compiling unchanged. A reader that *can* reach one of the three
/// overlay builders' nodes -- anything fed a [`CtxChain`]/`CtxSource::
/// Live` entry -- must resolve through [`Store::peek`]/[`Store::
/// snapshot`] (an O(1)/recursive fallback walk) or [`Node::flat_entries`]
/// (an eager, recursive flatten: still correct, just back to paying
/// *fallback*'s own depth in clones, the pre-#484 cost `exec::
/// loop_::chain_slots`/`dict_slots` and `exec::conditional::
/// chain_slots`/`dict_slots` already pay once per `if`-branch-step/rare
/// multi-entry-chain call, never per pass) -- plain `.iter()`/`.get()`/
/// Deref on such a node silently drops every key *fallback* alone
/// carries (issue #501's review: `exec::conditional`'s
/// `chain_slots`/`dict_slots` did exactly that for a release, losing an
/// `if`'s enclosing scope from its second branch step on inside any
/// loop body). [`Store::saved`] never sees an overlay node at all: it
/// only ever recurses into a [`Slot::Node`] reachable from
/// [`Store::root`](Store), and no overlay node is ever attached to the
/// real tree.
#[derive(Debug, Clone)]
pub struct Node {
    entries: IndexMap<Value, Slot>,
    fallback: Option<NodeRef>,
    /// A single *fallback* key this overlay must never expose, even once
    /// *fallback* -- a real, still-mutating tracked node -- comes to
    /// carry it later in the same pass (issue #501's review).
    /// [`exec::loop_::with_prev_overlay`] sets this to the current
    /// pass's own `iter_<idx>` key: that key doesn't exist in the real
    /// loop node yet when the overlay is built (`execute_loop`'s own
    /// `store.ensure_dict(write_parent, iter_<idx>)` runs straight
    /// after), but *fallback* is a live `NodeRef` to that same real
    /// node, so once a later step in *this* pass does create it, every
    /// reader of the overlay would otherwise see it immediately --
    /// something `core/loop.py::LoopRuntime._with_prev`'s own one-time
    /// `dict(own_node)` copy (taken before `iter_<idx>` exists) never
    /// does: Python's shallow copy freezes the *set* of keys as of that
    /// moment, even though a nested value already in that set (`meta`,
    /// an *earlier* `iter_<K>`) keeps aliasing its live dict and so
    /// still reflects any further mutation. `None` -- every other
    /// overlay builder, and *fallback* itself one level further down --
    /// never needs one.
    hidden: Option<Value>,
    /// *fallback*'s own direct-entry count, frozen at the moment this
    /// overlay was built -- any key *fallback* gains at or after that
    /// position (issue #501's second review) is invisible through this
    /// overlay, exactly as if it didn't exist. `core/scope.py::
    /// scope_ctx`'s own `prime = dict(ctx.get("prime") or {})` takes
    /// this same one-time snapshot of `prime`'s own key SET after every
    /// body step (not just a loop's own `_with_prev`, [`Node::hidden`]'s
    /// one key) -- [`exec::loop_::step_overlay`]'s own `prime_fallback`
    /// is a live `NodeRef` reused across every later step/pass instead,
    /// so a step that isn't the first in its own scope could otherwise
    /// see a *later* sibling's own skeleton node (created, not yet
    /// rendered -- `exec::tool::execute_tool` `ensure_dict`s an effect's
    /// own node before rendering its params) the instant that sibling
    /// starts, something cof's own frozen copy never exposes. Exact
    /// under two invariants this count alone can't re-check on every
    /// read, so both are checked once, elsewhere, instead:
    ///
    /// 1. Nothing ever `shift_remove`s a key from the specific node a
    ///    watermark counts against -- checked for every `shift_remove`
    ///    call site in this crate: every one operates on an effect's
    ///    own nested `meta` dict (`exec::tool`/`exec::use_`'s own
    ///    `pop_leaf`, `exec::loop_`'s own `failed_passes` pop), never on
    ///    a `prime`/loop-name node directly -- the only kind [`exec::
    ///    loop_::step_overlay`]/[`exec::loop_::with_prev_overlay`] ever
    ///    watermark or hide a key on. A `shift_remove` there would shift
    ///    every later key's own index down by one, silently exposing
    ///    whatever key the watermark's own count next lands on instead
    ///    of the one actually meant.
    /// 2. The node a watermark counts against only ever *grows* new
    ///    top-level keys from here on -- never itself becomes an
    ///    overlay with a `fallback`/`hidden`/`watermark` of its own,
    ///    whose growth this type's own index-based count can't see
    ///    through. True because every [`Store`] write-path method
    ///    (`ensure_dict`/`set_leaf`/`alias`/`merge`) only ever takes a
    ///    real, `Store`-created node as *parent* -- each one
    ///    `debug_assert!`s `parent`'s own `fallback` is `None` on entry
    ///    -- and a real node's `fallback`/`hidden`/`watermark` are
    ///    always `None` for its whole lifetime (only `exec::loop_`'s own
    ///    `plain_overlay`/`with_prev_overlay`/`step_overlay` ever build
    ///    one of those, and never write into it afterward).
    watermark: Option<usize>,
}

impl Node {
    /// *self*'s own top-level map, with no [`Node::fallback`] applied.
    /// Correct only when the caller already knows *self* never carries a
    /// *fallback* (every [`Store`]-created node) -- see this struct's
    /// own doc comment. Reaching into a node fed from a [`CtxChain`]
    /// instead needs [`Node::flat_entries`].
    pub fn entries(&self) -> &IndexMap<Value, Slot> {
        &self.entries
    }

    /// *self*'s own entries layered on top of *fallback*'s own
    /// [`Node::flat_entries`] (recursively, so a chain of overlays still
    /// flattens to one map, and [`Node::hidden`]/[`Node::watermark`] are
    /// respected at every hop), or just *self*'s own entries if
    /// *fallback* is `None` -- an eager version of the lazy walk
    /// [`Store::peek`]/[`Store::snapshot`] do one key/one whole subtree
    /// at a time, for a caller (`exec::loop_`/`exec::conditional`'s own
    /// `chain_slots`/`dict_slots`) that needs every key as a plain,
    /// detached [`IndexMap`] up front rather than a further live
    /// [`NodeRef`] to keep reading lazily. *Not* O(1): this walks the
    /// whole fallback chain and clones every key it carries, same as
    /// the pre-#484 shape of the two callers above -- correct for them
    /// precisely because neither is on an O(1)-per-pass path (unlike
    /// `exec::loop_`'s own `plain_overlay`/`with_prev_overlay`/
    /// `step_overlay`, which layer directly via [`Store::peek`]-backed
    /// fallback resolution and never call this).
    pub fn flat_entries(&self) -> IndexMap<Value, Slot> {
        self.flat_entries_under(None)
    }

    /// [`Node::flat_entries`]'s own recursion, carrying *watermark* --
    /// the caller one level up's own [`Node::watermark`] field, if any,
    /// constraining *this* node's own direct entries (not *its*
    /// `fallback`'s, which only ever answers to *this* node's own
    /// `watermark` field instead, checked when recursing below).
    fn flat_entries_under(&self, watermark: Option<usize>) -> IndexMap<Value, Slot> {
        let mut merged = match &self.fallback {
            Some(fallback) => {
                let mut m = fallback.borrow().flat_entries_under(self.watermark);
                if let Some(hidden) = &self.hidden {
                    m.shift_remove(hidden);
                }
                m
            }
            None => IndexMap::with_capacity(self.entries.len()),
        };
        for (k, v) in self.entries.iter() {
            merged.insert(k.clone(), v.clone());
        }
        if let Some(n) = watermark {
            for (k, _) in self.entries.iter().skip(n) {
                merged.shift_remove(k);
            }
        }
        merged
    }
}

impl std::ops::Deref for Node {
    type Target = IndexMap<Value, Slot>;
    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl std::ops::DerefMut for Node {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.entries
    }
}

/// A single mutable state node -- `Rc<RefCell<Node>>`. Shared so that two
/// different paths can reference the identical live node (`Rc::ptr_eq`),
/// not a deep copy of it.
pub type NodeRef = Rc<RefCell<Node>>;

/// A fresh, empty node with no fallback.
pub fn new_node() -> NodeRef {
    Rc::new(RefCell::new(Node {
        entries: IndexMap::new(),
        fallback: None,
        hidden: None,
        watermark: None,
    }))
}

/// A fresh node carrying *entries*, with no fallback -- the plain,
/// detached-overlay shape every ctx-chain builder in `exec::loop_`/
/// `exec::conditional`/`exec::live_scope` already wraps a freshly merged
/// map in.
pub fn node_from_entries(entries: IndexMap<Value, Slot>) -> NodeRef {
    Rc::new(RefCell::new(Node {
        entries,
        fallback: None,
        hidden: None,
        watermark: None,
    }))
}

/// A fresh node carrying *entries*, falling back to *fallback* for any
/// key *entries* doesn't have -- see [`Node`]'s own doc comment.
pub fn node_with_fallback(entries: IndexMap<Value, Slot>, fallback: NodeRef) -> NodeRef {
    Rc::new(RefCell::new(Node {
        entries,
        fallback: Some(fallback),
        hidden: None,
        watermark: None,
    }))
}

/// [`node_with_fallback`], plus [`Node::hidden`] -- a single key
/// *fallback* must never expose through this overlay, even once
/// *fallback* itself comes to carry it (issue #501's review).
pub fn node_with_fallback_hiding(
    entries: IndexMap<Value, Slot>,
    fallback: NodeRef,
    hidden: Value,
) -> NodeRef {
    Rc::new(RefCell::new(Node {
        entries,
        fallback: Some(fallback),
        hidden: Some(hidden),
        watermark: None,
    }))
}

/// [`node_with_fallback`], plus [`Node::watermark`] -- *fallback*'s own
/// direct-entry count as of right now, so any key it gains afterward
/// stays invisible through this overlay (issue #501's second review:
/// `exec::loop_::step_overlay`'s own use).
pub fn node_with_fallback_watermarked(
    entries: IndexMap<Value, Slot>,
    fallback: NodeRef,
    watermark: usize,
) -> NodeRef {
    Rc::new(RefCell::new(Node {
        entries,
        fallback: Some(fallback),
        hidden: None,
        watermark: Some(watermark),
    }))
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
    /// Counts every [`Store::snapshot`] call -- test-only, so a test can
    /// prove a presence check (`{{> name}}` composition's own
    /// `ScopeRoot::contains`, issues #487/#492) never
    /// materializes a whole subtree just to answer a yes/no question,
    /// rather than asserting on the resulting text alone (which a
    /// correct-but-costly path would also get right).
    #[cfg(test)]
    snapshot_calls: std::cell::Cell<usize>,
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
            #[cfg(test)]
            snapshot_calls: std::cell::Cell::new(0),
        }
    }

    /// This store's own [`Store::snapshot`] call count so far -- see the
    /// field's own doc comment.
    #[cfg(test)]
    pub(crate) fn snapshot_call_count(&self) -> usize {
        self.snapshot_calls.get()
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
        debug_assert!(
            parent.borrow().fallback.is_none(),
            "Store::ensure_dict: parent is one of exec::loop_'s detached overlay nodes, not a \
             real Store-created one -- see Node's own doc comment"
        );
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
        let child = node_from_entries(converted);
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
        debug_assert!(
            parent.borrow().fallback.is_none(),
            "Store::set_leaf: parent is one of exec::loop_'s detached overlay nodes, not a \
             real Store-created one -- see Node's own doc comment"
        );
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
        debug_assert!(
            parent.borrow().fallback.is_none(),
            "Store::alias: parent is one of exec::loop_'s detached overlay nodes, not a real \
             Store-created one -- see Node's own doc comment"
        );
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
        debug_assert!(
            parent.borrow().fallback.is_none(),
            "Store::merge: parent is one of exec::loop_'s detached overlay nodes, not a real \
             Store-created one -- see Node's own doc comment"
        );
        for branch in branches {
            let entries = std::mem::take(&mut branch.borrow_mut().entries);
            for (key, slot) in entries {
                parent.borrow_mut().insert(key, slot);
            }
        }
        Ok(())
    }

    /// *parent*'s own [`Slot`] at *key*, cloned, with no promotion and
    /// no descent -- unlike [`Store::child`], a plain leaf
    /// [`Slot::Value`] (even a `Value::Dict` one nobody has ever called
    /// [`Store::ensure_dict`]/[`Store::child`] on) comes back exactly as
    /// it is, not coerced into a tracked [`Slot::Node`]. This is the one
    /// primitive a dotted-path walk against the *live* tree (issue #463:
    /// a template/CEL render that resolves each segment itself, rather
    /// than paying for a whole-subtree [`Store::snapshot`] up front)
    /// needs at each segment: O(1), and a clone of at most one child --
    /// an `Rc` bump for a [`Slot::Node`], or whatever that one leaf's own
    /// size is for a [`Slot::Value`], never anything *else* under
    /// *parent*.
    ///
    /// Falls through to *parent*'s own [`Node::fallback`], if any and if
    /// *key* isn't one of *parent*'s own entries -- another O(1)
    /// [`RefCell::borrow`], never a walk of anything *fallback* doesn't
    /// itself also have to fall through for. Short-circuits to `None`
    /// without ever touching *fallback* if *key* is *parent*'s own
    /// [`Node::hidden`] key instead; same if *key* is one of
    /// *fallback*'s own direct entries but only reached its position at
    /// or after *parent*'s own [`Node::watermark`] -- a key *fallback*
    /// (real, still-mutating) gained *after* *parent* was built.
    pub fn peek(&self, parent: &NodeRef, key: &Value) -> Option<Slot> {
        self.peek_under(parent, key, None)
    }

    /// [`Store::peek`]'s own recursion, carrying *watermark* -- the
    /// caller one level up's own [`Node::watermark`], if any,
    /// constraining *this* node's own direct entries.
    fn peek_under(&self, parent: &NodeRef, key: &Value, watermark: Option<usize>) -> Option<Slot> {
        let node = parent.borrow();
        if let Some(slot) = node.entries.get(key) {
            if watermark.is_some_and(|n| node.entries.get_index_of(key).is_some_and(|idx| idx >= n))
            {
                return None;
            }
            return Some(slot.clone());
        }
        if node.hidden.as_ref() == Some(key) {
            return None;
        }
        let fallback = node.fallback.clone();
        let next_watermark = node.watermark;
        drop(node);
        fallback.and_then(|f| self.peek_under(&f, key, next_watermark))
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
    ///
    /// A node with a [`Node::fallback`] (the `prime.<loop_name>` overlay
    /// [`exec::loop_::with_prev_overlay`] builds, issue #484) starts from
    /// *fallback*'s own full snapshot -- recursively, so a chain of
    /// overlays still resolves to one flat dict -- then layers *node*'s
    /// own entries (here, just `prev`) on top, the same "later entry
    /// wins its whole key, in whichever position it first appears"
    /// [`IndexMap::insert`] already gives every other merge in this
    /// crate -- *including* a key some enclosing overlay shadows (issue
    /// #501's second review: a placeholder at that key's own
    /// *natural* position, the same one Python's `{**d, k: v}`/`prime[
    /// name] = own` would have left it at, holds that position until
    /// whichever shallower hop actually owns the key overwrites it --
    /// `IndexMap::insert` on an *already-present* key overwrites the
    /// value without moving it, so the final position is the first,
    /// deepest place this key was ever seen, never wherever the
    /// shallowest hop happened to insert it after skipping every deeper
    /// one). A key some overlay *does* shadow (on the first
    /// review: without the skip half of this, a key `with_prev_
    /// overlay`'s three stacked overlays all happen to leave untouched
    /// -- the common case, since each only ever adds `prev`/the loop
    /// name/`prime` -- would still get resolved once per overlay hop on
    /// the way down, including a full, recursive [`Slot::Node`] subtree
    /// resolve for a container value, only to be discarded the moment a
    /// shallower hop's own entry for that same key overwrites it) has
    /// its *value* resolved once, at the hop that actually owns it, but
    /// a placeholder [`Value::None`] reserves its *position* at every
    /// shadowed hop below that: [`Self::snapshot_shadowed`]'s own
    /// *shadow* accumulates every enclosing overlay's own entries on the
    /// way down, so a key any of them will supply anyway is skipped
    /// (value-wise) at every hop below, the same "later entry wins" rule
    /// applied before paying for the value instead of after, without
    /// also reordering it the way plain omission would. [`Node::hidden`]
    /// is skipped entirely -- no placeholder, no later owner to overwrite
    /// it -- unconditionally (per the first review) -- see its
    /// own doc comment.
    pub fn snapshot(&self, node: &NodeRef) -> Value {
        #[cfg(test)]
        self.snapshot_calls.set(self.snapshot_calls.get() + 1);
        Value::Dict(self.snapshot_shadowed(node, &[], None))
    }

    /// [`Store::snapshot`]'s own recursion, carrying *shadow* -- every
    /// enclosing overlay's own entries, outermost first -- down through
    /// *node*'s own [`Node::fallback`] chain -- and *watermark*, the
    /// caller one level up's own [`Node::watermark`], if any,
    /// constraining *node*'s own direct entries the same way
    /// [`Node::hidden`] does: skipped entirely, no placeholder, since a
    /// watermarked-away key is a brand new one nothing deeper could
    /// already be shadowing. Never touches [`Store::snapshot_calls`]:
    /// that counter tracks how many *whole subtrees* a caller asked this
    /// crate to materialize, not how many internal hops doing so took.
    fn snapshot_shadowed(
        &self,
        node: &NodeRef,
        shadow: &[&IndexMap<Value, Slot>],
        watermark: Option<usize>,
    ) -> IndexMap<Value, Value> {
        let map = node.borrow();
        let mut result = match &map.fallback {
            Some(fallback) => {
                let mut next_shadow = shadow.to_vec();
                next_shadow.push(&map.entries);
                let mut resolved = self.snapshot_shadowed(fallback, &next_shadow, map.watermark);
                if let Some(hidden) = &map.hidden {
                    resolved.shift_remove(hidden);
                }
                resolved
            }
            None => IndexMap::with_capacity(map.entries.len()),
        };
        for (idx, (key, slot)) in map.entries.iter().enumerate() {
            if watermark.is_some_and(|n| idx >= n) {
                continue;
            }
            if shadow.iter().any(|s| s.contains_key(key)) {
                // Reserve *key*'s own position (its first, deepest
                // appearance) with a placeholder rather than skipping it
                // outright -- the shallower hop that actually owns this
                // key still runs this same loop at its own level and
                // overwrites the placeholder in place, never appending a
                // fresh entry at the end the way an outright `continue`
                // (pre-this-fix) would.
                result.entry(key.clone()).or_insert(Value::None);
                continue;
            }
            let value = match slot {
                Slot::Value(v) => v.clone(),
                Slot::Node(child) => Value::Dict(self.snapshot_shadowed(child, &[], None)),
            };
            result.insert(key.clone(), value);
        }
        result
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
    fn peek_reports_none_for_a_missing_key() {
        let store = Store::new();
        assert!(store.peek(&store.root, &key("missing")).is_none());
    }

    #[test]
    fn peek_reports_a_leaf_without_promoting_it() {
        let store = Store::new();
        let mut seed = IndexMap::new();
        seed.insert(key("a"), Value::Int(1.into()));
        set(&store.root, key("cfg"), Value::Dict(seed.clone()));
        match store.peek(&store.root, &key("cfg")) {
            Some(Slot::Value(Value::Dict(ref d))) => assert_eq!(*d, seed),
            other => panic!("expected the plain dict leaf, unpromoted, got {other:?}"),
        }
        // Still a plain leaf, not promoted into a tracked node -- unlike
        // `child`, `peek` never mutates what it reads.
        assert!(!is_node(&store.root, &key("cfg")));
    }

    #[test]
    fn peek_reports_a_tracked_node_as_a_slotnode() {
        let store = Store::new();
        let shots = store.ensure_dict(&store.root, key("shots")).unwrap();
        match store.peek(&store.root, &key("shots")) {
            Some(Slot::Node(n)) => assert!(Rc::ptr_eq(&n, &shots)),
            other => panic!("expected the tracked node, got {other:?}"),
        }
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

    /// Issue #501's third review (P1 -- "the watermark has no test"):
    /// `peek`/`snapshot`/`Node::flat_entries` must all agree a
    /// watermarked overlay hides a key its own `fallback` gains *after*
    /// the watermark was taken.
    #[test]
    fn watermark_hides_a_key_added_to_the_fallback_after_it_was_taken() {
        let store = Store::new();
        let real = store.ensure_dict(&store.root, key("prime")).unwrap();
        store.ensure_dict(&real, key("a")).unwrap();

        let watermark = real.borrow().len();
        let overlay = node_with_fallback_watermarked(IndexMap::new(), Rc::clone(&real), watermark);

        // A key added to `real` *after* the watermark was taken.
        let later = store.ensure_dict(&real, key("b")).unwrap();
        store.set_leaf(&later, key("v"), Value::Int(9.into()));

        assert!(
            store.peek(&overlay, &key("b")).is_none(),
            "peek must hide a key fallback gained after the watermark"
        );
        assert!(
            !overlay.borrow().flat_entries().contains_key(&key("b")),
            "flat_entries must hide it too"
        );
        let Value::Dict(ref snapshot) = store.snapshot(&overlay) else {
            panic!("expected a dict");
        };
        assert!(
            !snapshot.contains_key(&key("b")),
            "snapshot must hide it too, got {snapshot:?}"
        );
        // `a` -- present before the watermark was taken -- must still
        // be visible through all three, unaffected.
        assert!(store.peek(&overlay, &key("a")).is_some());
        assert!(overlay.borrow().flat_entries().contains_key(&key("a")));
        assert!(snapshot.contains_key(&key("a")));
    }

    /// The watermark counts *fallback*'s own direct entries, never
    /// descends into an existing child -- a write inside a child that
    /// already existed when the watermark was taken must stay visible
    /// through all three readers, exactly as a key `fallback` already
    /// had would.
    #[test]
    fn watermark_still_sees_a_write_inside_an_already_present_child() {
        let store = Store::new();
        let real = store.ensure_dict(&store.root, key("prime")).unwrap();
        let existing_child = store.ensure_dict(&real, key("a")).unwrap();
        store.set_leaf(&existing_child, key("v"), Value::Int(1.into()));

        let watermark = real.borrow().len();
        let overlay = node_with_fallback_watermarked(IndexMap::new(), Rc::clone(&real), watermark);

        // A write *inside* `a` -- not a new top-level key on `real` --
        // after the watermark was taken.
        store.set_leaf(&existing_child, key("v"), Value::Int(2.into()));

        match store.peek(&overlay, &key("a")) {
            Some(Slot::Node(n)) => assert!(Rc::ptr_eq(&n, &existing_child)),
            other => panic!("expected the existing child, got {other:?}"),
        }
        assert_eq!(
            get_value(&existing_child, &key("v")),
            Some(Value::Int(2.into()))
        );

        let flat = overlay.borrow().flat_entries();
        match flat.get(&key("a")) {
            Some(Slot::Node(n)) => assert!(Rc::ptr_eq(n, &existing_child)),
            other => panic!("flat_entries expected the existing child, got {other:?}"),
        }

        let Value::Dict(ref snapshot) = store.snapshot(&overlay) else {
            panic!("expected a dict");
        };
        match snapshot.get(&key("a")) {
            Some(Value::Dict(a)) => {
                assert_eq!(a.get(&key("v")), Some(&Value::Int(2.into())))
            }
            other => panic!("snapshot expected a's own materialized dict, got {other:?}"),
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
