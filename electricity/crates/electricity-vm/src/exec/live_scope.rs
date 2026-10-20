//! Issue #463: resolving a template/CEL render's own `ctx`/`state` from
//! the *live* store, a dotted segment at a time, instead of paying for
//! a whole-subtree [`Store::snapshot`] up front through [`super::
//! live_ctx`] -- the cost [`super::live_ctx`]'s own doc comment already
//! names as this crate's one remaining architectural waste: inside a
//! named loop, every tool render and every `while`/`if` condition
//! re-walks every earlier `iter_<N>` pass in full, regardless of
//! whether the render in question ever reads any of them.
//!
//! [`LiveRoot`] is this module's own answer: a [`CtxChain`] plus the
//! [`Store`] it chains into, implementing [`electricity_template::
//! ScopeRoot`] (so a template renders straight off the live tree,
//! [`electricity_template::render_template_with_root`]'s own seam for
//! exactly this) and [`crate::params::ParamsCtx`] (so a tool's
//! `{from: ...}` params walk, and a `use` effect's own `{from:}`
//! input walk, read the live tree the same way). Both resolve a key's
//! *first* segment the same way [`super::live_ctx`]'s own merge rule
//! does -- scanning the chain from its last entry backward, stopping
//! at the first one whose own top level actually has that segment
//! ([`chain_first_segment`]) -- then keep descending through further
//! live [`Slot::Node`] children for as long as the path stays inside
//! the tree the store actually tracks ([`live_child`]), falling back
//! to [`electricity_template::step`]'s or a `{from:}` walk's own
//! per-segment rule only once a plain [`Slot::Value`] leaf is reached.
//! Nothing this walk doesn't actually touch is ever cloned, let alone
//! snapshotted.
//!
//! [`build_cel_state`] is the parallel fix for `if`/`while` CEL
//! conditions' own `state` root: collect the expression's
//! `state.`-rooted paths first ([`electricity_cel::condition_paths`]),
//! then build only those paths' subtrees through [`project_live`] --
//! the same narrowing [`electricity_cel::evaluate_condition`] already
//! does internally (`project`), just performed against the live store
//! instead of a `Value` someone already paid to materialize in full.
//! An expression that can't be narrowed this way (`size(state)`, or
//! anything that fails to parse here --
//! [`evaluate_condition`](electricity_cel::evaluate_condition)'s own,
//! later compile attempt is what actually reports a syntax error)
//! falls back to [`super::live_ctx`]'s full snapshot; that fallback is
//! correct, not fast, same as this crate's existing full-state path
//! today. [`build_cel_state`] is **not** also `evaluate_expect`'s own
//! `state` builder -- see the next paragraph.
//!
//! A tool or `use` effect's own `expect:`'s own `state` argument is
//! deliberately **not** narrowed this way ([`crate::params::
//! ParamsCtx`] has no equivalent of [`build_cel_state`] for it):
//! unlike `evaluate_condition`, `evaluate_expect` converts the whole
//! `state` it's handed before its own CEL evaluation ever runs, with
//! no internal `project` of its own (this crate's own `evaluate_expect`
//! doc comment, and `core/cel_eval.py::evaluate_cel_expect` before it)
//! -- a big int *anywhere* in `state`, read by the expression or not,
//! already makes it raise today, on both engines. Narrowing it here
//! would quietly drop that unrelated big int and *fix* a bug this
//! issue is explicitly not about (`exec::tool`'s and `exec::use_`'s
//! own `expect:` call sites both keep building `state` through
//! [`electricity_template::ScopeRoot::whole`] instead, exactly
//! [`super::live_ctx`]'s full cost, unchanged).

use super::{CtxChain, CtxSource, live_ctx};
use crate::params::ParamsCtx;
use crate::{NodeRef, Slot, Store};
use electricity_value::Value;
use indexmap::IndexMap;

/// A dotted-path walk's own in-progress position: still inside the live
/// store (so a further segment may descend through another
/// [`Slot::Node`] child without materializing anything), or already
/// resolved to a plain [`Value`] -- a [`Slot::Value`] leaf that was
/// itself a nested dict/list nobody ever called [`Store::ensure_dict`]/
/// [`Store::child`] on, a scalar, or (for [`LiveStep::materialize`]'s
/// own `Node` arm) a subtree a caller needs in full.
pub(crate) enum LiveStep {
    Node(NodeRef),
    Value(Value),
}

fn slot_step(slot: Slot) -> LiveStep {
    match slot {
        Slot::Node(n) => LiveStep::Node(n),
        Slot::Value(v) => LiveStep::Value(v),
    }
}

