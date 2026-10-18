//! Lane B/C: the per-container execution modules -- `dynamic` (chain/
//! tree), `conditional` (CEL `if`), and this module's own [`tool`]
//! (lane A's real `run_tool` dispatch seam).
//!
//! This module also hosts the handful of helpers every container
//! executor needs (the single-effect dispatcher [`execute_op`], the
//! `ctx`/override merge, the `meta.labels`/interrupt-text/error-wrap
//! conventions) -- shared, rather than duplicated, between
//! `dynamic.rs` and `conditional.rs`.

pub mod condition_model;
pub mod conditional;
pub mod disabled;
pub mod dynamic;
pub(crate) mod live_scope;
pub mod loop_;
pub mod prompt;
pub mod reflector;
pub mod report;
pub mod tool;
pub mod use_;
pub mod yield_;

use crate::{
    CancellationToken, Frame, NodeRef, RunContext, RunObserver, Store, StoreError, VmError,
};
use electricity_bytecode::{LeafKind, NodeKind, Op, Region};
use electricity_value::Value;
use indexmap::IndexMap;
use std::future::Future;
use std::pin::Pin;

/// A recursive async executor's own return type -- every container
/// executor in this crate recurses into its own children (a `dynamic`'s
/// body can hold another `dynamic`/`if`, an `if` branch can too), and an
/// `async fn` cannot call itself (or a sibling that calls it back)
/// without this indirection: a directly-recursive `async fn`'s own
/// future would have to contain itself, an infinite size the compiler
/// rejects. Boxing erases the size, at the cost of one heap allocation
/// per container frame -- `electricity-vm`'s test-tools-era frames are
/// shallow enough (an `if` inside a `dynamic` inside a `dynamic`, never
/// a deep recursive data structure) for that cost to be the right trade
/// against the alternative (a hand-written state machine per container
/// kind).
pub(crate) type BoxExecFuture<'a> = Pin<Box<dyn Future<Output = Result<(), VmError>> + 'a>>;

/// POSIX signal numbers `core/dynamic.py::_error_text` branches on --
/// matching [`crate::CancellationToken`]'s own doc comment (`SIGTERM`
/// 15, `SIGHUP` 1; `SIGINT` is 2, but never needs its own arm below --
/// see [`interrupted_text`]'s own comment).
const SIGTERM: i32 = 15;
const SIGHUP: i32 = 1;

/// Python's `core/dynamic.py::_error_text`'s own signal-name fallback --
/// the text an interrupted [`crate::VmError::Cancelled`] is reported as
/// (a dynamic's own `meta.error`, a run's own failure text) once a
/// caller resolves it through *token*. Every real call site in this
/// crate builds that text through this function, so the three wordings
/// here are the only ones any of them ever produce.
pub(crate) fn interrupted_text(token: &CancellationToken) -> String {
    match token.signum() {
        Some(SIGTERM) => "Interrupted (SIGTERM)".to_string(),
        Some(SIGHUP) => "Interrupted (SIGHUP)".to_string(),
        // `Some(SIGINT)`, and -- defensively, since this is only ever
        // called once `token.is_set()` is already known true -- any
        // other/missing signum too. Deliberately one arm, not two: a
        // dedicated `Some(SIGINT)` match next to this would just
        // repeat it (issue #431 review finding on PR #440).
        _ => "Interrupted (Ctrl-C/SIGINT)".to_string(),
    }
}

/// A plain `Value::Str` key -- shorthand for the many literal dict keys
/// (`"value"`, `"meta"`, `"created_at"`, ...) this crate's own meta
/// construction writes.
pub(crate) fn key(s: &str) -> Value {
    Value::Str(s.to_string())
}

/// [`crate::StoreError`] as a [`VmError`] -- every `Store` call in this
/// crate's own containers is one this crate's own code already proved
/// well-formed (a tracked parent, a target key that exists), so this
/// conversion exists only so `?` type-checks; it is never expected to
/// actually propagate in a document that compiled.
pub(crate) fn store_err(err: StoreError) -> VmError {
    VmError::Message(err.to_string())
}

