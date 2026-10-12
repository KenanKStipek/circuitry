//! `core/effect_identity.py`'s `call_path`/`nested_container`, ported as
//! a cheap-to-clone value rather than a `contextvars.ContextVar` (issue
//! #449's gate lane items 4/6; review findings 2/8 on PR #454) --
//! threaded by [`crate::exec::execute_op`] and every container executor
//! (including every still-stubbed one, down to the two `condition_model`
//! stubs) so lane G/G2/H/I's own `prompt`/`loop`/`use` never has to
//! widen one of those shared signatures to carry one.
//!
//! [`Frame::effect_path`] is the one path every observer hook and
//! [`crate::adapter::CallContext::path`] both read -- `effect_identity.py`'s
//! own docstring says the two always coincide.
//!
//! Unlike Python, electricity-compiler already bakes a globally
//! absolute [`EffectPath`] into every compiled `Op` (DESIGN.md's own
//! "stable effect-path ID" -- `electricity-bytecode::path`'s own module
//! doc comment), rather than deriving one at run time from a live,
//! possibly-isolated `Store`'s own path prefix the way Python's
//! `store.effect_path`/`own_path` do. That means [`Frame::nested`] (the
//! `nested_container` prefix push) is only ever needed where Python's
//! *reason* for it still applies in this architecture too: a `use`/
//! decomposition child compiles as its own, independent [`electricity_bytecode::Program`],
//! whose own root starts fresh at `prime` with no idea where its parent
//! placed it. An ordinary named `dynamic`/`if`/loop-pass child needs no
//! prefix push at all -- its own compiled [`EffectPath`] is already
//! globally unique (a `dynamic`'s tree branches are named distinctly by
//! the document itself; a loop pass's own [`electricity_bytecode::LoopId`]
//! placeholder already disambiguates every concurrent instance of one
//! shared body once [`Frame::with_pass`] resolves it) -- so
//! [`Frame::root`]'s own empty prefix and empty pass map make
//! [`Frame::effect_path`] return exactly *path* unchanged for the whole
//! of any M0-shaped run (no `use`, no `loop`), keeping `--events`
//! output byte-identical until a lane actually calls [`Frame::nested`]/
//! [`Frame::with_pass`].

use electricity_bytecode::{EffectPath, LoopId};
use std::collections::HashMap;
use std::rc::Rc;

