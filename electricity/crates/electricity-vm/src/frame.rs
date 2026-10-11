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
    /// `core/use.py`'s own `use_call_stack` -- the name of every `use`
    /// effect on the call stack, extended per level by
    /// [`Frame::push_use`], for lane I's own cycle detection.
    use_call_stack: Rc<Vec<String>>,
}

impl Frame {
    /// The frame a run's own root `dynamic` starts under -- no
    /// `use`/decomposition nesting, no loop pass resolved yet.
    pub fn root() -> Self {
        Frame {
            prefix: Rc::new(EffectPath(Vec::new())),
            pass_indices: Rc::new(HashMap::new()),
            use_call_stack: Rc::new(Vec::new()),
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
    pub fn effect_path(&self, path: &EffectPath) -> EffectPath {
        let resolved = path
            .resolve_passes(&self.pass_indices)
            .unwrap_or_else(|_| path.clone());
        if self.prefix.0.is_empty() {
            return resolved;
        }
        let mut segments = self.prefix.0.clone();
        segments.extend(resolved.without_child_root().0);
        EffectPath(segments)
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
        }
    }

    /// `core/use.py`'s own `use_call_stack.append(name)` -- a fresh
    /// `Vec`, never the parent's own mutated in place (this module's own
    /// doc comment on why that would be wrong for a concurrent sibling
    /// branch).
    pub fn push_use(&self, name: String) -> Frame {
        let mut stack = (*self.use_call_stack).clone();
        stack.push(name);
        Frame {
            prefix: Rc::clone(&self.prefix),
            pass_indices: Rc::clone(&self.pass_indices),
            use_call_stack: Rc::new(stack),
        }
    }

    /// Every `use` effect name currently on the call stack, outermost
    /// first -- lane I's own cycle check.
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
        let child = base.push_use("fetch".to_string());
        assert_eq!(base.use_call_stack(), &[] as &[String]);
        assert_eq!(child.use_call_stack(), &["fetch".to_string()]);
    }
}