/// *meta*'s own `completed_at`, Python-truthy -- `cli/runtime_shim.py::
/// _TotalsAccumulator.observe`'s own `meta.get("completed_at")` guard,
/// decided here, by every container, from whichever `Store`/`NodeRef`
/// it already has in scope at its own [`crate::RunObserver::
/// effect_complete`] call, and carried through that hook's own
/// *completed_at_is_truthy* parameter from there on (fix,
/// issue #506 lane L1's review round 2: `electricity` (lib)'s own
/// `Observer::effect_complete` used to re-derive this by walking the
/// *run's* own root store, segment by segment, from *path* alone --
/// which can never see a node still isolated in its own `use` child's
/// or tree branch's own `Store` at the exact moment its own
/// `effect_complete` fires, so it silently guessed `true` there
/// regardless, over-counting `effects_run` for a successful `reflector`
/// -- the one effect type whose own success never writes a
/// `completed_at` at all, issue #506's own bug-compatible rule --
/// inside either container shape). Every other site this crate ever
/// calls `effect_complete` from already writes a real `completed_at`
/// timestamp immediately beforehand on every exit path that reaches
/// that call at all, so this reads back `true` there every time; a
/// cancellation a container's own `except`-shaped guard lets fall
/// through to its own unconditional `effect_complete` call without
/// ever refreshing `completed_at` (`exec::conditional`'s own named
/// `if`) reads back `false` here instead of the old walk's own
/// store-local lucky guess.
pub(crate) fn completed_at_is_truthy(store: &Store, meta: &NodeRef) -> bool {
    match store.peek(meta, &key("completed_at")) {
        Some(crate::Slot::Value(v)) => v.as_str().is_some_and(|s| !s.is_empty()),
        _ => false,
    }
}

/// `dict(self.defn.labels) if self.defn.labels else None`
/// (`core/dynamic.py`/`core/conditional.py`) -- an absent *or* empty
/// `labels:` both read back as `meta.labels: null`, since an empty dict
/// is falsy in Python.
pub(crate) fn normalize_labels(labels: Option<&Value>) -> Value {
    match labels {
        Some(Value::Dict(map)) if !map.is_empty() => Value::Dict(map.clone()),
        _ => Value::None,
    }
}

/// The `ctx` a container's own children render/evaluate against --
/// `ctx = store.state if ctx_override is None else {**ctx_override,
/// **store.state}` (`core/dynamic.py::DynamicRuntime.execute`), where
/// *live* is this call's own fresh [`crate::Store::snapshot`] of
/// *store.state*'s Rust counterpart (recomputed by the caller
/// immediately before every use, never cached, so a chain's later
/// sibling sees an earlier one's writes the same way Python's live dict
/// reference does -- see `exec::dynamic`'s own module doc comment).
///
/// A chain entry is either [`CtxSource::Live`] -- re-snapshotted fresh
/// every time [`live_ctx`] is called, so a write anywhere underneath it
/// (including one made *after* this entry joined the chain, however
/// many `dynamic` levels down) is always visible -- or
/// [`CtxSource::Frozen`], a `Value` already materialized once and never
/// refreshed. `Frozen` is correct for exactly one case left in this
/// crate: a tree's own one-time `dict(ctx)` snapshot (DESIGN.md §5.5),
/// taken right before dispatch and shared by every branch, matching
/// Python's own single snapshot moment -- nothing else can mutate any
/// of a tree's own ancestors while its branches run (each writes into
/// its own isolated store until the merge), so there is nothing further
/// for a `Frozen` entry to miss.
///
/// An `if` branch's own per-step `scope_ctx` overlay (Quirk Q1) is
/// *not* a second `Frozen` case, even though it is also rebuilt fresh
/// from the branch's own *unmodified* starting chain on every single
/// step (never from the previous step's own overlay, exactly as
/// Python's `scope_ctx(base_ctx, local)` always re-reads `base_ctx`,
/// never the previous iteration's own rebuilt `ctx` -- see
/// `exec::conditional`'s own `build_overlay`). Unlike a tree, nothing
/// stops a branch's *own* later step from creating a container nested
/// two or more `dynamic` levels under an ancestor the overlay already
/// covers (`state.prime.<outer>.<sibling>.<grandchild>`) -- so the
/// overlay has to go on being live for everything underneath it, not
/// just the one level `scope_ctx` itself touches. `build_overlay`
/// builds it as a *detached* [`NodeRef`] (never attached to the real
/// store tree) whose own [`crate::store::Slot`]s are cloned, not
/// snapshotted: a [`crate::store::Slot::Node`] clone is an `Rc` clone of
/// the live node itself, so the overlay's own `prime` entry (and every
/// other key it carries forward from the chain it was built from) stays
/// exactly as live as Python's own `{**ctx, **local}` dict-literal
/// construction, which allocates a new top-level dict but never copies
/// what each of its *values* point to. It is pushed as a
/// [`CtxSource::Live`] entry, resolved through [`live_ctx`] exactly like
/// any other live node (a detached node is an ordinary `NodeRef` in
/// every other respect). This is the P0 "a nested container freezes its
/// own ancestors' state" finding on PR #440, found a second time after
/// an initial fix that gave the overlay a `Frozen(Value)` of its own --
/// which looked identical to the tree's case above, but was wrong for
/// exactly the reason the tree's case is safe: something *does* go on
/// mutating a branch's own ancestors (the branch's own later steps)
/// while this overlay is in use.
///
/// Either way, Python's own `ctx = store.state if ctx_override is None
/// else {**ctx_override, **store.state}` never actually copies a nested
/// dict -- the dict literal/spread only ever touches the *top* level,
/// so every nested value (in particular `ctx["prime"]`, however many
/// `dynamic` levels up it came from) is the exact same live object
/// `store.state` itself mutates -- so a Rust `Value`, once cloned out of
/// a [`Store::snapshot`], can never reproduce that by staying put: it
/// has to be rebuilt from the *live* [`NodeRef`] chain (or, for the
/// overlay, a detached one sharing the same live `Slot`s) at the moment
/// it is actually used instead.
#[derive(Clone)]
pub(crate) enum CtxSource {
    Live(NodeRef),
    Frozen(Value),
}

