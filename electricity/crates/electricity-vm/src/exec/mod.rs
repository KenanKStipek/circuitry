//! Lane B/C: the per-container execution modules -- `dynamic` (chain/
//! tree), `conditional` (CEL `if`), and this module's own [`tool`]
//! (lane A's real `run_tool` dispatch seam).
//!
//! This module also hosts the handful of helpers every container
//! executor needs (the single-effect dispatcher [`execute_op`], the
//! `ctx`/override merge, the `meta.labels`/interrupt-text/error-wrap
//! conventions) -- shared, rather than duplicated, between
//! `dynamic.rs` and `conditional.rs`.

pub mod conditional;
pub mod dynamic;
pub mod tool;

use crate::{CancellationToken, NodeRef, RunContext, RunObserver, Store, StoreError, VmError};
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
/// (including one made *after* this entry joined the chain) is always
/// visible -- or [`CtxSource::Frozen`], a `Value` already materialized
/// once and never refreshed (a tree's own one-time `dict(ctx)` snapshot,
/// DESIGN.md ::5.5; an `if` branch's own per-step `scope_ctx` overlay,
/// Quirk Q1, rebuilt fresh from the branch's own *unmodified* starting
/// chain on every single step -- never from the previous step's own
/// overlay, exactly as Python's `scope_ctx(base_ctx, local)` always
/// re-reads `base_ctx`, never the previous iteration's own rebuilt
/// `ctx` -- see `exec::conditional`'s own module doc comment. A branch's
/// *first* step is never wrapped in a `Frozen` entry at all: it runs
/// against the branch's own starting chain exactly as received, still
/// live, because freezing it before the branch has written anything of
/// its own would lose every nested write a step inside it goes on to
/// make through an already-existing ancestor path -- the P0 "a nested
/// container freezes its own ancestors' state" finding on PR #440,
/// which named exactly this). Python's
/// own `ctx = store.state if ctx_override is None else {**ctx_override,
/// **store.state}` never actually copies a nested dict -- the dict
/// literal/spread only ever touches the *top* level, so every nested
/// value (in particular `ctx["prime"]`, however many `dynamic` levels up
/// it came from) is the exact same live object `store.state` itself
/// mutates -- so a Rust `Value`, once cloned out of a [`Store::snapshot`],
/// can never reproduce that by staying put: it has to be rebuilt from
/// the *live* [`NodeRef`] chain at the moment it is actually used
/// instead (issue #431 review, the P0 "a nested container freezes its
/// own ancestors' state" finding on PR #440).
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
pub(crate) fn live_ctx(chain: &CtxChain, store: &Store) -> Value {
    let mut merged: IndexMap<Value, Value> = IndexMap::new();
    for source in chain {
        let resolved = match source {
            CtxSource::Live(node) => store.snapshot(node),
            CtxSource::Frozen(value) => value.clone(),
        };
        if let Value::Dict(ref map) = resolved {
            for (k, v) in map {
                merged.insert(k.clone(), v.clone());
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
    match err {
        VmError::Cancelled => err,
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
pub(crate) fn execute_op<'a>(
    op: &'a Op,
    store: &'a Store,
    parent: &'a NodeRef,
    ctx_chain: &'a CtxChain,
    run_ctx: &'a RunContext<'a>,
    observer: &'a dyn RunObserver,
    token: &'a CancellationToken,
) -> BoxExecFuture<'a> {
    Box::pin(async move {
        match &op.kind {
            NodeKind::Leaf(leaf) => match leaf.as_ref() {
                LeafKind::Tool(tool_op) => {
                    let ctx = live_ctx(ctx_chain, store);
                    tool::execute_tool(op, tool_op, store, parent, &ctx, run_ctx, observer, token)
                        .await
                }
                _ => Err(VmError::NotImplemented(format!(
                    "{}: unsupported effect type in the M0-H interpreter",
                    op.path
                ))),
            },
            NodeKind::Control(region) => match region {
                Region::If { .. } => {
                    conditional::execute_conditional(
                        op, store, parent, ctx_chain, run_ctx, observer, token,
                    )
                    .await
                }
                Region::Block { .. } | Region::Parallel { .. } | Region::TryFinally { .. } => {
                    dynamic::execute_dynamic(op, store, parent, ctx_chain, run_ctx, observer, token)
                        .await
                }
                Region::Loop { .. } => Err(VmError::NotImplemented(format!(
                    "{}: `loop` is not supported by the M0-H interpreter",
                    op.path
                ))),
            },
        }
    })
}
