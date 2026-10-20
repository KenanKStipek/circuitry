//! Lane B2: `dynamic` (chain and tree), ported from `core/dynamic.py::
//! DynamicRuntime.execute` -- the document root (a non-overlay `dynamic`
//! named `prime`) and every nested `dynamic` effect both run through
//! [`execute_dynamic`], exactly as `DynamicRuntime` is the one Python
//! class behind both.
//!
//! **`ctx` is never cached.** Python's own `ctx` is a *live* dict
//! reference (`store.state`), so a chain's later sibling -- and a
//! deeper descendant several `dynamic` levels down, through the
//! fully-qualified `prime.*` spelling -- automatically sees an earlier
//! write with no code of its own doing anything; this crate's own
//! [`crate::Store::snapshot`] instead returns an owned, point-in-time
//! [`Value`], so neither [`execute_chain`] nor [`execute_tree`]
//! materializes a `ctx` `Value` at all -- they thread a [`super::
//! CtxChain`] (a chain of still-live [`NodeRef`]s plus any already-
//! frozen overlay) down through [`execute_op`] instead, and
//! [`super::live_ctx`] only ever collapses it into a `Value` at the
//! actual point of use (CEL eval, tool rendering), always re-snapshotting
//! every live entry fresh right there. A tree's branches are the one place
//! [`super::CtxSource::Frozen`] is correct: Python takes exactly one
//! shallow `dict(ctx)` snapshot before dispatch and every branch shares
//! it (runtime-semantics.md §5.5), so [`execute_tree`] collapses the
//! inherited chain into one `Frozen` entry before dispatch, matching
//! that one-time snapshot exactly -- nothing else can mutate any of a
//! tree's own ancestors while its isolated branches run. `exec::
//! conditional`'s own `if` branch overlay (Quirk Q1's `scope_ctx`) is
//! *not* a second `Frozen` case, even though it is also rebuilt once
//! per branch step: a branch's own later step can still create a
//! container nested further under an ancestor the overlay already
//! covers, so the overlay is built as a *detached* [`crate::NodeRef`]
//! sharing the live chain's own `Slot`s (`build_overlay`) and pushed as
//! a [`super::CtxSource::Live`] entry instead -- see that module's own
//! doc comment, and `exec::mod`'s own `CtxSource` doc comment, for why.