/// The override chain behind an as-yet-unmaterialized `ctx` -- see
/// [`CtxSource`]. Cloning a chain is a handful of `Rc`/`Value` clones
/// (cheap: an `Rc` clone is a refcount bump, and the whole chain is at
/// most as many entries deep as the document has `dynamic`/`if` levels
/// of nesting), never a store walk of its own.
pub(crate) type CtxChain = Vec<CtxSource>;

/// Collapses *chain* into the one `Value` CEL/template rendering (or a
/// further-nested `dynamic`'s own `ctx_override`) actually reads --
/// `{**chain[0], **chain[1], ..., **chain[-1]}` (`core/dynamic.py::
/// DynamicRuntime.execute`'s own repeated top-level dict-spread,
/// unrolled: see this module's own doc comment above), with every
/// [`CtxSource::Live`] entry's own [`Store::snapshot`] taken fresh right
/// here, not at whatever earlier moment that entry joined the chain.
///
/// Known limitation (issue #431 review, PR #440; #442; tracked as
/// #463): [`Store::snapshot`] deep-copies the *entire* subtree under a
/// `Live` entry's own node, so a call here is O(the live state
/// reachable from that node), not Python's O(1) live-reference read.
/// Quadratic in state size over a long chain of deeply nested
/// containers -- most visibly, a named loop's own accumulated
/// `iter_<N>` children, re-copied in full on every single render of
/// every later pass (lane H's own `loop_perf` test in `tests/
/// loop_perf.rs` measures this directly).
///
/// This function fixes only the one constant-factor waste that stays
/// strictly inside it: the old version called [`Store::snapshot`] and
/// then `.clone()`d every one of its top-level entries *again* while
/// folding them into *merged* -- doubling the cost of every entry not
/// already shared by `Rc`. Taking `resolved`'s own `Value::Dict` by
/// value and moving its entries into *merged* (`for (k, v) in map`,
/// `IndexMap`'s own `IntoIterator`) instead of cloning them a second
/// time removes that double-clone, with no behaviour change: every M0
/// case and every new loop case in this PR stays byte-identical
/// (the existing golden corpora, and loop's own conformance cases run
/// on both engines -- control flow like this is covered by running
/// real documents, not a golden-corpus replay; see this PR's own
/// confirmed ruling).
///
/// A `tool` leaf's own render and an `if`/`while` condition no longer
/// call this function at all (issue #463, PR #482): `exec::live_scope`
/// resolves both lazily, a dotted segment at a time, off the live
/// store itself. This function stays the real cost for every caller
/// that still needs it: a `use` effect's own input render/`expect:`
/// (`exec::use_`'s own call sites aren't narrowed either -- PR #482
/// review finding 2, left as a follow-up: a `use` effect's own retry
/// loop does resolve its `{from:}`/template inputs lazily now through
/// the same [`crate::exec::live_scope::LiveRoot`], but its `expect:`'s
/// own `state` argument still collapses through this function, exactly
/// like `exec::tool`'s does, below), `evaluate_expect`'s own `state`
/// argument for both leaf kinds (deliberately not narrowed --
/// `exec::live_scope`'s own module doc comment), a CEL expression this
/// crate can't collect static paths for (`exec::live_scope::
/// build_cel_state`'s own fallback), and a tree dispatch's one-time
/// `Frozen` snapshot (DESIGN.md §5.5, paid once per dispatch, already
/// cheap). `electricity_value::Value`'s `Dict`/`List` still hold
/// plain, non-shared children (`#[derive(Clone)]` is a full deep
/// clone) -- untouched by #463, which solved the loop case by never
/// reaching this function in the first place for the two call sites
/// that dominated it, rather than by making this function itself
/// cheaper to call.
pub(crate) fn live_ctx(chain: &CtxChain, store: &Store) -> Value {
    let mut merged: IndexMap<Value, Value> = IndexMap::new();
    for source in chain {
        let mut resolved = match source {
            CtxSource::Live(node) => store.snapshot(node),
            CtxSource::Frozen(value) => value.clone(),
        };
        // `Value` has a hand-written `Drop` impl (iterative dropping,
        // regardless of depth), so a plain `if let Value::Dict(map) =
        // resolved` (a by-value move of just that one field) is
        // `E0509`: Rust forbids partially moving a field out of a type
        // that implements `Drop`. `std::mem::take` through `&mut`
        // sidesteps that -- it swaps the map out for an empty one
        // (trivially dropped when *resolved* itself goes out of scope)
        // rather than destructuring *resolved* itself.
        if let Value::Dict(ref mut map) = resolved {
            for (k, v) in std::mem::take(map) {
                merged.insert(k, v);
            }
        }
    }
    Value::Dict(merged)
}