/// Per-call context threaded alongside [`crate::exec::CtxChain`] --
/// cheap to clone (an `Rc` bump, plus, at most, one new small map/`Vec`
/// on the rare branch that actually extends one) so every container
/// executor can hand a child its own extended [`Frame`] without any
/// ancestor's own copy changing underneath it. That matters because a
/// *shared*, mutated-in-place call stack is exactly the false-cycle
/// hazard `core/use.py:856-871` describes for concurrent tree/loop
/// branches: two sibling branches must each see the use-call-stack (and
/// pass-index map) as it stood when *they* branched off, never a
/// sibling's own later push.
#[derive(Clone)]
pub struct Frame {
    /// `core/effect_identity.py::_PATH_PREFIX` -- the empty path
    /// (`EffectPath(vec![])`) outside a `use`/decomposition child,
    /// otherwise that child's own absolute path in the *parent*
    /// document (already stripped of the child's own `prime` root by
    /// the [`Frame::nested`] call that built this frame).
    prefix: Rc<EffectPath>,
    /// Every [`LoopId`] pass placeholder resolved so far, for
    /// [`EffectPath::resolve_passes`] -- extended, never mutated in
    /// place, by [`Frame::with_pass`].
    pass_indices: Rc<HashMap<LoopId, u32>>,
    /// `core/use.py`'s own `use_call_stack` -- *document identities*
    /// on the call stack (`core/use.py::_load_child_orch`'s own
    /// *identity*: a path-`use`'s resolved absolute path, an inline
    /// `use`'s own inline-document form -- never the `use` effect's own
    /// *name*, which two unrelated `use` effects can share without
    /// that being a cycle, and which the same child document reached
    /// through two different `use` effects would wrongly miss), one
    /// per level, extended by [`Frame::push_use`], for lane I's own
    /// cycle detection (M1 gate seam round 2, item S6). A path-`use`'s
    /// own identity is its resolved absolute path, verbatim; an inline
    /// `use`'s own identity (review P2-7 on PR #454's second review,
    /// documenting `core/use.py::_load_child_orch`'s own exact form)
    /// is `format!("inline:{:016x}", ...)`-shaped but not literally
    /// that macro call -- `"inline:"` plus the first 16 **hex
    /// characters** (not bytes) of the SHA-256 hex digest of the
    /// rendered, fence-cleaned YAML text, UTF-8 encoded, lowercase
    /// (Python's own `hashlib.sha256(cleaned.encode("utf-8")).hexdigest()[:16]`).
    use_call_stack: Rc<Vec<String>>,
    /// A reflector's own inner-dynamic path rewrite (issue #506's lane
    /// L1), set by [`Frame::rebased`] -- `None` for every frame outside
    /// a reflector's inner dynamic. `(from, to)`, both already resolved
    /// against this frame's own [`Frame::pass_indices`] at the point
    /// [`Frame::rebased`] was called (never re-resolved per lookup): a
    /// path [`Frame::effect_path`] is asked to resolve that starts with
    /// *from* gets that prefix replaced by *to* instead, right after
    /// its own placeholders resolve and before this frame's own
    /// [`Frame::prefix`] composes on top -- see [`Frame::rebased`]'s
    /// own doc comment for why this composes correctly with both
    /// [`Frame::nested`] and [`Frame::with_pass`].
    rebase: Option<Rc<(EffectPath, EffectPath)>>,
}

impl Frame {
    /// The frame a run's own root `dynamic` starts under -- no
    /// `use`/decomposition nesting, no loop pass resolved yet.
    pub fn root() -> Self {
        Frame {
            prefix: Rc::new(EffectPath(Vec::new())),
            pass_indices: Rc::new(HashMap::new()),
            use_call_stack: Rc::new(Vec::new()),
            rebase: None,
        }
    }

    /// `core/effect_identity.py::call_path` -- *path*'s own pass
    /// placeholders resolved against this frame's own pass indices
    /// ([`EffectPath::resolve_passes`]), with the innermost
    /// `nested_container` prefix (if any) composed on in front, *path*'s
    /// own leading `prime` segment dropped first exactly as
    /// `_without_child_root` drops it. Feeds both `--events`'s own
    /// `path`/`dispatch` fields and [`crate::adapter::CallContext::path`]
    /// -- `effect_identity.py`'s own docstring: the two always
    /// coincide.
    ///
    /// `resolve_passes` fails only when *path* still carries a
    /// [`electricity_bytecode::PathSegment::Pass`] placeholder this
    /// frame's own [`Frame::pass_indices`] has no entry for yet --
    /// every real caller resolves a loop's own body path through
    /// [`Frame::with_pass`] before ever asking this frame for one of
    /// its paths, so that can only mean a caller bug (review P2-7 on
    /// PR #454's second review: this used to fall back to *path*
    /// silently, placeholder segment and all, which would have
    /// shipped a malformed `--events` path instead of catching the
    /// bug that produced it). The `debug_assert!` below catches that
    /// in every debug build (every test in this workspace included);
    /// a release build still falls back to the unresolved *path*
    /// rather than panicking a real run over a cosmetic one.
    pub fn effect_path(&self, path: &EffectPath) -> EffectPath {
        let resolved = path
            .resolve_passes(&self.pass_indices)
            .unwrap_or_else(|loop_id| {
                debug_assert!(
                    false,
                    "{path}: no pass index resolved yet for {loop_id:?} -- every caller must \
                 resolve a loop's own body path through Frame::with_pass before asking \
                 this frame for one of its paths"
                );
                path.clone()
            });
        let resolved = self.apply_rebase(resolved);
        if self.prefix.0.is_empty() {
            return resolved;
        }
        let mut segments = self.prefix.0.clone();
        segments.extend(resolved.without_child_root().0);
        EffectPath(segments)
    }

