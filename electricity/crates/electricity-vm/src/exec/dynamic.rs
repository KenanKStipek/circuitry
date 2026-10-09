//! Lane B2: `dynamic` (chain and tree), ported from `core/dynamic.py::
//! DynamicRuntime.execute` -- the document root (a non-overlay `dynamic`
//! named `prime`) and every nested `dynamic` effect both run through
//! [`execute_dynamic`], exactly as `DynamicRuntime` is the one Python
//! class behind both.
//!
//! **`ctx` is never cached.** Python's own `ctx` is a *live* dict
//! reference (`store.state`), so a chain's later sibling automatically
//! sees an earlier one's writes with no code of its own doing anything
//! -- this crate's own [`crate::Store::snapshot`] instead returns an
//! owned, point-in-time [`Value`], so [`execute_chain`] recomputes it
//! fresh (via [`super::resolve_ctx`]) immediately before *every* chain
//! step, not once at the dynamic's own entry. A tree's branches are the
//! opposite: Python takes exactly one shallow `dict(ctx)` snapshot
//! before dispatch and every branch shares it (runtime-semantics.md
//! §5.5), so [`execute_tree`] snapshots once too.

use super::{
    BoxExecFuture, execute_op, interrupted_text, key, normalize_labels, resolve_ctx, store_err,
    wrap_effect_error,
};
use crate::{CancellationToken, NodeRef, RunContext, RunObserver, Store, VmError};
use electricity_bytecode::{LoopFlow, NodeKind, OnError, Op, Region};
use electricity_value::Value;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use std::collections::VecDeque;

/// `datetime.now(timezone.utc).isoformat()` -- always includes a
/// fractional-second component (unlike Python's own `isoformat`, which
/// omits it when the microsecond happens to be exactly zero): both are
/// valid ISO-8601, and every consumer of `created_at`/`completed_at`
/// (the conformance normalizer, `datetime.fromisoformat`) only checks the
/// shape, never the exact text, of this crate's own timestamps.
pub(crate) fn now_iso() -> String {
    use chrono::SecondsFormat;
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false)
}