/// `core/dynamic.py::DynamicRuntime._effect_path` -- *container_name*
/// (this container's own name, never the full state path) plus *child*'s
/// own name, or just *container_name* alone for an unnamed child (an
/// unnamed `if` has no state node/path segment of its own).
pub(crate) fn effect_label(container_name: &str, child: &Op) -> String {
    match &child.name {
        Some(name) => format!("{container_name}.{name}"),
        None => container_name.to_string(),
    }
}

/// Wraps *err* with *label*'s own `"<label>: <message>"` prefix
/// (`core/dynamic.py`'s chain-flow wrap, `RuntimeError(f"{effect_path}:
/// {e}")`, where `effect_path` is [`effect_label`]'s own result, not a
/// full state path -- issue #431 review finding on PR #440: a nested
/// `dynamic`'s own wrap must read `"<its own name>.<child>"`, never
/// `"<child's full path>"`) -- except a cancellation, which Python's own
/// `except Exception` (never `BaseException`) never catches, so it
/// always propagates unwrapped here too.
pub(crate) fn wrap_effect_error(label: &str, err: VmError) -> VmError {
    if err.is_fatal() {
        return err;
    }
    match err {
        // A typed tool failure keeps its own `cause` through the wrap --
        // `message` grows the label prefix, and `py_class` becomes
        // `RuntimeError` (review finding 1 on PR #454: #442's own tree
        // text needs the error to survive a chain/tree wrap as a typed
        // `Raised`, not collapse to a bare `VmError::Message`; review
        // P2-1 on the second review: `core/dynamic.py`'s own `raise
        // RuntimeError(...) from e` means the *wrapping* exception is
        // always a `RuntimeError`, never `e`'s own class -- `cause_class`
        // remembers *e*'s own `py_class` for `VmError::displayed_class`'s
        // one-level unwrap, discarding `e`'s own `cause_class` rather
        // than propagating it further, so a doubly-wrapped error's own
        // displayed class is `RuntimeError` (the inner wrap), never the
        // original leaf failure's class two levels down).
        VmError::Raised {
            py_class,
            message,
            cause,
            ..
        } => VmError::Raised {
            py_class: electricity_tools::PyExc::RuntimeError,
            message: format!("{label}: {message}"),
            cause,
            cause_class: Some(py_class),
        },
        other => VmError::Message(format!("{label}: {other}")),
    }
}

