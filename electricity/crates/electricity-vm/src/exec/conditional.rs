//! Lane B2: `if`/`conditional` in `mode: cel`, ported from `core/
//! conditional.py::ConditionalRuntime`.
//!
//! A *named* `if` creates its own node (`value`/`meta`) and fires
//! balanced `effect_start`/`effect_complete`; an *unnamed* `if` is
//! transparent -- its selected branch's effects write directly into
//! *parent* (the enclosing scope), it has no `value`/`meta` of its own,
//! and it fires neither hook (issue #431's Lane B section: "no hooks for
//! an unnamed `if`").

use super::dynamic::now_iso;
use super::{BoxExecFuture, execute_op, key, normalize_labels, store_err};
use crate::{CancellationToken, NodeRef, RunContext, RunObserver, Store, VmError};
use electricity_bytecode::{Condition, NodeKind, OnError, Op, Region};
use electricity_value::Value;
use indexmap::IndexMap;
use std::collections::HashSet;

/// Runs *op* (a compiled `if`) against *store*. *parent* is the node a
/// *named* `if` creates its own child under, or the node an *unnamed*
/// `if`'s branch effects write directly into. *ctx* is the caller's own
/// already-resolved rendering context -- used as-is, never treated as an
/// override the way a nested `dynamic` would (`core/conditional.py::
/// ConditionalRuntime.execute` takes `ctx` directly, with no
/// `ctx_override` parameter of its own).
pub(crate) fn execute_conditional<'a>(
    op: &'a Op,
    store: &'a Store,
    parent: &'a NodeRef,
    ctx: &'a Value,
    run_ctx: &'a RunContext<'a>,
    observer: &'a dyn RunObserver,
    token: &'a CancellationToken,
) -> BoxExecFuture<'a> {
    Box::pin(async move {
        let NodeKind::Control(Region::If {
            cond,
            then_,
            else_,
            threshold,
        }) = &op.kind
        else {
            unreachable!("execute_conditional is only ever dispatched for Region::If")
        };

        match &op.name {
            Some(name) => {
                let node = store.ensure_dict(parent, key(name)).map_err(store_err)?;
                store.set_leaf(&node, key("value"), Value::None);
                let meta = store.ensure_dict(&node, key("meta")).map_err(store_err)?;
                write_conditional_meta_start(store, &meta, cond, *threshold, op.labels.as_ref());

                observer.effect_start(&op.path);
                let result = decide_and_run(
                    op,
                    cond,
                    then_,
                    else_,
                    store,
                    &node,
                    Some((&node, &meta)),
                    ctx,
                    run_ctx,
                    observer,
                    token,
                )
                .await;
                observer.effect_complete(
                    &op.path,
                    result.as_ref().err().map(VmError::to_string).as_deref(),
                );
                result
            }
            None => {
                decide_and_run(
                    op, cond, then_, else_, store, parent, None, ctx, run_ctx, observer, token,
                )
                .await
            }
        }
    })
}

/// `meta["created_at"] = ...` through `meta["labels"] = ...`
/// (`core/conditional.py::ConditionalRuntime.execute`) -- the 6 keys, in
/// exactly this order, every named `if` node's `meta` starts with.
fn write_conditional_meta_start(
    store: &Store,
    meta: &NodeRef,
    cond: &Condition,
    threshold: f64,
    labels: Option<&Value>,
) {
    store.set_leaf(meta, key("created_at"), Value::Str(now_iso()));
    store.set_leaf(meta, key("completed_at"), Value::None);
    store.set_leaf(meta, key("error"), Value::None);
    store.set_leaf(meta, key("mode"), Value::Str(cond.mode().to_string()));
    store.set_leaf(meta, key("threshold"), Value::Float(threshold));
    store.set_leaf(meta, key("labels"), normalize_labels(labels));
}