impl LiveStep {
    /// This position, materialized in full -- a whole-subtree
    /// [`Store::snapshot`] for a [`LiveStep::Node`] (the one point this
    /// module's own live walk still pays [`super::live_ctx`]'s own
    /// cost, but only for the one subtree a render actually asked for,
    /// never an unrelated sibling), or the leaf [`Value`] itself,
    /// unchanged.
    fn materialize(self, store: &Store) -> Value {
        match self {
            LiveStep::Node(node) => store.snapshot(&node),
            LiveStep::Value(value) => value,
        }
    }
}

/// The value *chain*'s own top level resolves *first* to, by
/// [`super::live_ctx`]'s exact merge rule: `{**chain[0], **chain[1],
/// ..., **chain[-1]}` means a *later* entry's own top-level key wins
/// the **whole** value at that key, never a deeper per-key merge -- so
/// scanning *chain* from its last entry backward and stopping at the
/// first one whose own top level actually has *first* reproduces
/// [`super::live_ctx`]'s own result for that one key exactly, without
/// resolving any *other* top-level key at all. A [`CtxSource::Live`]
/// entry resolves through [`Store::peek`] (O(1), not a walk of anything
/// else at that node); a [`CtxSource::Frozen`] entry (already a plain
/// `Value` -- a tree's own one-time snapshot, DESIGN.md \u00a75.5) reads its
/// own top-level dict directly.
pub(crate) fn chain_first_segment(
    chain: &CtxChain,
    store: &Store,
    first: &str,
) -> Option<LiveStep> {
    let key = Value::Str(first.to_string());
    for source in chain.iter().rev() {
        match source {
            CtxSource::Live(node) => {
                if let Some(slot) = store.peek(node, &key) {
                    return Some(slot_step(slot));
                }
            }
            CtxSource::Frozen(Value::Dict(map)) => {
                if let Some(v) = map.get(&key) {
                    return Some(LiveStep::Value(v.clone()));
                }
            }
            CtxSource::Frozen(_) => {}
        }
    }
    None
}

/// *step*'s own child named *part* -- another O(1) [`Store::peek`] for
/// as long as *step* is still a [`LiveStep::Node`], or
/// [`electricity_template::step`]'s own per-segment rule (dict by key,
/// list/str/bytes by Python-style index, a scalar probed with an
/// int-parseable segment raising) the moment it's a [`LiveStep::Value`]
/// leaf -- the same handoff [`chain_first_segment`] can already land
/// on for the *first* segment, continued one segment further.
fn live_child(store: &Store, step: &LiveStep, part: &str) -> Result<Option<LiveStep>, String> {
    match step {
        LiveStep::Node(node) => {
            let key = Value::Str(part.to_string());
            Ok(store.peek(node, &key).map(slot_step))
        }
        LiveStep::Value(value) => Ok(electricity_template::step(value, part)?.map(LiveStep::Value)),
    }
}

/// [`project_live`]'s own child step -- `electricity_cel::paths::
/// project`'s dict-only descent (a `state.`-path read never follows a
/// Mustache attribute, a list/str/bytes index, or a date/datetime
/// field: `electricity_cel::paths::first_unresolved` doesn't either),
/// continued against a [`LiveStep`] instead of a plain [`Value`]. Using
/// [`live_child`] here instead -- as this once did -- let a
/// [`LiveStep::Value`] leaf that happens to be a `Date`/`DateTime` step
/// past its own `year`/`month`/... fields, narrowing `state.x.d.year`
/// into a *resolved* projected state even though `project`/
/// `first_unresolved` against a full snapshot treat the same path as
/// unresolved (a `Date` isn't a `Value::Dict`) -- disagreeing with cof,
/// and with what this crate's own non-live `evaluate_condition` path
/// does for the exact same expression (electricity#549's parity
/// invariant).
fn project_live_child(store: &Store, step: &LiveStep, part: &str) -> Option<LiveStep> {
    match step {
        LiveStep::Node(node) => {
            let key = Value::Str(part.to_string());
            store.peek(node, &key).map(slot_step)
        }
        LiveStep::Value(Value::Dict(dict)) => dict
            .get(&Value::Str(part.to_string()))
            .cloned()
            .map(LiveStep::Value),
        LiveStep::Value(_) => None,
    }
}