    /// [`Frame::rebase`]'s own prefix swap, applied to an already
    /// pass-resolved *path* -- a no-op (returns *path* unchanged) for
    /// every frame outside a reflector's inner dynamic, or for a path
    /// that doesn't start with the rebase's own *from*.
    fn apply_rebase(&self, path: EffectPath) -> EffectPath {
        let Some(rebase) = &self.rebase else {
            return path;
        };
        let (from, to) = rebase.as_ref();
        if path.0.len() >= from.0.len() && path.0[..from.0.len()] == from.0[..] {
            let mut segments = to.0.clone();
            segments.extend_from_slice(&path.0[from.0.len()..]);
            EffectPath(segments)
        } else {
            path
        }
    }

    /// A reflector's own inner-dynamic path rewrite (issue #506's lane
    /// L1): every path this frame is ever asked to resolve that starts
    /// with *from* -- both *from* and *to* given in the same,
    /// not-yet-prefixed coordinate space [`Frame::effect_path`]'s own
    /// *resolved* local reaches right after pass-placeholder
    /// resolution, before this frame's own [`Frame::prefix`] composes
    /// on top -- gets that prefix replaced by *to* instead. *from*/
    /// *to* are resolved against this frame's own [`Frame::pass_indices`]
    /// immediately, not deferred to each later [`Frame::effect_path`]
    /// call, so a reflector compiled inside a named loop body (whose
    /// own path, and so every inner child's, still carries a
    /// [`electricity_bytecode::PathSegment::Pass`] placeholder at this
    /// point) rebases correctly: the caller passes the placeholder-
    /// bearing `from`/`to` straight through (no need to resolve them
    /// itself first), and this method resolves both exactly once,
    /// against the loop pass [`Frame::with_pass`] already set on
    /// *self* before the reflector ever ran.
    ///
    /// Composes with both other builders because it never touches
    /// [`Frame::prefix`]/[`Frame::pass_indices`] at all, only adding
    /// its own [`Frame::rebase`] field on top of whatever this frame
    /// already carries: [`Frame::nested`] (a reflector inside a `use`
    /// child) still prefixes the *rebased* result afterward, in
    /// [`Frame::effect_path`]'s own order; [`Frame::with_pass`] (a
    /// reflector inside a named loop) has already resolved the loop's
    /// own placeholder into *this* frame's [`Frame::pass_indices`]
    /// before this call ever reads it.
    pub fn rebased(&self, from: &EffectPath, to: &EffectPath) -> Frame {
        let from_resolved = from
            .resolve_passes(&self.pass_indices)
            .unwrap_or_else(|_| from.clone());
        let to_resolved = to
            .resolve_passes(&self.pass_indices)
            .unwrap_or_else(|_| to.clone());
        Frame {
            prefix: Rc::clone(&self.prefix),
            pass_indices: Rc::clone(&self.pass_indices),
            use_call_stack: Rc::clone(&self.use_call_stack),
            rebase: Some(Rc::new((from_resolved, to_resolved))),
        }
    }

    /// `core/effect_identity.py::nested_container` -- the [`Frame`] a
    /// `use`/decomposition child's own freshly-compiled
    /// [`electricity_bytecode::Program`] runs under: its own prefix is
    /// *this* frame's [`Frame::effect_path`] of *path* (the `use`
    /// effect's own container path in the *parent* document), and its
    /// own [`Frame::pass_indices`] start over empty -- the child's own
    /// compiled paths carry no [`LoopId`] of the parent run's. The
    /// [`Frame::use_call_stack`] carries forward unchanged; a caller
    /// that is pushing a `use` effect's own name onto it chains
    /// [`Frame::push_use`] separately.
    pub fn nested(&self, path: &EffectPath) -> Frame {
        Frame {
            prefix: Rc::new(self.effect_path(path)),
            pass_indices: Rc::new(HashMap::new()),
            use_call_stack: Rc::clone(&self.use_call_stack),
            rebase: None,
        }
    }