/// Runs *op* -- the document root, or a nested `dynamic` -- against
/// *store*, writing its own node under *parent* at `op.name` (always
/// `Some`: an unnamed `dynamic` cannot be compiled, unlike an unnamed
/// `if`/`loop`). *ctx_override* is Quirk Q1's `ctx_override` parameter
/// (`core/dynamic.py::DynamicRuntime.execute`): [`Value::None`] for "no
/// override" (the document root, or an ordinary dynamic nested directly
/// in a chain/tree with no enclosing scope overlay), or the enclosing
/// `if` branch's own overlaid ctx for a dynamic nested inside one
/// (DESIGN.md §6.3's own re-merge quirk) -- this function always
/// recomputes its own effective ctx as `resolve_ctx(ctx_override,
/// store.snapshot(parent))`, never using *ctx_override* directly.
pub(crate) fn execute_dynamic<'a>(
    op: &'a Op,
    store: &'a Store,
    parent: &'a NodeRef,
    ctx_override: &'a Value,
    run_ctx: &'a RunContext<'a>,
    observer: &'a dyn RunObserver,
    token: &'a CancellationToken,
) -> BoxExecFuture<'a> {
    Box::pin(async move {
        let name = op
            .name
            .as_deref()
            .expect("a dynamic (root or nested) always has a name");
        let dyn_node = store.ensure_dict(parent, key(name)).map_err(store_err)?;

        let (body, finally): (&Region, Option<&Region>) = match &op.kind {
            NodeKind::Control(Region::TryFinally { body, finally }) => {
                (body.as_ref(), Some(finally.as_ref()))
            }
            NodeKind::Control(region) => (region, None),
            NodeKind::Leaf(_) => {
                unreachable!("execute_dynamic is only ever dispatched for a Control region")
            }
        };
        let flow = body
            .dynamic_flow()
            .expect("a dynamic's own body is always Block or Parallel");

        store.set_leaf(&dyn_node, key("value"), Value::None);
        let meta_node = store
            .ensure_dict(&dyn_node, key("meta"))
            .map_err(store_err)?;
        write_dynamic_meta_start(store, &meta_node, run_ctx, flow, op.labels.as_ref());

        observer.effect_start(&op.path);

        // *ctx* for this dynamic's own children is always recomputed
        // from *parent* -- the **enclosing** scope this dynamic itself
        // was created under, exactly as `core/dynamic.py::DynamicRuntime.
        // execute`'s own `ctx = store.state if ctx_override is None else
        // ...` reads `store.state` (the parameter *store*, not the
        // `child_store` this dynamic's own children write into). Passing
        // `&dyn_node` here instead would have every child read this
        // dynamic's own node as if it were one level higher up the tree
        // than it really is -- the Quirk Q1 overlay rebuild inside a
        // nested `if` branch (`tests/conditional.rs::
        // a_later_branch_effect_sees_an_earlier_siblings_write_by_bare_
        // name`) is what first caught this getting that backwards.
        let body_result = match body {
            Region::Block { ops, .. } => {
                execute_chain(
                    ops,
                    store,
                    &dyn_node,
                    parent,
                    ctx_override,
                    run_ctx,
                    observer,
                    token,
                )
                .await
            }
            Region::Parallel {
                branches,
                max_concurrency,
                stop_on_error,
            } => {
                execute_tree(
                    op,
                    branches,
                    *max_concurrency,
                    *stop_on_error,
                    store,
                    &dyn_node,
                    parent,
                    ctx_override,
                    run_ctx,
                    observer,
                    token,
                )
                .await
            }
            Region::If { .. } | Region::Loop { .. } | Region::TryFinally { .. } => {
                unreachable!("a dynamic's own body is never If/Loop/TryFinally")
            }
        };

        // `finally:` always runs -- including when the body above was
        // cancelled -- against a fresh, never-cancelled token
        // (`core/dynamic.py`'s own `get_token().cleanup()` context,
        // which suppresses *every* nested check for the duration, not
        // just this dynamic's own chain loop).
        let finally_result = match finally {
            Some(Region::Block { ops, .. }) => {
                let finally_token = CancellationToken::new();
                Some(
                    execute_chain(
                        ops,
                        store,
                        &dyn_node,
                        parent,
                        ctx_override,
                        run_ctx,
                        observer,
                        &finally_token,
                    )
                    .await,
                )
            }
            Some(_) => unreachable!("a dynamic's own finally: is always a Block"),
            None => None,
        };

        let body_exc = body_result.err();
        let finally_exc = finally_result.and_then(|r| r.err());

        let reported_error = if let Some(be) = &body_exc {
            store.set_leaf(&dyn_node, key("value"), Value::Bool(false));
            let text = be.to_string();
            store.set_leaf(&meta_node, key("error"), Value::Str(text.clone()));
            if let Some(fe) = &finally_exc {
                store.set_leaf(&meta_node, key("finally_error"), Value::Str(fe.to_string()));
            }
            Some(text)
        } else if let Some(fe) = &finally_exc {
            store.set_leaf(&dyn_node, key("value"), Value::Bool(false));
            let text = fe.to_string();
            store.set_leaf(&meta_node, key("error"), Value::Str(text.clone()));
            Some(text)
        } else {
            store.set_leaf(&dyn_node, key("value"), Value::Bool(true));
            None
        };
        store.set_leaf(&meta_node, key("completed_at"), Value::Str(now_iso()));

        observer.effect_complete(&op.path, reported_error.as_deref());

        if let Some(be) = body_exc {
            let is_cancellation = matches!(be, VmError::Interrupted(_));
            if is_cancellation || matches!(op.on_error, OnError::Fail) {
                return Err(be);
            }
            return Ok(());
        }
        if let Some(fe) = finally_exc {
            let is_cancellation = matches!(fe, VmError::Interrupted(_));
            if is_cancellation || matches!(op.on_error, OnError::Fail) {
                return Err(fe);
            }
            return Ok(());
        }
        Ok(())
    })
}