use super::{
    BoxExecFuture, CtxChain, CtxSource, effect_label, execute_op, interrupted_text, key,
    normalize_labels, store_err, wrap_effect_error,
};
use crate::{CancellationToken, Frame, NodeRef, RunContext, RunObserver, Store, VmError};
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
/// `if`/`loop`). *ctx_chain* is Quirk Q1's `ctx_override` parameter
/// (`core/dynamic.py::DynamicRuntime.execute`), not yet materialized:
/// empty for "no override" (the document root, or an ordinary dynamic
/// nested directly in a chain/tree with no enclosing scope overlay), or
/// carrying the enclosing `if` branch's own overlaid ctx as a single
/// [`CtxSource::Frozen`] entry for a dynamic nested inside one
/// (DESIGN.md §6.3's own re-merge quirk) -- this dynamic's own children
/// always get *ctx_chain* extended with one more [`CtxSource::Live`]
/// entry for *parent* (never collapsed into a `Value` here: see this
/// module's own doc comment on why that has to stay live).
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_dynamic<'a>(
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

        let effect_path = frame.effect_path(&op.path);
        let instance = observer.effect_start(&effect_path);

        // *ctx_chain* for this dynamic's own children always extends the
        // *inherited* chain with one more live entry for *parent* -- the
        // **enclosing** scope this dynamic itself was created under,
        // exactly as `core/dynamic.py::DynamicRuntime.execute`'s own `ctx
        // = store.state if ctx_override is None else ...` reads
        // `store.state` (the parameter *store*, not the `child_store`
        // this dynamic's own children write into). Pushing `&dyn_node`
        // here instead would have every child read this dynamic's own
        // node as if it were one level higher up the tree than it really
        // is -- the Quirk Q1 overlay rebuild inside a nested `if` branch
        // (`tests/conditional.rs::
        // a_later_branch_effect_sees_an_earlier_siblings_write_by_bare_
        // name`) is what first caught this getting that backwards.
        let mut child_chain: CtxChain = ctx_chain.clone();
        child_chain.push(CtxSource::Live(parent.clone()));

        let body_result = match body {
            Region::Block { ops, .. } => {
                execute_chain(
                    ops,
                    store,
                    &dyn_node,
                    name,
                    &child_chain,
                    frame,
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
                    &child_chain,
                    frame,
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
        // just this dynamic's own chain loop). Same *child_chain* as the
        // body: a `finally` effect writes into the same namespace as the
        // body (`core/dynamic.py::_execute_chain`'s own `label` defaults
        // to this dynamic's own name either way, never a distinct
        // `.finally.` segment).
        //
        // *was_set_before_finally* is the **real** *token* (not
        // *finally_token*), read right before `finally:` starts -- a
        // real signal handler still runs (and raises) on Python's own
        // main thread no matter what `get_token().cleanup()` suppresses,
        // so a signal landing *during* an otherwise-successful `finally:`
        // still interrupts it there; this crate's own cooperative
        // `token.is_set()` polling has no equivalent mid-effect
        // interruption, so the closest approximation is: if *token*
        // wasn't set before `finally:` started but is set once it ends,
        // treat that as if `finally:` itself were cancelled (PR #440's
        // review of issue #431).
        let was_set_before_finally = token.is_set();
        let finally_result = match finally {
            Some(Region::Block { ops, .. }) => {
                let finally_token = CancellationToken::new();
                Some(
                    execute_chain(
                        ops,
                        store,
                        &dyn_node,
                        name,
                        &child_chain,
                        frame,
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
        let finally_exc = match finally_result {
            None => None,
            Some(Err(e)) => Some(e),
            Some(Ok(())) if !was_set_before_finally && token.is_set() => Some(VmError::Cancelled),
            Some(Ok(())) => None,
        };

        let reported_error = if let Some(be) = &body_exc {
            store.set_leaf(&dyn_node, key("value"), Value::Bool(false));
            let text = error_text(be, token);
            store.set_leaf(&meta_node, key("error"), Value::Str(text.clone()));
            if let Some(fe) = &finally_exc {
                let finally_text = error_text(fe, token);
                store.set_leaf(
                    &meta_node,
                    key("finally_error"),
                    Value::Str(finally_text.clone()),
                );
                // `core/dynamic.py::DynamicRuntime.execute`'s own
                // `logger.warning("Dynamic %r: cleanup in 'finally' also
                // failed (%s) after the original failure (%s)", ...)` --
                // fires whenever both failed, regardless of this
                // dynamic's own `on_error` (issue #442).
                log::warn!(
                    "Dynamic {}: cleanup in 'finally' also failed ({finally_text}) after the original failure ({text})",
                    dynamic_name_repr(name)
                );
            }
            Some(text)
        } else if let Some(fe) = &finally_exc {
            store.set_leaf(&dyn_node, key("value"), Value::Bool(false));
            let text = error_text(fe, token);
            store.set_leaf(&meta_node, key("error"), Value::Str(text.clone()));
            Some(text)
        } else {
            store.set_leaf(&dyn_node, key("value"), Value::Bool(true));
            None
        };
        store.set_leaf(&meta_node, key("completed_at"), Value::Str(now_iso()));

        observer.effect_complete(
            &effect_path,
            instance,
            reported_error.as_deref(),
            super::completed_at_is_truthy(store, &meta_node),
        );

        if let Some(be) = body_exc {
            if be.is_fatal() || matches!(op.on_error, OnError::Fail) {
                return Err(be);
            }
            // `core/dynamic.py::DynamicRuntime.execute`'s own
            // `logger.warning("Dynamic %r: %s; on_error=%s, continuing
            // with the next effect", ...)` (issue #442).
            log::warn!(
                "Dynamic {}: {}; on_error={}, continuing with the next effect",
                dynamic_name_repr(name),
                error_text(&be, token),
                on_error_str(op.on_error)
            );
            return Ok(());
        }
        if let Some(fe) = finally_exc {
            if fe.is_fatal() || matches!(op.on_error, OnError::Fail) {
                return Err(fe);
            }
            // `core/dynamic.py::DynamicRuntime.execute`'s own
            // `logger.warning("Dynamic %r: finally failed (%s);
            // on_error=%s, continuing with the next effect", ...)`
            // (issue #442).
            log::warn!(
                "Dynamic {}: finally failed ({}); on_error={}, continuing with the next effect",
                dynamic_name_repr(name),
                error_text(&fe, token),
                on_error_str(op.on_error)
            );
            return Ok(());
        }
        Ok(())
    })
}

/// *err*'s own `meta.error` text -- [`interrupted_text`] resolved
/// against *token* for [`VmError::Cancelled`] (which carries no text of
/// its own), *err*'s plain [`std::fmt::Display`] otherwise.
fn error_text(err: &VmError, token: &CancellationToken) -> String {
    match err {
        VmError::Cancelled => interrupted_text(token),
        other => other.to_string(),
    }
}

/// Python `repr(name)` for a `dynamic`'s own `%r`-formatted warning text
/// (issue #442) -- `name` is always a plain string (a dynamic is never
/// unnamed), so this is just [`Value::py_repr`] on it wrapped as a
/// [`Value::Str`].
fn dynamic_name_repr(name: &str) -> String {
    Value::Str(name.to_string()).py_repr()
}

/// The on-degradation warnings above only ever fire for `skip`/
/// `continue` (a dynamic's own `on_error` is never `break`, and `fail`
/// always returns `Err` before reaching them) -- Python's own `%s` on
/// `self.defn.on_error`, the plain config string.
fn on_error_str(on_error: OnError) -> &'static str {
    match on_error {
        OnError::Skip => "skip",
        OnError::Continue => "continue",
        OnError::Fail | OnError::Break => {
            unreachable!(
                "a dynamic's own on_error warning path is only ever reached for skip/continue"
            )
        }
    }
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
/// queue. *container_name* is this dynamic's own name (`op.name`, never
/// the full state path), the prefix a child's own failure gets wrapped
/// with -- [`effect_label`]. *ctx_chain* is handed straight to
/// [`execute_op`] unmaterialized at every step: a later sibling (or, for
/// a `prime.*`-qualified reference, a sibling three `dynamic` levels up)
/// seeing an earlier one's write falls straight out of every
/// [`CtxSource::Live`] entry's own [`super::live_ctx`] re-snapshot, with
/// no extra bookkeeping in this loop. `finally:`'s own chain reuses this
/// with a *token* that is never cancelled (see [`execute_dynamic`]'s own
/// call site) rather than a separate code path.
#[allow(clippy::too_many_arguments)]
fn execute_chain<'a>(
    ops: &'a [Op],
    store: &'a Store,
    write_node: &'a NodeRef,
    container_name: &'a str,
    ctx_chain: &'a CtxChain,
    frame: &'a Frame,
    run_ctx: &'a RunContext<'a>,
    observer: &'a dyn RunObserver,
    token: &'a CancellationToken,
) -> BoxExecFuture<'a> {
    Box::pin(async move {
        for child_op in ops {
            if token.is_set() {
                return Err(VmError::Cancelled);
            }
            let result = execute_op(
                child_op, store, write_node, ctx_chain, frame, run_ctx, observer, token,
            )
            .await;
            // `core/dynamic.py`'s own chain loop calls `store.on_write`
            // from a `finally:` around every single effect -- success or
            // failure alike, never skipped by an absorbed `on_error`.
            observer.write();
            result.map_err(|e| wrap_effect_error(&effect_label(container_name, child_op), e))?;
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
    ctx_chain: &'a CtxChain,
    frame: &'a Frame,
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
        // `RunObserver::dispatch`'s own declared parameter order is
        // `(path, branches, concurrency)` (`observer.rs`) -- *n* (the
        // total) first, *concurrency* second. Each branch is named
        // distinctly by the document itself (lane A's own `Frame`
        // module doc comment: unlike a loop's own `flow: tree` pass,
        // an ordinary `dynamic` tree branch's compiled path is already
        // globally unique), so *frame* passes straight through to every
        // branch unchanged, never through `Frame::nested`.
        let container_path = frame.effect_path(&op.path);
        observer.dispatch(&container_path, n, concurrency);

        if n == 0 {
            return Ok(());
        }

        // Python's own `tree_ctx = dict(ctx)`: one shallow snapshot taken
        // right here, shared by every branch -- the one place a `ctx`
        // *is* allowed to freeze, since nothing else can mutate any of
        // *ctx_chain*'s own ancestors while this tree's branches run (an
        // isolated branch store is the only thing any of them can write
        // into until the merge below).
        let tree_chain: CtxChain = vec![CtxSource::Frozen(super::live_ctx(ctx_chain, store))];
        let branches = store.parallel_branches(dyn_node, n).map_err(store_err)?;
        observer.register_tree_branches(&container_path, &branches);

        let container_name = op
            .name
            .as_deref()
            .expect("a dynamic (root or nested) always has a name");

        // Scoped so `launch`/`in_flight`/`pending` -- which all borrow
        // *branches*/*tree_chain* -- are fully dropped before *branches*
        // is moved into `store.merge` below.
        let (tree_errors, refused, interrupted) = {
            let mut pending: VecDeque<usize> = (0..n).collect();
            let mut in_flight = FuturesUnordered::new();
            let mut tree_errors: Vec<VmError> = Vec::new();
            let mut interrupted = false;
            let mut stopped = false;

            let launch = |idx: usize| {
                let branch_op = &branch_ops[idx];
                let branch_node = &branches[idx];
                let tree_chain = &tree_chain;
                async move {
                    if token.is_set() {
                        return (idx, Err(VmError::Cancelled));
                    }
                    let result = execute_op(
                        branch_op,
                        store,
                        branch_node,
                        tree_chain,
                        frame,
                        run_ctx,
                        observer,
                        token,
                    )
                    .await;
                    // `core/dynamic.py::DynamicRuntime._execute_branch`'s
                    // own `finally: store.on_write(...)` -- once this
                    // branch settles, success or failure, never for a
                    // branch that never started (the `token.is_set()`
                    // early return above skips it, same as Python's own
                    // `get_token().check()` before that `finally`'s try).
                    observer.write();
                    (idx, result)
                }
            };

            for _ in 0..max_workers.min(n) {
                if let Some(idx) = pending.pop_front() {
                    in_flight.push(launch(idx));
                }
            }

            // PR #462: a `VmError::Refused`
            // branch (lane I's own mid-run child refusal, M1 lane I
            // prerequisites, issue #448, item U3) used to fall into the
            // generic `Err(e)` arm below like any ordinary failure --
            // folded into `tree_errors`, then foldable again into one
            // `VmError::Message` summary by `combine_tree_errors` once a
            // *second* branch also failed, which `on_error: continue`
            // then absorbed below (`VmError::is_fatal`'s own doc
            // comment: a capability refusal must never be absorbed, the
            // same as a cancellation) -- or, once `stopped` was already
            // set by an earlier ordinary failure under `stop_on_error`,
            // dropped by the `if !stopped` guard entirely. *refused* is
            // tracked separately from `tree_errors` for exactly that
            // reason: a Refused branch always sets `stopped` (stopping
            // new dispatch, regardless of *stop_on_error*'s own value --
            // a refusal is no more a degradable failure than a
            // cancellation is), is recorded even if `stopped` was
            // already set, and is returned unfolded below, never handed
            // to `combine_tree_errors`.
            let mut refused: Option<VmError> = None;

            while let Some((idx, result)) = in_flight.next().await {
                match result {
                    Ok(()) => {}
                    Err(VmError::Cancelled) => {
                        interrupted = true;
                    }
                    Err(e) if e.is_fatal() => {
                        if refused.is_none() {
                            refused = Some(e);
                        }
                        stopped = true;
                    }
                    Err(e) => {
                        // `core/dynamic.py::DynamicRuntime._await_tree_
                        // branches` breaks out of its own drain loop
                        // immediately after recording the first failure
                        // under `stop_on_error` -- a later branch's own
                        // result is waited for (never force-cancelled)
                        // but never inspected for an error again, so
                        // exactly one failure, never more, ever lands in
                        // `tree_errors` once `stopped` is set.
                        if !stopped {
                            let label = effect_label(container_name, &branch_ops[idx]);
                            tree_errors.push(wrap_effect_error(&label, e));
                            if stop_on_error {
                                stopped = true;
                            }
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

            (tree_errors, refused, interrupted)
        };

        if interrupted {
            // PR #549's review: a cancelled tree never
            // merges (cof's own no-state-write cancellation contract,
            // the comment on `tree_errors`/`refused` above), but the
            // overlay this call registered must still come down here --
            // left in place, a `finally:` clause's own still-coalescing
            // `--live-state` writes would keep showing branch content
            // that is never going to be merged and never reaches
            // `--out`, until `close()` finally overwrites it for good.
            observer.clear_tree_branches(&container_path, &branches);
            return Err(VmError::Cancelled);
        }

        // Cleared *before* the merge below, which takes *branches* by
        // value -- `clear_tree_branches` only needs a borrow, and no
        // flush can run between these two calls (both synchronous, no
        // `.await` in between), so the overlay coming down one
        // statement earlier than the merge it precedes is never
        // observable as a gap.
        observer.clear_tree_branches(&container_path, &branches);
        // A refused branch still merges like any other `stop_on_error`
        // failure -- every in-flight branch has already drained above,
        // so there is nothing left to lose by keeping whatever every
        // branch (refused or not) already wrote.
        store.merge(dyn_node, branches).map_err(store_err)?;
        // `core/dynamic.py`'s own tree-flow merge loop ends with one
        // more `store.on_write(...)`, after every branch has landed.
        observer.write();

        if let Some(refused) = refused {
            return Err(refused);
        }
        if !tree_errors.is_empty() {
            return Err(combine_tree_errors(tree_errors));
        }
        Ok(())
    })
}

/// `core/dynamic.py::TreeExecutionError._format` -- a single branch
/// failure's own already-wrapped (`"<path>: <message>"`) text
/// verbatim; two or more are folded into one numbered summary, each
/// line also naming the failure's own displayed class (`"[i]
/// {type(original).__name__}: {err}"`, `err.__cause__ or err`'s
/// one-level-only unwrap -- [`VmError::displayed_class`] already
/// implements the same rule). Either way, the combined error itself is
/// always a [`VmError::Raised`] of [`electricity_tools::PyExc::
/// TreeExecutionError`] (a `RuntimeError` subclass, like
/// `DecompositionError`), never a bare [`VmError::Message`] and never
/// the single branch's own *class* even when there is only one failure
/// (`TreeExecutionError.__init__` always raises its own type; only its
/// `_format`-built *message* varies with the branch count) -- a nested
/// tree's own combined failure must keep the `TreeExecutionError` class
/// for its *own* enclosing container's wrap to show
/// (`"[N] TreeExecutionError: <path>: ..."`), the same way any other
/// branch failure's class survives a chain/tree wrap, whether that
/// nested tree had one failing branch or several. The single-error case
/// keeps that one branch's own [`electricity_tools::ErrorCause`] rather
/// than discarding it to [`electricity_tools::ErrorCause::None`] -- a
/// bare pass-through of the one real underlying failure, same as a
/// chain's own single-step wrap already does through [`wrap_effect_error`].
fn combine_tree_errors(mut errors: Vec<VmError>) -> VmError {
    if errors.len() == 1 {
        let err = errors.remove(0);
        let cause = match &err {
            VmError::Raised { cause, .. } => cause.clone(),
            _ => electricity_tools::ErrorCause::None,
        };
        return VmError::Raised {
            py_class: electricity_tools::PyExc::TreeExecutionError,
            message: err.to_string(),
            cause,
            cause_class: None,
        };
    }
    let mut parts = vec![format!("{} effects failed in parallel:", errors.len())];
    for (i, err) in errors.iter().enumerate() {
        match err.displayed_class() {
            Some(class) => parts.push(format!("  [{}] {class}: {err}", i + 1)),
            None => parts.push(format!("  [{}] {err}", i + 1)),
        }
    }
    VmError::Raised {
        py_class: electricity_tools::PyExc::TreeExecutionError,
        message: parts.join("\n"),
        cause: electricity_tools::ErrorCause::None,
        cause_class: None,
    }
}