/// A [`CtxChain`] plus the [`Store`] it chains into, as a lazily
/// resolved `ctx`/`state` root -- see this module's own doc comment.
/// Cheap to construct and to pass by reference: two borrows, no store
/// walk of its own until something actually calls [`ScopeRoot::lookup`]/
/// [`ScopeRoot::whole`]/[`ParamsCtx::resolve_from`] on it.
#[derive(Clone, Copy)]
pub(crate) struct LiveRoot<'a> {
    pub(crate) chain: &'a CtxChain,
    pub(crate) store: &'a Store,
}

impl<'a> LiveRoot<'a> {
    pub(crate) fn new(chain: &'a CtxChain, store: &'a Store) -> Self {
        LiveRoot { chain, store }
    }
}

impl electricity_template::ScopeRoot for LiveRoot<'_> {
    fn lookup(&self, key: &str) -> Result<Option<Value>, String> {
        let mut parts = key.split('.');
        // `str::split` on a non-empty pattern always yields at least
        // one item -- `key` is a template tag's own name, never empty
        // (the tokenizer rejects an empty `{{}}` before this is ever
        // reached).
        let first = parts.next().expect("split yields at least one segment");
        let Some(mut current) = chain_first_segment(self.chain, self.store, first) else {
            return Ok(None);
        };
        loop {
            match current {
                LiveStep::Node(node) => {
                    let Some(part) = parts.next() else {
                        return Ok(Some(self.store.snapshot(&node)));
                    };
                    let key = Value::Str(part.to_string());
                    match self.store.peek(&node, &key) {
                        Some(slot) => current = slot_step(slot),
                        None => return Ok(None),
                    }
                }
                LiveStep::Value(value) => {
                    let mut resolved = value;
                    for part in parts {
                        match electricity_template::step(&resolved, part)? {
                            Some(next) => resolved = next,
                            None => return Ok(None),
                        }
                    }
                    return Ok(Some(resolved));
                }
            }
        }
    }

    fn whole(&self) -> Value {
        live_ctx(self.chain, self.store)
    }

    /// Overrides the default (`lookup`-and-check) impl with a
    /// presence-only walk: `{{> name}}` composition's own
    /// effect-reference check (`crate::compose`, issue #487) only ever
    /// probes `prime.<head>` -- a node a named loop can have grown to
    /// thousands of passes underneath, so resolving all the way to a
    /// `Value` just to answer a yes/no question (the default impl's
    /// own `lookup` call, which for a [`LiveStep::Node`] would
    /// `materialize` -- a full [`Store::snapshot`] -- the whole
    /// subtree) would pay for exactly the cost this crate's own lazy
    /// rendering seam exists to avoid. Every step here stays either an
    /// O(1) [`Store::peek`] (while still inside the live tree) or
    /// [`electricity_template::step`]'s own per-segment rule (once a
    /// plain leaf [`Value`] is reached), never a `materialize` call.
    fn contains(&self, key: &str) -> bool {
        let mut parts = key.split('.');
        let first = match parts.next() {
            Some(first) => first,
            None => return false,
        };
        let Some(mut current) = chain_first_segment(self.chain, self.store, first) else {
            return false;
        };
        for part in parts {
            current = match live_child(self.store, &current, part) {
                Ok(Some(next)) => next,
                Ok(None) | Err(_) => return false,
            };
        }
        true
    }
}

impl ParamsCtx for LiveRoot<'_> {
    /// `core/use.py::_resolve_reference`'s own walk, against the live
    /// chain -- dict-by-key/list-by-Python-index only, no Mustache
    /// fallback and no error path (an unresolvable segment is just
    /// "not found", matching the plain-`Value` impl this mirrors).
    /// Descends live for as long as the path stays inside the store's
    /// own tracked tree, then switches to borrowed stepping through
    /// whatever single leaf [`Value`] it lands on (never re-cloning
    /// that leaf's own children segment by segment, the same
    /// clone-only-the-final-match discipline the plain-`Value` impl
    /// already keeps).
    fn resolve_from(&self, path: &str) -> Option<Value> {
        let mut parts = path.split('.');
        let first = parts.next()?;
        let mut current = chain_first_segment(self.chain, self.store, first)?;
        loop {
            match current {
                LiveStep::Node(node) => {
                    let Some(part) = parts.next() else {
                        return Some(self.store.snapshot(&node));
                    };
                    let key = Value::Str(part.to_string());
                    current = slot_step(self.store.peek(&node, &key)?);
                }
                LiveStep::Value(value) => {
                    let mut resolved = &value;
                    for part in parts {
                        resolved = match resolved {
                            Value::Dict(dict) => dict.get(&Value::Str(part.to_string()))?,
                            Value::List(items) => {
                                let index: i64 = part.parse().ok()?;
                                &items[python_list_index(items.len(), index)?]
                            }
                            _ => return None,
                        };
                    }
                    return Some(resolved.clone());
                }
            }
        }
    }
}