/// `core/conditional.py::ConditionalRuntime._decide_and_run` --
/// evaluates *cond* (CEL only; a `mode: model` condition cannot reach
/// this far, refused before the run starts), selects exactly one of
/// *then_*/*else_*, and runs that branch's own effects in order against
/// *branch_parent*, rebuilding the within-branch scope overlay
/// (`scope_ctx`/`local_writes`, Quirk Q1) after each one. *named* is
/// `Some((value_node, meta_node))` for a named `if`, `None` for an
/// unnamed (transparent) one -- every `meta`/`value` write below is a
/// no-op when it is `None`, exactly mirroring Python's own `if node:`/
/// `if meta:` guards.
#[allow(clippy::too_many_arguments)]
async fn decide_and_run<'a>(
    op: &'a Op,
    cond: &'a Condition,
    then_: &'a Region,
    else_: &'a Option<Box<Region>>,
    store: &'a Store,
    branch_parent: &'a NodeRef,
    named: Option<(&'a NodeRef, &'a NodeRef)>,
    ctx: &'a Value,
    run_ctx: &'a RunContext<'a>,
    observer: &'a dyn RunObserver,
    token: &'a CancellationToken,
) -> Result<(), VmError> {
    let Condition::Cel { expr, strict } = cond else {
        return Err(VmError::NotImplemented(format!(
            "{}: a `mode: model` `if` is not supported by the M0-H interpreter",
            op.path
        )));
    };

    let result = match electricity_cel::evaluate_condition(expr, ctx, *strict) {
        Ok(b) => b,
        Err(e) => {
            let text = e.to_string();
            if let Some((_, meta)) = named {
                store.set_leaf(meta, key("error"), Value::Str(text.clone()));
                store.set_leaf(meta, key("completed_at"), Value::Str(now_iso()));
            }
            match op.on_error {
                OnError::Fail => return Err(VmError::Message(text)),
                OnError::Skip => {
                    if let Some((node, _)) = named {
                        let mut v = IndexMap::new();
                        v.insert(key("result"), Value::None);
                        v.insert(key("branch"), Value::None);
                        v.insert(key("effects"), Value::Dict(IndexMap::new()));
                        store.set_leaf(node, key("value"), Value::Dict(v));
                    }
                    return Ok(());
                }
                OnError::Continue => false,
                OnError::Break => unreachable!("a conditional's own on_error is never `break`"),
            }
        }
    };

    let branch_ops: &[Op] = select_branch_ops(result, then_, else_);
    let branch_label = if result { "then" } else { "else" };

    if let Some((_, meta)) = named {
        store.set_leaf(meta, key("condition_result"), Value::Bool(result));
        store.set_leaf(meta, key("branch"), Value::Str(branch_label.to_string()));
    }

    let own_names: HashSet<Value> = branch_ops
        .iter()
        .filter_map(|o| o.name.clone().map(Value::Str))
        .collect();
    let baseline: HashSet<Value> = branch_parent.borrow().keys().cloned().collect();
    let base_ctx = ctx.clone();
    let mut live_ctx = ctx.clone();
    let mut executed: Vec<Value> = Vec::new();
    let mut failure: Option<VmError> = None;

    for (index, child_op) in branch_ops.iter().enumerate() {
        match execute_op(
            child_op,
            store,
            branch_parent,
            &live_ctx,
            run_ctx,
            observer,
            token,
        )
        .await
        {
            Ok(()) => {
                executed.push(effect_record(child_op, index));
                let local = local_writes(store, branch_parent, &baseline, &own_names);
                live_ctx = scope_ctx(&base_ctx, &local);
            }
            Err(e) => {
                failure = Some(match e {
                    VmError::Interrupted(_) => e,
                    other => match &child_op.name {
                        Some(name) => VmError::Message(format!("{name}: {other}")),
                        None => other,
                    },
                });
                break;
            }
        }
    }

    if let Some(err) = failure {
        // Python's own outer `except Exception` (never `BaseException`)
        // around the whole branch loop: a cancellation propagates raw,
        // with no `value`/`meta.error` write at all.
        if matches!(err, VmError::Interrupted(_)) {
            return Err(err);
        }
        if let Some((node, meta)) = named {
            store.set_leaf(
                node,
                key("value"),
                branch_value(result, branch_label, executed),
            );
            store.set_leaf(meta, key("error"), Value::Str(err.to_string()));
            store.set_leaf(meta, key("completed_at"), Value::Str(now_iso()));
        }
        return Err(err);
    }

    if let Some((node, meta)) = named {
        store.set_leaf(
            node,
            key("value"),
            branch_value(result, branch_label, executed),
        );
        store.set_leaf(meta, key("completed_at"), Value::Str(now_iso()));
    }
    Ok(())
}