    /// One more loop pass resolved -- no prefix push of its own
    /// (`core/effect_identity.py`'s own module doc comment: "a
    /// chain-flow loop pass needs no special handling here", and a
    /// `flow: tree` pass's own concurrent instances are already
    /// disambiguated by this same resolution once concretized, never by
    /// a [`Frame::nested`] prefix -- see this module's own doc comment).
    pub fn with_pass(&self, loop_id: LoopId, index: u32) -> Frame {
        let mut indices = (*self.pass_indices).clone();
        indices.insert(loop_id, index);
        Frame {
            prefix: Rc::clone(&self.prefix),
            pass_indices: Rc::new(indices),
            use_call_stack: Rc::clone(&self.use_call_stack),
            rebase: self.rebase.clone(),
        }
    }

    /// `core/use.py`'s own `parent_stack + [identity]` -- a fresh
    /// `Vec`, never the parent's own mutated in place (this module's
    /// own doc comment on why that would be wrong for a concurrent
    /// sibling branch). *identity* is the child document's own
    /// identity (this struct's own field doc comment), computed by the
    /// caller the same way `core/use.py::_load_child_orch` computes
    /// *identity* -- never the `use` effect's own name.
    pub fn push_use(&self, identity: String) -> Frame {
        let mut stack = (*self.use_call_stack).clone();
        stack.push(identity);
        Frame {
            prefix: Rc::clone(&self.prefix),
            pass_indices: Rc::clone(&self.pass_indices),
            use_call_stack: Rc::new(stack),
            rebase: self.rebase.clone(),
        }
    }

    /// Every child document identity currently on the call stack,
    /// outermost first -- lane I's own cycle check: a path-`use`'s
    /// resolved path or an inline `use`'s own inline form, never a
    /// `use` effect's name.
    pub fn use_call_stack(&self) -> &[String] {
        &self.use_call_stack
    }
}