/// Python list indexing (negative indices count from the end) -- `None`
/// on any out-of-range index, never a panic. A second copy of
/// `params.rs`'s own helper of the same name/behaviour, kept local
/// rather than shared across a module boundary for a six-line function
/// (`params.rs`'s own comment on its copy makes the same call against
/// `electricity_tools::json`'s identical rule).
fn python_list_index(len: usize, index: i64) -> Option<usize> {
    let len = len as i64;
    let normalized = if index < 0 { index + len } else { index };
    if normalized < 0 || normalized >= len {
        None
    } else {
        Some(normalized as usize)
    }
}

/// A minimal `Value::Dict` carrying only *paths*' own subtrees, walked
/// live against *chain*/*store* instead of from an already-materialized
/// `Value` -- the store-backed counterpart of `electricity_cel::paths::
/// project` (ported line for line, down to the same shortest-path-first
/// insertion order and the same "stop, don't overwrite" rule once a
/// shorter path already placed a leaf a longer one tries to extend
/// past), except that `source`'s own descent happens through
/// [`chain_first_segment`]/[`project_live_child`] rather than a plain
/// `Value` dict-get -- and, like `project` itself, never through a
/// Mustache attribute, a list/str/bytes index, or a date/datetime field
/// (see [`project_live_child`]'s own doc comment). *paths* are raw,
/// `state.`-prefixed
/// ([`electricity_cel::condition_paths`]'s own shape) -- the leading
/// `state` segment is this function's own conceptual root (*chain*
/// itself), stripped the same way `project`'s own loop skips it.
pub(crate) fn project_live(store: &Store, chain: &CtxChain, paths: &[String]) -> Value {
    let mut unique: Vec<&str> = paths.iter().map(String::as_str).collect();
    unique.sort_by_key(|p| (p.matches('.').count(), *p));
    unique.dedup();

    let mut root: IndexMap<Value, Value> = IndexMap::new();
    for path in unique {
        let segments: Vec<&str> = path.split('.').skip(1).collect();
        let Some(first) = segments.first().copied() else {
            continue;
        };
        let Some(mut current) = chain_first_segment(chain, store, first) else {
            continue;
        };
        let mut target = &mut root;
        for (index, part) in segments.iter().enumerate() {
            if index > 0 {
                match project_live_child(store, &current, part) {
                    Some(next) => current = next,
                    None => break,
                }
            }
            let key = Value::Str(part.to_string());
            if index == segments.len() - 1 {
                target.insert(key, current.materialize(store));
                break;
            }
            match target.get(&key) {
                Some(Value::Dict(_)) => {}
                Some(_) => break,
                None => {
                    target.insert(key.clone(), Value::Dict(IndexMap::new()));
                }
            }
            let Some(Value::Dict(next_target)) = target.get_mut(&key) else {
                unreachable!("just inserted or confirmed to be a dict")
            };
            target = next_target;
        }
    }
    Value::Dict(root)
}

/// The `state` a CEL `if`/`while` condition should evaluate against --
/// the narrowed subset *expr* actually reads ([`project_live`]) when
/// that's decidable up front, or [`super::live_ctx`]'s own full
/// snapshot when it isn't (a parse failure here, reported properly by
/// the caller's own, subsequent real evaluation attempt; or a
/// `size(state)`-style whole-state read, which no path list could
/// narrow). **Not** also a tool/`use` `expect:`'s own `state` builder
/// -- see this module's own doc comment for why.
pub(crate) fn build_cel_state(chain: &CtxChain, store: &Store, expr: &str) -> Value {
    match electricity_cel::condition_paths(expr) {
        Ok(collected) if !collected.reads_whole_state => {
            project_live(store, chain, &collected.paths)
        }
        _ => live_ctx(chain, store),
    }
}