/// `meta.update({...})` (`core/dynamic.py::DynamicRuntime.execute`) --
/// the 10 keys, in exactly this order, every `dynamic` node's `meta`
/// starts with.
fn write_dynamic_meta_start(
    store: &Store,
    meta_node: &NodeRef,
    run_ctx: &RunContext<'_>,
    flow: LoopFlow,
    labels: Option<&Value>,
) {
    store.set_leaf(meta_node, key("created_at"), Value::Str(now_iso()));
    store.set_leaf(meta_node, key("completed_at"), Value::None);
    store.set_leaf(
        meta_node,
        key("adapter"),
        Value::Str(run_ctx.adapter.to_string()),
    );
    store.set_leaf(
        meta_node,
        key("model"),
        Value::Str(run_ctx.model.to_string()),
    );
    store.set_leaf(meta_node, key("tokens_sent"), Value::None);
    store.set_leaf(meta_node, key("tokens_received"), Value::None);
    store.set_leaf(meta_node, key("error"), Value::None);
    store.set_leaf(
        meta_node,
        key("flow"),
        Value::Str(flow.as_str().to_string()),
    );
    store.set_leaf(meta_node, key("dry_run"), Value::Bool(run_ctx.dry_run));
    store.set_leaf(meta_node, key("labels"), normalize_labels(labels));
}

/// `core/dynamic.py::DynamicRuntime._execute_chain`/the chain half of
/// `.execute` -- runs *ops* in order, stopping at the first unabsorbed
/// failure, checking *token* before every single one (including the
/// first: a chain that is already cancelled before it starts runs
/// nothing at all) -- "Queued branches never start once cancelled"
/// applies just as much to "the next chain step" as to a tree's own
/// queue. `finally:`'s own chain reuses this with a *token* that is
/// never cancelled (see [`execute_dynamic`]'s own call site) rather than
/// a separate code path.
#[allow(clippy::too_many_arguments)]
fn execute_chain<'a>(
    ops: &'a [Op],
    store: &'a Store,
    write_node: &'a NodeRef,
    ctx_node: &'a NodeRef,
    ctx_override: &'a Value,
    run_ctx: &'a RunContext<'a>,
    observer: &'a dyn RunObserver,
    token: &'a CancellationToken,
) -> BoxExecFuture<'a> {
    Box::pin(async move {
        for child_op in ops {
            if token.is_set() {
                return Err(VmError::Interrupted(interrupted_text(token)));
            }
            let live_ctx = resolve_ctx(ctx_override, store.snapshot(ctx_node));
            execute_op(
                child_op, store, write_node, &live_ctx, run_ctx, observer, token,
            )
            .await
            .map_err(|e| wrap_effect_error(&child_op.path, e))?;
        }
        Ok(())
    })
}