/// Dispatches one compiled `Op` -- a `tool` leaf (through lane C's own
/// [`tool::execute_tool`]), a nested `dynamic`/`finally:`-wrapped
/// `dynamic` (through [`dynamic::execute_dynamic`]), or an `if`
/// (through [`conditional::execute_conditional`]). *parent* is the
/// `NodeRef` *op* itself writes under (`store.ensure_dict(parent,
/// op.name)` for a named effect; passed straight through for a
/// transparent, unnamed `if`). *ctx_chain* is the caller's own
/// not-yet-materialized rendering context (see [`CtxSource`]) -- a
/// `tool` leaf collapses it into a `Value` with [`live_ctx`] right
/// here, at the moment it is actually needed; a nested `dynamic` or
/// `if` child gets the chain itself, unresolved -- [`dynamic::
/// execute_dynamic`] extends it with its own enclosing scope (Quirk
/// Q1's re-merge), and [`conditional::execute_conditional`] resolves it
/// itself, fresh, for the condition and for every branch step (that
/// module's own doc comment: freezing it once here, before the branch
/// even starts, is the P0 "a nested container freezes its own
/// ancestors' state" finding on PR #440) -- exactly mirroring
/// `core/dynamic.py::_execute_effect`'s own per-effect-type dispatch,
/// where only a `dynamic`/`if` child ever receives `ctx` as an
/// `ctx_override`/live reference rather than a resolved value.
///
/// `prompt`/`use`/`yield`/`reflector`/`loop`/a `mode: model` `if` are all
/// refused before a real run ever starts (lane A's `first_unsupported`),
/// so reaching one here is a defect, not a reachable document shape --
/// reported as [`VmError::NotImplemented`] rather than panicking.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_op<'a>(
    op: &'a Op,
    store: &'a Store,
    parent: &'a NodeRef,
    ctx_chain: &'a CtxChain,
    frame: &'a Frame,
    run_ctx: &'a RunContext<'a>,
    observer: &'a dyn RunObserver,
    token: &'a CancellationToken,
) -> BoxExecFuture<'a> {
    Box::pin(async move {
        // Review P2-4 on PR #454's second review: wired now (`core/
        // dynamic.py::DynamicRuntime._execute_effect`'s own single
        // `is_enabled` check, before dispatching to any effect type,
        // leaf or container -- disabling a container disables its
        // whole subtree because nothing inside it ever gets this far).
        // `op.enabled` is `true` for every `Op` orchestration YAML
        // alone ever compiles to -- only a profile's own `effects.
        // <path>.enabled: false` (`electricity_compiler::overrides::
        // apply_effect_overrides`, lane J2, issue #518) ever sets it
        // `false`, so this branch stays unreachable for a run with no
        // `--profile` (or a profile with no `effects:` entries).
        //
        // Python's own order (`core/disabled.py::write_disabled_node`):
        // write the node first, *then* fire the balanced `effect_start`/
        // `effect_complete` pair around it, then `on_write`
        // (`store.write()`'s own hook here) -- never start-before-write,
        // unlike every other leaf/container's own hook pairing in this
        // crate, which always has real work to report *between* start
        // and complete.
        if disabled::skip_disabled(op, &Value::None) {
            if op.name.is_some() {
                let effect_path = frame.effect_path(&op.path);
                disabled::write_disabled_node(store, parent, op)?;
                // A report never fires `effect_start`/`effect_complete`,
                // disabled or not (`core/disabled.py::write_disabled_node`'s
                // own `fire_hooks=False` for a report) -- a disabled named
                // report still writes its skip node above, just as silently,
                // and still reaches `on_write` like any other disabled effect.
                let is_report = matches!(&op.kind, NodeKind::Leaf(leaf) if matches!(leaf.as_ref(), LeafKind::Report(_)));
                if !is_report {
                    let instance = observer.effect_start(&effect_path);
                    observer.effect_complete(&effect_path, instance, None, true);
                }
                observer.write();
            }
            return Ok(());
        }
        match &op.kind {
            NodeKind::Leaf(leaf) => match leaf.as_ref() {
                LeafKind::Tool(tool_op) => {
                    // Issue #463: no longer a `live_ctx` snapshot taken
                    // eagerly here -- `execute_tool`'s own render/
                    // `expect:` call sites resolve `live_scope::
                    // LiveRoot` lazily, the same way every other leaf
                    // kind below already receives `ctx_chain`
                    // unresolved.
                    let live_root = live_scope::LiveRoot::new(ctx_chain, store);
                    tool::execute_tool(
                        op, tool_op, store, parent, &live_root, frame, run_ctx, observer, token,
                    )
                    .await
                }
                LeafKind::Prompt(prompt_op) => {
                    prompt::execute_prompt(
                        op, prompt_op, store, parent, ctx_chain, frame, run_ctx, observer, token,
                    )
                    .await
                }
                LeafKind::Use(use_op) => {
                    use_::execute_use(
                        op, use_op, store, parent, ctx_chain, frame, run_ctx, observer, token,
                    )
                    .await
                }
                LeafKind::Reflector(reflector_op) => {
                    reflector::execute_reflector(
                        op,
                        reflector_op,
                        store,
                        parent,
                        ctx_chain,
                        frame,
                        run_ctx,
                        observer,
                        token,
                    )
                    .await
                }
                LeafKind::Yield(yield_op) => {
                    yield_::execute_yield(
                        op, yield_op, store, parent, ctx_chain, frame, run_ctx, observer, token,
                    )
                    .await
                }
                LeafKind::Report(report_op) => {
                    report::execute_report(
                        op, report_op, store, parent, ctx_chain, frame, run_ctx, observer,
                    )
                    .await;
                    Ok(())
                }
            },
            NodeKind::Control(region) => match region {
                Region::If { .. } => {
                    conditional::execute_conditional(
                        op, store, parent, ctx_chain, frame, run_ctx, observer, token,
                    )
                    .await
                }
                Region::Block { .. } | Region::Parallel { .. } | Region::TryFinally { .. } => {
                    // Issue #516 §4(c): every nested `dynamic` reached
                    // through this generic dispatch (the document root
                    // dynamic never goes through `execute_op` at all --
                    // `execute_root`/a `use` child's own root dynamic
                    // call `dynamic::execute_dynamic` directly) is one
                    // display level deeper than whatever ambient depth
                    // *frame* already carries.
                    let nested_frame = frame.with_display_depth(
                        crate::complexity::depth::dynamic_nested_dynamic_child(
                            frame.display_depth(),
                        ),
                    );
                    dynamic::execute_dynamic(
                        op,
                        store,
                        parent,
                        ctx_chain,
                        &nested_frame,
                        run_ctx,
                        observer,
                        token,
                    )
                    .await
                }
                Region::Loop {
                    // Not read here: `op.name` below is the same
                    // value (`compile_loop`'s own `validated_name`),
                    // and `execute_loop` already receives `op`.
                    name: _,
                    spec,
                    body,
                    flow,
                    max_concurrency,
                    max_iterations,
                    min_iterations,
                    collect,
                } => {
                    loop_::execute_loop(
                        op,
                        spec,
                        body,
                        *flow,
                        *max_concurrency,
                        *max_iterations,
                        *min_iterations,
                        collect.as_deref(),
                        store,
                        parent,
                        ctx_chain,
                        frame,
                        run_ctx,
                        observer,
                        token,
                    )
                    .await
                }
            },
        }
    })
}