#[cfg(test)]
mod differential_tests {
    //! The REQUIRED differential test (issue #463's own lane notes): for
    //! a representative set of store shapes, templates, `{from:}` paths
    //! and CEL expressions, render/evaluate through the *old* path
    //! (`super::live_ctx`'s full snapshot, feeding `electricity_template::
    //! render_template`/`electricity_cel::evaluate_condition` exactly as
    //! every call site here did before this module existed) and the
    //! *new* one ([`LiveRoot`]/[`build_cel_state`]), and assert the two
    //! are identical -- output text, missing-key fallbacks, and error
    //! text alike. Every case below is chosen to exercise one of the
    //! parity risks the lane notes call out by name: the Mustache scope
    //! chain (section push/pop, inverted sections, a section over a
    //! list/dict/scalar), store aliases (`last` aliasing an `iter_<N>`
    //! sibling), a whole-subtree splice (key order/aliasing preserved
    //! exactly as `Store::snapshot` would render it), a layered overlay
    //! chain (a later entry's own top-level key winning outright, an
    //! earlier entry's key staying reachable when a later one doesn't
    //! redefine it), and a `state.`-path CEL read including a
    //! whole-state (`size(state)`) expression that must fall back to a
    //! full snapshot.

    use super::*;
    use crate::params::ParamsCtx;
    use chrono::NaiveDate;
    use electricity_template::{PlainCtx, ScopeRoot, render_template, render_template_with_root};
    use indexmap::IndexMap;
    use std::rc::Rc;

    fn k(s: &str) -> Value {
        Value::Str(s.to_string())
    }

    /// A loop-shaped store: `prime.a.b`, `prime.items` (a list),
    /// `prime.iter_0`/`prime.iter_1` (named-loop passes) with `last`
    /// aliased to `iter_1`, `prime.raw.nested.x` (a tool-output-shaped
    /// plain dict leaf nobody ever promoted), and a top-level `input`
    /// dict -- one store, reused by every case below.
    fn test_store() -> Store {
        let store = Store::new();
        let prime = store.ensure_dict(&store.root, k("prime")).unwrap();

        let a = store.ensure_dict(&prime, k("a")).unwrap();
        store.set_leaf(&a, k("b"), Value::from(42_i64));
        // Present but falsy, one level inside the `prime.a` section's
        // own pushed scope -- `zero` must resolve here and never fall
        // through to the root's own same-named, truthy `zero` below
        // (PR #482: Mustache's "first scope where the key
        // resolves wins, even falsy" rule, exercised across the
        // section/root boundary that actually reaches `ScopeRoot`).
        store.set_leaf(&a, k("zero"), Value::from(0_i64));

        // A path that resolves, but to `None` -- distinct from a
        // missing key for `first_unresolved`'s own "unresolved or
        // None" rule: a strict CEL read of this path must
        // raise the same way a missing one does.
        store.set_leaf(&prime, k("nullable"), Value::None);

        // A date leaf, one level under `prime` -- `project`/
        // `first_unresolved` only ever descend through `Value::Dict`,
        // so a CEL path reading one of this date's own named fields
        // (`prime.today.year`) must come out *unresolved*, never
        // narrowed into a resolved int the way a Mustache tag's own
        // `getattr` fallback would (PR #551's review).
        store.set_leaf(
            &prime,
            k("today"),
            Value::Date(NaiveDate::from_ymd_opt(2026, 10, 14).unwrap()),
        );

        store.set_leaf(
            &prime,
            k("items"),
            Value::List(vec![
                Value::from(1_i64),
                Value::from(2_i64),
                Value::from(3_i64),
            ]),
        );

        let iter0 = store.ensure_dict(&prime, k("iter_0")).unwrap();
        store.set_leaf(&iter0, k("value"), Value::from("first"));
        let iter1 = store.ensure_dict(&prime, k("iter_1")).unwrap();
        store.set_leaf(&iter1, k("value"), Value::from("second"));
        store.alias(&prime, k("last"), &k("iter_1")).unwrap();

        let mut nested = IndexMap::new();
        nested.insert(k("x"), Value::from(7_i64));
        let mut raw = IndexMap::new();
        raw.insert(k("nested"), Value::Dict(nested));
        store.set_leaf(&prime, k("raw"), Value::Dict(raw));

        let mut input = IndexMap::new();
        input.insert(k("n"), Value::from(5_i64));
        input.insert(k("s"), Value::from("hi"));
        store.set_leaf(&store.root, k("input"), Value::Dict(input));

        // Truthy, same name as `prime.a`'s own falsy `zero` above --
        // only reachable here if the Mustache fallback wrongly skipped
        // the section's own resolving-but-falsy scope.
        store.set_leaf(&store.root, k("zero"), Value::from(1_i64));

        store
    }

    /// A plain single-entry chain -- the whole store, live, no overlay.
    fn plain_chain(store: &Store) -> CtxChain {
        vec![CtxSource::Live(Rc::clone(&store.root))]
    }

