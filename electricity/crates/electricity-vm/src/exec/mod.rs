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
use electricity_bytecode::{EffectPath, LeafKind, NodeKind, Op, Region};
use electricity_value::Value;
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
/// matching [`crate::CancellationToken`]'s own doc comment (`SIGINT` 2,
/// `SIGTERM` 15, `SIGHUP` 1).
const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;
const SIGHUP: i32 = 1;

/// Python's `core/dynamic.py::_error_text`'s own signal-name fallback --
/// the text [`crate::VmError::Interrupted`] carries. Every real call site
/// in this crate only ever constructs an `Interrupted` through this
/// function, so the three wordings here are the only ones that type's
/// `String` payload ever holds.
pub(crate) fn interrupted_text(token: &CancellationToken) -> String {
    match token.signum() {
        Some(SIGTERM) => "Interrupted (SIGTERM)".to_string(),
        Some(SIGHUP) => "Interrupted (SIGHUP)".to_string(),
        // `Some(SIGINT)`, and -- defensively, since this is only ever
        // called once `token.is_set()` is already known true -- any
        // other/missing signum too.
        _ => {
            let _ = SIGINT;
            "Interrupted (Ctrl-C/SIGINT)".to_string()
        }
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
/// [`Value::None`] is this crate's spelling of Python's `ctx_override is
/// None` (lane A's own test already calls [`crate::execute_root`] with
/// exactly that sentinel for the document root, which has no override of
/// its own).
pub(crate) fn resolve_ctx(ctx_override: &Value, live: Value) -> Value {
    let Value::Dict(override_map) = ctx_override else {
        return live;
    };
    let mut merged = override_map.clone();
    if let Value::Dict(live_map) = &live {
        for (k, v) in live_map {
            merged.insert(k.clone(), v.clone());
        }
    }
    Value::Dict(merged)
}

/// Wraps *err* with *path*'s own `"<path>: <message>"` prefix
/// (`core/dynamic.py`'s chain-flow wrap, `RuntimeError(f"{effect_path}:
/// {e}")`) -- except a cancellation, which Python's own `except
/// Exception` (never `BaseException`) never catches, so it always
/// propagates unwrapped here too.
pub(crate) fn wrap_effect_error(path: &EffectPath, err: VmError) -> VmError {
    match err {
        VmError::Interrupted(_) => err,
        other => VmError::Message(format!("{path}: {other}")),
    }
}

/// Dispatches one compiled `Op` -- a `tool` leaf (through lane C's own
/// [`tool::execute_tool`]), a nested `dynamic`/`finally:`-wrapped
/// `dynamic` (through [`dynamic::execute_dynamic`]), or an `if`
/// (through [`conditional::execute_conditional`]). *parent* is the
/// `NodeRef` *op* itself writes under (`store.ensure_dict(parent,
/// op.name)` for a named effect; passed straight through for a
/// transparent, unnamed `if`). *ctx* is already the caller's own fully
/// resolved rendering context -- for a `tool`/`if` child this is used
/// directly; for a nested `dynamic` child, [`dynamic::execute_dynamic`]
/// treats it as *that* dynamic's own `ctx_override` (Quirk Q1's
/// re-merge), exactly mirroring `core/dynamic.py::_execute_effect`'s own
/// per-effect-type dispatch.
///
/// `prompt`/`use`/`yield`/`reflector`/`loop`/a `mode: model` `if` are all
/// refused before a real run ever starts (lane A's `first_unsupported`),
/// so reaching one here is a defect, not a reachable document shape --
/// reported as [`VmError::NotImplemented`] rather than panicking.
pub(crate) fn execute_op<'a>(
    op: &'a Op,
    store: &'a Store,
    parent: &'a NodeRef,
    ctx: &'a Value,
    run_ctx: &'a RunContext<'a>,
    observer: &'a dyn RunObserver,
    token: &'a CancellationToken,
) -> BoxExecFuture<'a> {
    Box::pin(async move {
        match &op.kind {
            NodeKind::Leaf(leaf) => match leaf.as_ref() {
                LeafKind::Tool(tool_op) => {
                    tool::execute_tool(op, tool_op, store, parent, ctx, run_ctx, observer, token)
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
                        op, store, parent, ctx, run_ctx, observer, token,
                    )
                    .await
                }
                Region::Block { .. } | Region::Parallel { .. } | Region::TryFinally { .. } => {
                    dynamic::execute_dynamic(op, store, parent, ctx, run_ctx, observer, token).await
                }
                Region::Loop { .. } => Err(VmError::NotImplemented(format!(
                    "{}: `loop` is not supported by the M0-H interpreter",
                    op.path
                ))),
            },
        }
    })
}