impl Default for Frame {
    fn default() -> Self {
        Frame::root()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_bytecode::PathSegment;

    fn path(segments: &[&str]) -> EffectPath {
        EffectPath(
            segments
                .iter()
                .map(|s| PathSegment::Name(s.to_string()))
                .collect(),
        )
    }

    #[test]
    fn root_frame_leaves_an_ordinary_named_path_unchanged() {
        // Named container (`Store.child(name)`): a `dynamic`/`if` child's
        // own compiled path is already globally absolute, so the root
        // frame -- empty prefix, empty pass map -- must hand it back
        // byte-for-byte. This is the guarantee that keeps every M0
        // `--events` case identical: M0 never calls `Frame::nested`/
        // `Frame::with_pass`, so every path it ever resolves takes this
        // branch.
        let p = path(&["prime", "outer", "answer"]);
        assert_eq!(Frame::root().effect_path(&p), p);
    }

    #[test]
    fn with_pass_resolves_a_named_loop_pass() {
        // `loop.py:1398`'s own `store.child(iter_key)` -- a named loop's
        // pass placeholder resolves to `iter_<N>`, composed into the
        // path exactly where the placeholder sat, no prefix push
        // involved.
        let loop_id = LoopId(0);
        let mut p = EffectPath::root().push_name("shots");
        p = p.push_pass(loop_id).push_name("describe");
        let frame = Frame::root().with_pass(loop_id, 2);
        assert_eq!(
            frame.effect_path(&p).to_string(),
            "prime.shots.iter_2.describe"
        );
    }

    #[test]
    #[should_panic(expected = "no pass index resolved yet")]
    #[cfg(debug_assertions)]
    fn effect_path_with_no_pass_index_resolved_yet_panics_in_debug() {
        // Review P2-7 on PR #454's second review: a caller asking for a
        // loop body's own path before `Frame::with_pass` has resolved
        // that pass is a caller bug, not a legitimate path shape -- the
        // `debug_assert!` catches it here rather than silently handing
        // back a path with the placeholder segment still in it.
        let loop_id = LoopId(0);
        let p = EffectPath::root()
            .push_name("shots")
            .push_pass(loop_id)
            .push_name("describe");
        let _ = Frame::root().effect_path(&p);
    }

    #[test]
    fn with_pass_disambiguates_concurrent_tree_flow_passes() {
        // A `flow: tree` loop's own concurrent passes share one compiled
        // body (one `LoopId` placeholder for all of them) -- each
        // branch's own `Frame::with_pass` call (never `Frame::nested`)
        // is what tells two concurrently-dispatched passes' identically
        // *shaped* paths apart.
        let loop_id = LoopId(0);
        let p = EffectPath::root()
            .push_name("lp")
            .push_pass(loop_id)
            .push_name("step");
        let first = Frame::root().with_pass(loop_id, 0);
        let second = Frame::root().with_pass(loop_id, 1);
        assert_eq!(first.effect_path(&p).to_string(), "prime.lp.iter_0.step");
        assert_eq!(second.effect_path(&p).to_string(), "prime.lp.iter_1.step");
    }

    #[test]
    fn nested_composes_a_use_childs_own_root_path_onto_the_parent() {
        // `use`/decomposition child: the child's own compiled Program
        // starts fresh at `prime` -- `nested_container(store, use_name)`
        // with the child root stripped.
        let use_path = EffectPath::root().push_name("outer").push_name("fetch");
        let frame = Frame::root().nested(&use_path);
        let child_root = EffectPath::root();
        assert_eq!(
            frame.effect_path(&child_root).to_string(),
            "prime.outer.fetch"
        );
        let child_leaf = EffectPath::root().push_name("answer");
        assert_eq!(
            frame.effect_path(&child_leaf).to_string(),
            "prime.outer.fetch.answer"
        );
    }

    #[test]
    fn nested_composes_to_arbitrary_depth() {
        // A `use` child nested inside another `use` child: the
        // docstring's own "nesting composes to arbitrary depth" case.
        let outer_use = EffectPath::root().push_name("a");
        let frame1 = Frame::root().nested(&outer_use);
        let inner_use = EffectPath::root().push_name("b");
        let frame2 = frame1.nested(&inner_use);
        let leaf = EffectPath::root().push_name("answer");
        assert_eq!(frame2.effect_path(&leaf).to_string(), "prime.a.b.answer");
    }

    #[test]
    fn rebased_rewrites_the_matching_prefix() {
        // The reflector's own seam: an inner effect compiled at
        // `<refl>.<step>` (never `<refl>.inner.<step>` -- the compiler
        // never emits that segment) must report the run-time path
        // `<refl>.inner.<step>`.
        let refl = EffectPath::root().push_name("plan_it");
        let frame = Frame::root().rebased(&refl, &refl.push_name("inner"));
        let refl_own_path = refl.clone();
        assert_eq!(
            frame.effect_path(&refl_own_path).to_string(),
            "prime.plan_it.inner"
        );
        let step = refl.push_name("propose_steps");
        assert_eq!(
            frame.effect_path(&step).to_string(),
            "prime.plan_it.inner.propose_steps"
        );
    }

    #[test]
    fn rebased_leaves_an_unrelated_path_untouched() {
        let refl = EffectPath::root().push_name("plan_it");
        let frame = Frame::root().rebased(&refl, &refl.push_name("inner"));
        let sibling = EffectPath::root().push_name("other");
        assert_eq!(frame.effect_path(&sibling).to_string(), "prime.other");
    }

    #[test]
    fn rebased_composes_with_nested_a_reflector_inside_a_use_child() {
        // A reflector running inside a `use` child: the child's own
        // `Frame::nested` prefix must still land in front of the
        // rebased `inner` segment, in `effect_path`'s own order
        // (rebase first, prefix second).
        let use_path = EffectPath::root().push_name("outer").push_name("fetch");
        let child_frame = Frame::root().nested(&use_path);
        let refl = EffectPath::root().push_name("plan_it");
        let frame = child_frame.rebased(&refl, &refl.push_name("inner"));
        let step = refl.push_name("propose_steps");
        assert_eq!(
            frame.effect_path(&step).to_string(),
            "prime.outer.fetch.plan_it.inner.propose_steps"
        );
    }

    #[test]
    fn rebased_composes_with_with_pass_a_reflector_inside_a_named_loop() {
        // A reflector inside a named loop body: the loop's own pass
        // index must already be resolved (`Frame::with_pass`, applied
        // by the loop before the reflector ever runs) by the time
        // `rebased`'s own `from`/`to` -- which still carry the loop's
        // `Pass` placeholder -- are themselves resolved.
        let loop_id = LoopId(0);
        let refl = EffectPath::root()
            .push_name("shots")
            .push_pass(loop_id)
            .push_name("plan_it");
        let frame = Frame::root()
            .with_pass(loop_id, 2)
            .rebased(&refl, &refl.push_name("inner"));
        let step = refl.push_name("propose_steps");
        assert_eq!(
            frame.effect_path(&step).to_string(),
            "prime.shots.iter_2.plan_it.inner.propose_steps"
        );
    }

    #[test]
    fn transparent_unnamed_containers_need_no_new_segment() {
        // An unnamed `if`/`dynamic`/loop never gets its own `push_name`
        // at compile time (`electricity-compiler`'s own `own_path`
        // falls through to the parent's path unchanged) -- so there is
        // nothing for `Frame` to do here beyond the identity case
        // already covered above; this test only pins that an unnamed
        // child's path (indistinguishable, at this layer, from its
        // parent's) still round-trips.
        let parent = path(&["prime", "outer"]);
        assert_eq!(Frame::root().effect_path(&parent), parent);
    }

    #[test]
    fn push_use_extends_a_fresh_stack_not_the_parents_own() {
        let base = Frame::root();
        let child = base.push_use("child.yml".to_string());
        assert_eq!(base.use_call_stack(), &[] as &[String]);
        assert_eq!(child.use_call_stack(), &["child.yml".to_string()]);
    }

    #[test]
    fn two_sibling_tree_branches_using_the_same_child_see_no_false_cycle() {
        // S6 (M1 gate seam round 2): `core/use.py:856-871`'s own
        // concern -- a *shared*, mutated-in-place call stack would let
        // one tree-flow branch's own push become visible to a sibling
        // branch as a false ancestor. `Frame::push_use` instead extends
        // a fresh `Vec` per call, off the *same* parent frame, so two
        // sibling branches that each `use` the identical child document
        // never see each other's own push at all.
        let parent = Frame::root();
        let left = parent.push_use("child.yml".to_string());
        let right = parent.push_use("child.yml".to_string());
        assert_eq!(left.use_call_stack(), &["child.yml".to_string()]);
        assert_eq!(right.use_call_stack(), &["child.yml".to_string()]);
        assert_eq!(parent.use_call_stack(), &[] as &[String]);
        // Neither sibling's own identity shows up twice -- each one's
        // own stack has exactly the one push it made itself.
        assert_eq!(left.use_call_stack().len(), 1);
        assert_eq!(right.use_call_stack().len(), 1);
    }

    #[test]
    fn a_real_cycle_is_detected_by_the_identity_already_being_on_the_stack() {
        // A real cycle: the same child document reached a second time
        // on one call path (not two siblings -- one branch nesting into
        // itself). The identity lane I's own cycle check compares
        // against is already on `Frame::use_call_stack` by the time the
        // same child is about to be pushed again.
        let root = Frame::root();
        let one_level = root.push_use("a.yml".to_string());
        let two_levels = one_level.push_use("b.yml".to_string());
        let identity = "a.yml".to_string();
        assert!(two_levels.use_call_stack().contains(&identity));
    }
}