/// `if self.defn.timeout_ms:` then `max(1, math.ceil(timeout_ms /
/// 1000))` (`core/tool.py:519-520`) -- a truthy `timeout_ms` (set and
/// non-zero, negative included) always wins over any other budget,
/// rounded up to whole seconds and floored at 1 so a negative or
/// sub-second value still times out quickly rather than falling back
/// to "no timeout set" (issue #536). Shared by `tool::resolve_timeout_
/// seconds` and `prompt::attempt_timeout_seconds`, the two sites that
/// read an effect's own `timeout_ms`.
pub(crate) fn effect_timeout_seconds(timeout_ms: i64) -> u32 {
    let quotient = timeout_ms / 1000;
    let remainder = timeout_ms % 1000;
    let ceil_seconds = if remainder > 0 {
        quotient + 1
    } else {
        quotient
    };
    ceil_seconds.clamp(1, i64::from(u32::MAX)) as u32
}

/// `core/timeout_ref.py::resolve_timeout_ms` (issue #564): an effect's
/// `timeout_ms` for one attempt -- a literal as written, a
/// `{from: <path>, default: <int>}` read from *ctx* now. A path that is
/// missing (or holds `null`) takes the `default`, or fails without one;
/// anything but a positive integer fails, naming the path. *effect_label*
/// is the failure text's prefix (`Tool effect 'name'`).
pub(crate) fn resolve_timeout_ms<R: crate::params::ParamsCtx>(
    timeout_ms: Option<&electricity_bytecode::effects::TimeoutMs>,
    ctx: &R,
    effect_label: &str,
) -> Result<Option<i64>, String> {
    use electricity_bytecode::effects::TimeoutMs;
    let from = match timeout_ms {
        None => return Ok(None),
        Some(TimeoutMs::Literal(ms)) => return Ok(Some(*ms)),
        Some(TimeoutMs::From(from)) => from,
    };
    let resolved = ctx.resolve_from(&from.path);
    match &resolved {
        None | Some(Value::None) => match from.default {
            Some(default) => Ok(Some(default)),
            None => Err(format!(
                "{effect_label}: timeout_ms '{{from: {}}}' did not resolve to a value and has no default.",
                from.path
            )),
        },
        // A positive integer too big for `i64` saturates, like a literal's.
        Some(Value::Int(ms)) if !ms.is_zero() && !ms.is_negative() => Ok(Some(match ms {
            electricity_value::int_value::IntValue::Small(n) => *n,
            electricity_value::int_value::IntValue::Big(_) => i64::MAX,
        })),
        Some(other) => Err(format!(
            "{effect_label}: timeout_ms '{{from: {}}}' resolved to {}; expected a positive integer.",
            from.path,
            other.py_repr()
        )),
    }
}