fn branch_value(result: bool, branch_label: &str, executed: Vec<Value>) -> Value {
    let mut v = IndexMap::new();
    v.insert(key("result"), Value::Bool(result));
    v.insert(key("branch"), Value::Str(branch_label.to_string()));
    v.insert(key("effects"), Value::List(executed));
    Value::Dict(v)
}

fn region_ops(region: &Region) -> &[Op] {
    match region {
        Region::Block { ops, .. } => ops.as_slice(),
        _ => unreachable!("an if branch is always compiled as Region::Block"),
    }
}

fn select_branch_ops<'a>(
    result: bool,
    then_: &'a Region,
    else_: &'a Option<Box<Region>>,
) -> &'a [Op] {
    if result {
        region_ops(then_)
    } else {
        match else_ {
            Some(region) => region_ops(region),
            None => &[],
        }
    }
}

/// `ConditionalRuntime._effect_record` -- `index`/`type`/`name`, in that
/// order; `type` is the Python class name [`Op::python_type_name`]
/// already reproduces.
fn effect_record(op: &Op, index: usize) -> Value {
    let mut m = IndexMap::new();
    m.insert(key("index"), Value::from(index as i64));
    m.insert(key("type"), Value::from(op.python_type_name()));
    m.insert(
        key("name"),
        match &op.name {
            Some(n) => Value::from(n.as_str()),
            None => Value::None,
        },
    );
    Value::Dict(m)
}

/// `core.scope.local_writes` -- the top-level keys *node* gained since
/// *baseline*, plus any of *own_names* even if already present in
/// *baseline* (a step shadowing an enclosing name of the same spelling
/// still counts as local to this branch).
fn local_writes(
    store: &Store,
    node: &NodeRef,
    baseline: &HashSet<Value>,
    own_names: &HashSet<Value>,
) -> IndexMap<Value, Value> {
    let snapshot = store.snapshot(node);
    let Value::Dict(map) = &snapshot else {
        return IndexMap::new();
    };
    map.iter()
        .filter(|(k, _)| !baseline.contains(*k) || own_names.contains(*k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// `core.scope.scope_ctx` -- a shallow `{**ctx, **local}` overlay, with
/// *local* additionally merged one level inside `ctx["prime"]` so both
/// the bare (`{{step.value}}`) and `prime`-qualified
/// (`{{prime.step.value}}`) spellings resolve inside the branch.
fn scope_ctx(ctx: &Value, local: &IndexMap<Value, Value>) -> Value {
    if local.is_empty() {
        return ctx.clone();
    }
    let ctx_map = match ctx {
        Value::Dict(d) => d.clone(),
        _ => IndexMap::new(),
    };
    let mut merged = ctx_map.clone();
    for (k, v) in local {
        merged.insert(k.clone(), v.clone());
    }
    let prime_key = key("prime");
    let new_prime = match ctx_map.get(&prime_key) {
        Some(Value::Dict(parent)) => {
            let mut np = parent.clone();
            for (k, v) in local {
                np.insert(k.clone(), v.clone());
            }
            Value::Dict(np)
        }
        _ => Value::Dict(local.clone()),
    };
    merged.insert(prime_key, new_prime);
    Value::Dict(merged)
}