    /// A layered overlay chain: the base store, plus a second, detached
    /// `Live` entry (never attached to *store*'s own tree, exactly the
    /// shape `exec::conditional`'s own `build_overlay`/`exec::loop_`'s
    /// own `plain_overlay` produce) whose own top level carries `iter`
    /// (a fresh key the base chain entry doesn't have at all) and, only
    /// when *override_prime* is set, a *different* `prime` dict that
    /// must win outright over the base's own -- `super::live_ctx`'s
    /// merge rule is a later entry's *whole* top-level key replacing an
    /// earlier one's, never a per-key-inside-`prime` merge.
    fn overlay_chain(store: &Store, override_prime: bool) -> CtxChain {
        let mut overlay = IndexMap::new();
        let mut iter_map = IndexMap::new();
        iter_map.insert(k("index"), Value::from(3_i64));
        overlay.insert(k("iter"), Slot::Value(Value::Dict(iter_map)));
        if override_prime {
            let mut replacement = IndexMap::new();
            replacement.insert(k("only_here"), Value::from("overlay"));
            overlay.insert(k("prime"), Slot::Value(Value::Dict(replacement)));
        }
        vec![
            CtxSource::Live(Rc::clone(&store.root)),
            CtxSource::Live(crate::node_from_entries(overlay)),
        ]
    }

    /// A chain whose own *second* entry is [`CtxSource::Frozen`] --
    /// the one-time `dict(ctx)` a tree dispatch snapshots once before
    /// its branches run (DESIGN.md §5.5), the only source a real run
    /// ever puts in a chain besides `Live` -- carrying a fresh `iter`
    /// key, the same way [`overlay_chain`]'s own `Live` detached entry
    /// does, so the two sources are interchangeable from a caller's
    /// point of view and [`chain_first_segment`]'s own `Frozen` arm
    /// (otherwise untested) gets exercised here.
    fn frozen_chain(store: &Store) -> CtxChain {
        let mut frozen = IndexMap::new();
        let mut iter_map = IndexMap::new();
        iter_map.insert(k("index"), Value::from(9_i64));
        frozen.insert(k("iter"), Value::Dict(iter_map));
        vec![
            CtxSource::Live(Rc::clone(&store.root)),
            CtxSource::Frozen(Value::Dict(frozen)),
        ]
    }

    /// Renders *template* both ways and asserts byte-identical output
    /// (`Err` text included).
    fn assert_same_render(chain: &CtxChain, store: &Store, template: &str, case: &str) {
        let old_ctx = live_ctx(chain, store);
        let old =
            render_template(template, &old_ctx, &PlainCtx, "template").map_err(|e| e.to_string());

        let live_root = LiveRoot::new(chain, store);
        let new = render_template_with_root(template, &live_root, &PlainCtx, "template")
            .map_err(|e| e.to_string());

        assert_eq!(old, new, "{case}: template {template:?} diverged");
    }

    /// Resolves a `{from: ...}` *path* both ways and asserts identical
    /// results.
    /// [`ScopeRoot::contains`]'s own default impl (a plain [`Value`]'s
    /// `lookup`-and-check) against [`LiveRoot`]'s own presence-only
    /// override.
    fn assert_same_contains(chain: &CtxChain, store: &Store, path: &str, case: &str) {
        let old_ctx = live_ctx(chain, store);
        let old = old_ctx.contains(path);

        let live_root = LiveRoot::new(chain, store);
        let new = live_root.contains(path);

        assert_eq!(old, new, "{case}: contains({path:?}) diverged");
    }

    fn assert_same_resolve(chain: &CtxChain, store: &Store, path: &str, case: &str) {
        let old_ctx = live_ctx(chain, store);
        let old = old_ctx.resolve_from(path);

        let live_root = LiveRoot::new(chain, store);
        let new = live_root.resolve_from(path);

        assert_eq!(old, new, "{case}: {{from: {path}}} diverged");
    }

    /// Evaluates a CEL *expr* both ways and asserts an identical
    /// boolean verdict (`Err` text included) -- *strict* matches
    /// `evaluate_condition`'s own `strict:` flag (lane notes: must be
    /// exercised `true` as well as `false`).
    fn assert_same_condition(
        chain: &CtxChain,
        store: &Store,
        expr: &str,
        strict: bool,
        case: &str,
    ) {
        let old_state = live_ctx(chain, store);
        let old = electricity_cel::evaluate_condition(expr, &old_state, strict)
            .map_err(|e| e.to_string());

        let new_state = build_cel_state(chain, store, expr);
        let new = electricity_cel::evaluate_condition(expr, &new_state, strict)
            .map_err(|e| e.to_string());

        assert_eq!(old, new, "{case}: CEL {expr:?} (strict={strict}) diverged");
    }