#[cfg(test)]
mod effect_timeout_seconds_tests {
    use super::effect_timeout_seconds;

    #[test]
    fn a_negative_timeout_ms_floors_to_one_second() {
        assert_eq!(effect_timeout_seconds(-5000), 1);
    }

    #[test]
    fn a_sub_second_positive_timeout_ms_rounds_up() {
        assert_eq!(effect_timeout_seconds(500), 1);
    }

    #[test]
    fn an_exact_whole_second_timeout_ms_is_unchanged() {
        assert_eq!(effect_timeout_seconds(5000), 5);
        assert_eq!(effect_timeout_seconds(5001), 6);
    }

    #[test]
    fn a_negative_sub_second_timeout_ms_still_floors_to_one() {
        assert_eq!(effect_timeout_seconds(-500), 1);
    }
}

#[cfg(test)]
mod resolve_timeout_ms_tests {
    use super::resolve_timeout_ms;
    use electricity_bytecode::effects::{TimeoutFrom, TimeoutMs};
    use electricity_value::{Dict, Value};

    fn from(path: &str, default: Option<i64>) -> Option<TimeoutMs> {
        Some(TimeoutMs::From(TimeoutFrom {
            path: path.to_string(),
            default,
        }))
    }

    fn ctx(ms: Value) -> Value {
        let mut input = Dict::new();
        input.insert(Value::from("ms"), ms);
        let mut root = Dict::new();
        root.insert(Value::from("input"), Value::Dict(input));
        Value::Dict(root)
    }

    #[test]
    fn no_timeout_and_a_literal_never_read_the_context() {
        assert_eq!(
            resolve_timeout_ms(None, &Value::None, "Tool effect 't'"),
            Ok(None)
        );
        assert_eq!(
            resolve_timeout_ms(
                Some(&TimeoutMs::Literal(-5000)),
                &Value::None,
                "Tool effect 't'"
            ),
            Ok(Some(-5000))
        );
    }

    #[test]
    fn a_positive_integer_is_read_from_state() {
        let timeout_ms = from("input.ms", Some(9));
        assert_eq!(
            resolve_timeout_ms(
                timeout_ms.as_ref(),
                &ctx(Value::from(2500)),
                "Tool effect 't'"
            ),
            Ok(Some(2500))
        );
    }

    #[test]
    fn a_missing_or_null_path_takes_the_default() {
        let timeout_ms = from("input.nope", Some(7000));
        assert_eq!(
            resolve_timeout_ms(timeout_ms.as_ref(), &ctx(Value::from(1)), "Tool effect 't'"),
            Ok(Some(7000))
        );
        let timeout_ms = from("input.ms", Some(7000));
        assert_eq!(
            resolve_timeout_ms(timeout_ms.as_ref(), &ctx(Value::None), "Tool effect 't'"),
            Ok(Some(7000))
        );
    }

    #[test]
    fn a_missing_path_without_a_default_names_the_path() {
        let timeout_ms = from("input.nope", None);
        assert_eq!(
            resolve_timeout_ms(
                timeout_ms.as_ref(),
                &ctx(Value::from(1)),
                "Prompt effect 'p'"
            ),
            Err(
                "Prompt effect 'p': timeout_ms '{from: input.nope}' did not resolve to a value \
                 and has no default."
                    .to_string()
            )
        );
    }

    #[test]
    fn anything_but_a_positive_integer_fails_naming_the_path_and_the_value() {
        let timeout_ms = from("input.ms", Some(7000));
        for (value, shown) in [
            (Value::from("5000"), "'5000'"),
            (Value::Float(2.5), "2.5"),
            (Value::Bool(true), "True"),
            (Value::from(0), "0"),
            (Value::from(-5000), "-5000"),
            (Value::List(vec![Value::from(1)]), "[1]"),
        ] {
            assert_eq!(
                resolve_timeout_ms(timeout_ms.as_ref(), &ctx(value), "Tool effect 't'"),
                Err(format!(
                    "Tool effect 't': timeout_ms '{{from: input.ms}}' resolved to {shown}; \
                     expected a positive integer."
                )),
            );
        }
    }

    #[test]
    fn an_integer_too_big_for_i64_saturates_like_a_literal() {
        let huge = electricity_value::IntValue::parse_decimal(&"9".repeat(30)).unwrap();
        let timeout_ms = from("input.ms", None);
        assert_eq!(
            resolve_timeout_ms(
                timeout_ms.as_ref(),
                &ctx(Value::Int(huge)),
                "Tool effect 't'"
            ),
            Ok(Some(i64::MAX))
        );
    }
}