/// `core/dynamic.py::DynamicRuntime.execute`'s own `flow == "tree"`
/// branch -- a shared, deterministic `ctx` snapshot taken once before
/// dispatch (never per-branch, unlike chain), up to `max_workers`
/// branches in flight at once via a manually bounded
/// [`FuturesUnordered`] (issue #431's Lane B section: "run as futures
/// inside one task", not `tokio::task::spawn_local` -- this function's
/// own arguments aren't `'static`), merged back into *dyn_node* in index
/// order only when nothing was ever cancelled (`core/dynamic.py`'s own
/// merge loop sits *after* the try/except that a `BaseException` skips
/// entirely -- see this function's own `interrupted` handling, which
/// reproduces that all-or-nothing rule rather than merging every branch
/// but the one that got interrupted).
#[allow(clippy::too_many_arguments)]
fn execute_tree<'a>(
    op: &'a Op,
    branch_ops: &'a [Op],
    max_concurrency: Option<u32>,
    stop_on_error: bool,
    store: &'a Store,
    dyn_node: &'a NodeRef,
    ctx_node: &'a NodeRef,
    ctx_override: &'a Value,
    run_ctx: &'a RunContext<'a>,
    observer: &'a dyn RunObserver,
    token: &'a CancellationToken,
) -> BoxExecFuture<'a> {
    Box::pin(async move {
        let n = branch_ops.len();
        let max_workers = match max_concurrency {
            Some(mc) => std::cmp::max(1, std::cmp::min(mc as usize, n)),
            None => std::cmp::max(1, n),
        };
        let concurrency = std::cmp::min(max_workers, n);
        observer.dispatch(&op.path, concurrency, n);

        if n == 0 {
            return Ok(());
        }

        let tree_ctx = resolve_ctx(ctx_override, store.snapshot(ctx_node));
        let branches = store.parallel_branches(dyn_node, n).map_err(store_err)?;

        // Scoped so `launch`/`in_flight`/`pending` -- which all borrow
        // *branches*/*tree_ctx* -- are fully dropped before *branches* is
        // moved into `store.merge` below.
        let (tree_errors, interrupted) = {
            let mut pending: VecDeque<usize> = (0..n).collect();
            let mut in_flight = FuturesUnordered::new();
            let mut tree_errors: Vec<VmError> = Vec::new();
            let mut interrupted = false;
            let mut stopped = false;

            let launch = |idx: usize| {
                let branch_op = &branch_ops[idx];
                let branch_node = &branches[idx];
                let tree_ctx = &tree_ctx;
                async move {
                    if token.is_set() {
                        return (idx, Err(VmError::Interrupted(interrupted_text(token))));
                    }
                    let result = execute_op(
                        branch_op,
                        store,
                        branch_node,
                        tree_ctx,
                        run_ctx,
                        observer,
                        token,
                    )
                    .await;
                    (idx, result)
                }
            };

            for _ in 0..max_workers.min(n) {
                if let Some(idx) = pending.pop_front() {
                    in_flight.push(launch(idx));
                }
            }

            while let Some((idx, result)) = in_flight.next().await {
                match result {
                    Ok(()) => {}
                    Err(VmError::Interrupted(_)) => {
                        interrupted = true;
                    }
                    Err(e) => {
                        tree_errors.push(wrap_effect_error(&branch_ops[idx].path, e));
                        if stop_on_error {
                            stopped = true;
                        }
                    }
                }
                if token.is_set() {
                    interrupted = true;
                }
                if interrupted || stopped {
                    pending.clear();
                } else if let Some(next_idx) = pending.pop_front() {
                    in_flight.push(launch(next_idx));
                }
            }

            (tree_errors, interrupted)
        };

        if interrupted {
            return Err(VmError::Interrupted(interrupted_text(token)));
        }

        store.merge(dyn_node, branches).map_err(store_err)?;

        if !tree_errors.is_empty() {
            return Err(combine_tree_errors(tree_errors));
        }
        Ok(())
    })
}

/// `core/dynamic.py::TreeExecutionError._format` -- a single branch
/// failure's own already-wrapped (`"<path>: <message>"`) text verbatim;
/// two or more are folded into one numbered summary.
///
/// Known deviation: Python's own numbered lines additionally show each
/// failure's *original* (pre-wrap) exception class name (`"[1]
/// ValueError: <path>: <message>"`) by unwrapping one level of
/// `__cause__` -- this crate's [`VmError`] does not carry a Python-style
/// exception class name to reproduce that with, so each line here is
/// just `"  [N] <already-wrapped message>"`. Acceptable for M0-H: no
/// conformance case or acceptance criterion in issue #431 exercises two
/// *simultaneous* tree-branch failures' combined text, only a single
/// failure's (which this matches exactly) or `stop_on_error`'s dispatch
/// behavior (which this does not touch).
fn combine_tree_errors(mut errors: Vec<VmError>) -> VmError {
    if errors.len() == 1 {
        return errors.remove(0);
    }
    let mut parts = vec![format!("{} effects failed in parallel:", errors.len())];
    for (i, err) in errors.iter().enumerate() {
        parts.push(format!("  [{}] {err}", i + 1));
    }
    VmError::Message(parts.join("\n"))
}