    const TEMPLATES: &[&str] = &[
        "{{input.n}}",
        "{{input.s}}",
        "{{prime.a.b}}",
        "{{prime.items.1}}",
        "{{prime.items.-1}}",
        "{{prime.last.value}}",
        "{{prime.iter_0.value}}",
        "{{prime.raw.nested.x}}",
        "{{missing.key}}",
        "{{{prime.raw}}}",
        "{{#prime.items}}[{{.}}]{{/prime.items}}",
        "{{^missing}}none{{/missing}}",
        "{{#prime.a}}{{b}}{{/prime.a}}",
        ".",
        "{{.}}",
        // A name not in the section's own scope at all (`prime.a` has
        // no `input` key) -- the fallback has to reach all the way
        // back to the root scope, which is `ScopeRoot::lookup`'s own
        // multi-segment path, not `walk_dotted` on an already-pushed
        // `Value`.
        "{{#prime.a}}{{input.n}}{{/prime.a}}",
        // A section over a truthy scalar: pushes `42` itself as the new
        // nearest scope, so `{{.}}` resolves to it, not to `prime.a.b`'s
        // enclosing dict ("a section over a scalar").
        "{{#prime.a.b}}{{.}}{{/prime.a.b}}",
        // `prime.a.zero` (0, falsy) must win over the root's own `zero`
        // (1, truthy) -- the nearest *resolving* scope wins even falsy,
        // never the nearest *truthy* one.
        "{{#prime.a}}{{zero}}{{/prime.a}}",
        // `input.n` is `5`, an int: indexing it like a sequence raises,
        // on both paths alike (an error-text case).
        "{{input.n.0}}",
    ];

    const FROM_PATHS: &[&str] = &[
        "input.n",
        "prime.a.b",
        "prime.items.1",
        "prime.items.-1",
        "prime.last.value",
        "prime.iter_0.value",
        "prime.raw.nested.x",
        "missing.key",
        "prime",
    ];

    /// [`ScopeRoot::contains`]'s own override on [`LiveRoot`] (issue
    /// #487's `{{> name}}` composition presence check) -- a mix of
    /// hits (including a present-but-`None` leaf, which must still
    /// count as present) and misses, at both a top-level segment and a
    /// deeper dotted one.
    const CONTAINS_PATHS: &[&str] = &[
        "prime",
        "prime.a",
        "prime.a.b",
        "prime.nullable",
        "prime.iter_0.value",
        "missing",
        "prime.missing",
        "prime.a.missing",
    ];

    const CONDITIONS: &[&str] = &[
        "state.input.n == 5",
        "state.prime.a.b == 42",
        "state.prime.last.value == 'second'",
        "state.prime.items[1] == 2",
        "has(state.missing.key)",
        "state.missing.key == 1",
        "size(state) >= 0",
        // `prime.today` is a `Value::Date`, never a `Value::Dict` --
        // `state.prime.today.year` must read as unresolved (false,
        // non-strict) on both paths alike, never narrowed by the live
        // path into a resolved `2026` the way a Mustache tag's own
        // `getattr` fallback would.
        "state.prime.today.year == 2026",
        "has(state.prime.today.year)",
    ];

    /// Exercised with `strict: true` -- `evaluate_condition`'s `strict`
    /// flag turns an unresolved `state.`-path read into an `Err` rather
    /// than a logged, false result -- must be covered as well
    /// as the `strict: false` default [`CONDITIONS`] above uses).
    const CONDITIONS_STRICT: &[&str] = &[
        // Resolves, so strict has nothing to raise over -- a passing
        // control case alongside the two that do raise below.
        "state.prime.a.b == 42",
        // Present, but `None` -- `first_unresolved`'s own "unresolved or
        // None" rule treats this exactly like a missing key, so strict
        // raises here too ("a value that resolves to `None`
        // under a strict condition").
        "state.prime.nullable == null",
        // A plain missing key, strict's ordinary case.
        "state.missing.key == 1",
        // A date's own field, which the attribute fallback never
        // resolves in a `state.`-path read -- strict
        // raises here too, on both paths alike.
        "state.prime.today.year == 2026",
    ];

    #[test]
    fn templates_match_against_a_plain_chain() {
        let store = test_store();
        let chain = plain_chain(&store);
        for template in TEMPLATES {
            assert_same_render(&chain, &store, template, "plain chain");
        }
    }