#[cfg(test)]
mod wrap_effect_error_tests {
    use super::wrap_effect_error;
    use crate::VmError;
    use electricity_tools::{ErrorCause, PyExc};

    #[test]
    fn a_raised_failures_cause_survives_the_chain_wrap_but_its_class_becomes_runtime_error() {
        // Review finding 1 on PR #454: a chain/tree wrap must keep
        // `VmError::Raised` a `Raised`, not collapse to a bare
        // `VmError::Message` -- a nested `dynamic`'s own error-collection
        // point (`execute_chain`/`execute_tree`, both of which call this
        // function) is exactly where #442's tree text would otherwise
        // lose the real exception class entirely. Review P2-1 on the
        // second review: `core/dynamic.py`'s own `raise RuntimeError(...)
        // from e` means the *wrapping* exception's own class is always
        // `RuntimeError`, never `e`'s class -- that survives one level
        // in as `cause_class` instead, for `VmError::displayed_class`.
        let raised = VmError::Raised {
            py_class: PyExc::ValueError,
            message: "no reply".to_string(),
            cause: ErrorCause::NoReply,
            cause_class: None,
        };
        let wrapped = wrap_effect_error("outer.fetch", raised);
        let VmError::Raised {
            py_class,
            message,
            cause,
            cause_class,
        } = wrapped
        else {
            panic!("expected a VmError::Raised to stay Raised through the wrap");
        };
        assert_eq!(py_class, PyExc::RuntimeError);
        assert_eq!(cause, ErrorCause::NoReply);
        assert_eq!(message, "outer.fetch: no reply");
        assert_eq!(cause_class, Some(PyExc::ValueError));
    }

    #[test]
    fn displayed_class_is_the_tools_own_class_before_any_wrap() {
        // Review P2-1 on the second review's own first probe: a direct
        // tool failure, never wrapped, displays its own real class --
        // Python's `err.__cause__ or err` with `__cause__` still `None`.
        let raised = VmError::Raised {
            py_class: PyExc::ValueError,
            message: "no reply".to_string(),
            cause: ErrorCause::NoReply,
            cause_class: None,
        };
        assert_eq!(raised.displayed_class(), Some(PyExc::ValueError));
    }

    #[test]
    fn displayed_class_is_runtime_error_for_a_doubly_wrapped_chain_failure() {
        // Review P2-1 on the second review's own second probe: a nested
        // chain failure (two container wraps deep) displays
        // `RuntimeError` -- the *inner* wrap's own class, one level in
        // from the outer wrap, never the original leaf failure's class
        // two levels down (`err.__cause__ or err` peels exactly one
        // level, same as `dynamic.py:56-57`).
        let raised = VmError::Raised {
            py_class: PyExc::ValueError,
            message: "no reply".to_string(),
            cause: ErrorCause::NoReply,
            cause_class: None,
        };
        let wrapped_once = wrap_effect_error("inner.fetch", raised);
        let wrapped_twice = wrap_effect_error("outer.inner", wrapped_once);
        assert_eq!(wrapped_twice.displayed_class(), Some(PyExc::RuntimeError));
    }

    #[test]
    fn a_cancellation_passes_through_the_wrap_unchanged() {
        let wrapped = wrap_effect_error("outer.fetch", VmError::Cancelled);
        assert_eq!(wrapped, VmError::Cancelled);
    }

    #[test]
    fn a_refusal_passes_through_the_wrap_unchanged() {
        // M1 lane I prerequisites, issue #448, item U3: `VmError::Refused`
        // is fatal the same way `VmError::Cancelled` is (`VmError::
        // is_fatal`) -- never wrapped with an effect-path prefix through a
        // chain/tree container, even though, unlike a cancellation, it
        // still gets recorded like an ordinary failure on every
        // container's own node (ruling R1 on PR #462,
        // `VmError::is_fatal`'s own doc comment) -- this wrap is strictly
        // a control-path no-op either way.
        let refused = VmError::Refused {
            refusal: electricity_bytecode::Refusal {
                path: electricity_bytecode::EffectPath::root().push_name("step"),
                reason: electricity_bytecode::RefusalReason::Loop,
            },
            child_label: Some("child.yaml".to_string()),
        };
        let wrapped = wrap_effect_error("outer.fetch", refused.clone());
        assert_eq!(wrapped, refused);
    }

    #[test]
    fn a_plain_message_still_collapses_to_message() {
        let wrapped = wrap_effect_error("outer.fetch", VmError::Message("boom".to_string()));
        assert_eq!(wrapped, VmError::Message("outer.fetch: boom".to_string()));
    }
}