    #[test]
    fn templates_match_against_a_frozen_chain() {
        let store = test_store();
        let chain = frozen_chain(&store);
        for template in TEMPLATES {
            assert_same_render(&chain, &store, template, "frozen chain");
        }
        assert_same_render(&chain, &store, "{{iter.index}}", "frozen chain");
    }

    #[test]
    fn templates_match_against_an_overlay_chain_without_a_prime_override() {
        let store = test_store();
        let chain = overlay_chain(&store, false);
        for template in TEMPLATES {
            assert_same_render(&chain, &store, template, "overlay, no prime override");
        }
        assert_same_render(
            &chain,
            &store,
            "{{iter.index}}",
            "overlay, no prime override",
        );
    }

    #[test]
    fn templates_match_against_an_overlay_chain_with_a_prime_override() {
        let store = test_store();
        let chain = overlay_chain(&store, true);
        // The overlay's own `prime` replaces the base's outright -- the
        // base's `prime.a.b` must *not* leak through, on both paths
        // alike.
        assert_same_render(&chain, &store, "{{prime.a.b}}", "overlay, prime override");
        assert_same_render(
            &chain,
            &store,
            "{{prime.only_here}}",
            "overlay, prime override",
        );
        assert_same_render(&chain, &store, "{{iter.index}}", "overlay, prime override");
    }

    #[test]
    fn from_paths_match_against_a_plain_chain() {
        let store = test_store();
        let chain = plain_chain(&store);
        for path in FROM_PATHS {
            assert_same_resolve(&chain, &store, path, "plain chain");
        }
    }

    #[test]
    fn from_paths_match_against_an_overlay_chain() {
        let store = test_store();
        let chain = overlay_chain(&store, true);
        for path in FROM_PATHS {
            assert_same_resolve(&chain, &store, path, "overlay, prime override");
        }
        assert_same_resolve(&chain, &store, "prime.only_here", "overlay, prime override");
    }

    #[test]
    fn from_paths_match_against_a_frozen_chain() {
        let store = test_store();
        let chain = frozen_chain(&store);
        for path in FROM_PATHS {
            assert_same_resolve(&chain, &store, path, "frozen chain");
        }
        assert_same_resolve(&chain, &store, "iter.index", "frozen chain");
    }

    #[test]
    fn contains_matches_against_a_plain_chain() {
        let store = test_store();
        let chain = plain_chain(&store);
        for path in CONTAINS_PATHS {
            assert_same_contains(&chain, &store, path, "plain chain");
        }
    }

    #[test]
    fn contains_matches_against_an_overlay_chain() {
        let store = test_store();
        let chain = overlay_chain(&store, true);
        for path in CONTAINS_PATHS {
            assert_same_contains(&chain, &store, path, "overlay, prime override");
        }
        assert_same_contains(&chain, &store, "prime.only_here", "overlay, prime override");
        assert_same_contains(&chain, &store, "iter.index", "overlay, prime override");
    }

    #[test]
    fn contains_matches_against_a_frozen_chain() {
        let store = test_store();
        let chain = frozen_chain(&store);
        for path in CONTAINS_PATHS {
            assert_same_contains(&chain, &store, path, "frozen chain");
        }
        assert_same_contains(&chain, &store, "iter.index", "frozen chain");
    }

    #[test]
    fn conditions_match_against_a_plain_chain() {
        let store = test_store();
        let chain = plain_chain(&store);
        for expr in CONDITIONS {
            assert_same_condition(&chain, &store, expr, false, "plain chain");
        }
    }

    #[test]
    fn conditions_match_against_an_overlay_chain() {
        let store = test_store();
        let chain = overlay_chain(&store, true);
        for expr in CONDITIONS {
            assert_same_condition(&chain, &store, expr, false, "overlay, prime override");
        }
        assert_same_condition(
            &chain,
            &store,
            "state.iter.index == 3",
            false,
            "overlay, prime override",
        );
    }

    #[test]
    fn conditions_match_against_a_frozen_chain() {
        let store = test_store();
        let chain = frozen_chain(&store);
        for expr in CONDITIONS {
            assert_same_condition(&chain, &store, expr, false, "frozen chain");
        }
        assert_same_condition(
            &chain,
            &store,
            "state.iter.index == 9",
            false,
            "frozen chain",
        );
    }

    #[test]
    fn conditions_match_under_strict_mode() {
        let store = test_store();
        let chain = plain_chain(&store);
        for expr in CONDITIONS_STRICT {
            assert_same_condition(&chain, &store, expr, true, "plain chain, strict");
        }
    }
}
