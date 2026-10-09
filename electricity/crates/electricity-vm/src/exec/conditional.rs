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
use super::{BoxExecFuture, CtxChain, CtxSource, execute_op, key, normalize_labels, store_err};
use crate::{CancellationToken, NodeRef, RunContext, RunObserver, Slot, Store, VmError};
use electricity_bytecode::{Condition, NodeKind, OnError, Op, Region};
use electricity_value::Value;
use indexmap::IndexMap;
use std::collections::HashSet;

/// Runs *op* (a compiled `if`) against *store*. *parent* is the node a
/// *named* `if` creates its own child under, or the node an *unnamed*
/// `if`'s branch effects write directly into. *ctx_chain* is the
/// caller's own not-yet-materialized rendering context (see
/// [`super::CtxSource`]) -- used as-is, never extended with an entry of
/// this `if`'s own (`core/conditional.py::ConditionalRuntime.execute`
/// takes `ctx` directly, with no `ctx_override` parameter of its own:
/// an `if`, unlike a `dynamic`, never introduces a scope of its own for
/// anything *outside* its own branch loop). [`decide_and_run`] resolves
/// this chain itself, fresh, every time it is actually needed -- never
/// once, up front -- see that function's own doc comment for why.
pub(crate) fn execute_conditional<'a>(
    op: &'a Op,
    store: &'a Store,
    parent: &'a NodeRef,
    ctx_chain: &'a CtxChain,
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
                    ctx_chain,
                    run_ctx,
                    observer,
                    token,
                )
                .await;
                // `cli/events.py::EventLog.on_complete` reads the node's
                // own `meta.error` for the `end` event, never the
                // exception `decide_and_run` returned -- the two diverge
                // for a condition error absorbed by `on_error: skip`/
                // `continue` (meta.error is set, but `decide_and_run`
                // still returns `Ok`) and for an interrupted branch
                // (`decide_and_run` returns `Err(Cancelled)`, but
                // meta.error is never written for a cancellation).
                observer.effect_complete(&op.path, node_error(store, &meta).as_deref());
                result
            }
            None => {
                decide_and_run(
                    op, cond, then_, else_, store, parent, None, ctx_chain, run_ctx, observer,
                    token,
                )
                .await
            }
        }
    })
}

/// *meta*'s own current `error` value, if it is a string -- what
/// [`execute_conditional`] reports to `effect_complete` instead of
/// whatever `decide_and_run` itself returned (see that call site).
fn node_error(store: &Store, meta: &NodeRef) -> Option<String> {
    match &store.snapshot(meta) {
        Value::Dict(map) => match map.get(&key("error")) {
            Some(Value::Str(text)) => Some(text.clone()),
            _ => None,
        },
        _ => None,
    }
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
///
/// Python's own `ctx` parameter (what *ctx_chain* stands in for) is a
/// live reference: `base_ctx = ctx` binds no new object, so
/// `base_ctx.get("prime")` -- read again by every `scope_ctx` call
/// below -- always sees whatever that live object currently holds,
/// including a write an *earlier* branch step just made through an
/// already-existing ancestor path (`state.prime.<dynamic>.<name>`, any
/// number of `dynamic` levels deep, however many steps ago that
/// ancestor's own container was created). This function never
/// resolves *ctx_chain* into one `Value` and keeps using that -- a
/// single such snapshot, taken once up front, cannot reproduce
/// liveness: `Value` is owned data, not a reference, so it would
/// freeze every nested value exactly as it stood at that one moment --
/// the P0 "a nested container freezes its own ancestors' state"
/// finding on PR #440, found *twice*: once for *ctx_chain* itself
/// (fixed by threading the chain this far at all, rather than a
/// resolved `ctx: &Value`), and once more for the overlay this
/// function rebuilds after every branch step ([`build_overlay`]) --
/// an *initial* fix collapsed that overlay into a [`super::
/// CtxSource::Frozen`] `Value` too, which is exactly as wrong one level
/// later: a branch's own *later* step can create a container nested
/// two or more `dynamic` levels under an ancestor the overlay already
/// covers (`state.prime.<outer>.<sibling>.<grandchild>`), and a
/// `Frozen` snapshot taken before that container existed can never
/// gain it, no matter how many times it gets rebuilt from a *fresh*
/// chain read afterward -- the snapshot itself is already inert data
/// one level down. `build_overlay` instead builds the overlay as a
/// *detached* [`crate::NodeRef`] whose own [`crate::Slot`]s are cloned
/// (not snapshotted) from *ctx_chain*'s current state plus *local* --
/// a [`crate::Slot::Node`] clone is an `Rc` clone of the live node
/// itself, reproducing Python's own `{**ctx, **local}` dict-literal
/// construction exactly (a new top-level dict, but every value inside
/// it the same object the source held) -- and pushes it as a
/// [`super::CtxSource::Live`] entry, resolved through [`super::
/// live_ctx`] exactly like any other live node. The one wrinkle this
/// keeps faithfully: `build_overlay`'s own `parent = ctx.get("prime")`
/// read happens once, from *ctx_chain* as it stood *before* the current
/// step ran -- so a sibling container the *next* step itself creates
/// still isn't visible to that step's own overlay (Python's own
/// `scope_ctx` has exactly the same gap; `tests/conditional.rs`'s own
/// "second branch step" test pins this down). The branch's own first
/// step runs against *ctx_chain* exactly as received -- never wrapped
/// in an overlay at all, since nothing has been written yet to build
/// one from.
#[allow(clippy::too_many_arguments)]
async fn decide_and_run<'a>(
    op: &'a Op,
    cond: &'a Condition,
    then_: &'a Region,
    else_: &'a Option<Box<Region>>,
    store: &'a Store,
    branch_parent: &'a NodeRef,
    named: Option<(&'a NodeRef, &'a NodeRef)>,
    ctx_chain: &'a CtxChain,
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

    let ctx = super::live_ctx(ctx_chain, store);
    let result = match electricity_cel::evaluate_condition(expr, &ctx, *strict) {
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
    // The branch's own first step runs against *ctx_chain* exactly as
    // received -- still live. From the second step on, `step_chain`
    // holds a single [`CtxSource::Live`] entry pointing at a *detached*
    // overlay node ([`build_overlay`]), rebuilt *from ctx_chain again*
    // (never from the previous overlay) every time -- see
    // `decide_and_run`'s own doc comment above.
    let mut step_chain: CtxChain = ctx_chain.clone();
    let mut executed: Vec<Value> = Vec::new();
    let mut failure: Option<VmError> = None;

    for (index, child_op) in branch_ops.iter().enumerate() {
        match execute_op(
            child_op,
            store,
            branch_parent,
            &step_chain,
            run_ctx,
            observer,
            token,
        )
        .await
        {
            Ok(()) => {
                executed.push(effect_record(child_op, index));
                let local = local_writes(branch_parent, &baseline, &own_names);
                step_chain = vec![CtxSource::Live(build_overlay(ctx_chain, &local))];
            }
            Err(e) => {
                failure = Some(match e {
                    VmError::Cancelled => e,
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
        if matches!(err, VmError::Cancelled) {
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
/// still counts as local to this branch). Returns [`Slot`]s, not a
/// snapshot [`Value`]: a [`Slot::Node`] clones its `Rc`, so a container
/// one of the branch's own steps just created stays the *same* live
/// node once it lands in [`build_overlay`]'s own detached map, exactly
/// as Python's `{key: state[key] for key in ...}` carries the same live
/// dict object forward, never a copy of it.
fn local_writes(
    node: &NodeRef,
    baseline: &HashSet<Value>,
    own_names: &HashSet<Value>,
) -> IndexMap<Value, Slot> {
    node.borrow()
        .iter()
        .filter(|(k, _)| !baseline.contains(*k) || own_names.contains(*k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Every entry of *chain*'s own top-level map, [`Slot`]s rather than
/// resolved values -- [`super::live_ctx`]'s own merge loop, stopping one
/// level short of turning each entry into an owned [`Value`]. A
/// [`CtxSource::Live`] entry's own map is borrowed and cloned directly
/// (a [`Slot::Node`] clone is an `Rc` clone: the live node itself, not a
/// copy of its contents); a [`CtxSource::Frozen`] entry (a tree's own
/// one-time snapshot, DESIGN.md §5.5 -- already a plain `Value` with
/// nothing further live underneath it to preserve) has its own
/// top-level dict entries wrapped as opaque [`Slot::Value`] leaves.
fn chain_slots(chain: &CtxChain) -> IndexMap<Value, Slot> {
    let mut merged: IndexMap<Value, Slot> = IndexMap::new();
    for source in chain {
        match source {
            CtxSource::Live(node) => {
                for (k, v) in node.borrow().iter() {
                    merged.insert(k.clone(), v.clone());
                }
            }
            CtxSource::Frozen(Value::Dict(map)) => {
                for (k, v) in map {
                    merged.insert(k.clone(), Slot::Value(v.clone()));
                }
            }
            CtxSource::Frozen(_) => {}
        }
    }
    merged
}

/// *node*'s own top-level map as [`Slot`]s, or an empty one if *node*
/// isn't a tracked [`Slot::Node`] (nor a plain `Value::Dict` leaf) --
/// [`build_overlay`]'s own `parent = ctx.get("prime")` read.
fn dict_slots(slot: Option<&Slot>) -> IndexMap<Value, Slot> {
    match slot {
        Some(Slot::Node(node)) => node.borrow().clone(),
        Some(Slot::Value(Value::Dict(map))) => map
            .iter()
            .map(|(k, v)| (k.clone(), Slot::Value(v.clone())))
            .collect(),
        _ => IndexMap::new(),
    }
}

/// `core.scope.scope_ctx` -- `{**ctx, **local}`, with *local*
/// additionally merged one level inside `ctx["prime"]` so both the bare
/// (`{{step.value}}`) and `prime`-qualified (`{{prime.step.value}}`)
/// spellings resolve inside the branch -- built as a *detached* node
/// (never attached to *store*'s own tree; nothing but this `if`'s own
/// chain ever points at it), whose map holds cloned [`Slot`]s rather
/// than a deep-copied [`Value`]: Python's own `{**d}`/dict-literal
/// construction allocates a *new* dict object, but every *value* inside
/// it is the exact same object the source dict held -- so a `Slot::Node`
/// clone (an `Rc` clone, the live node itself) reproduces that exactly,
/// where cloning a already-materialized `Value` cannot (that copies the
/// whole subtree, permanently losing liveness below the top level -- the
/// P0 "a nested container freezes its own ancestors' state" finding on
/// PR #440, found again on a second review after `CtxSource::Frozen`
/// alone -- itself still a `Value` under the hood -- turned out to make
/// exactly the same mistake one level later: a branch's *second* step
/// could see a sibling *earlier* step's own write, but a container that
/// sibling itself went on to create nested *two* `dynamic` levels under
/// an already-live ancestor (`prime.<outer>.<sibling>.<grandchild>`)
/// still couldn't, because the whole ancestor subtree had already been
/// snapshotted into inert data one level up). [`super::live_ctx`]
/// resolves straight through a [`CtxSource::Live`] entry pointing at
/// this node exactly as it would any other live node -- a detached node
/// is otherwise an ordinary [`NodeRef`], [`crate::Store::snapshot`]
/// included, it simply isn't reachable from *store*'s own root.
fn build_overlay(chain: &CtxChain, local: &IndexMap<Value, Slot>) -> NodeRef {
    let base = chain_slots(chain);
    let prime_key = key("prime");
    let parent_prime = dict_slots(base.get(&prime_key));

    let mut top = base;
    for (k, v) in local {
        top.insert(k.clone(), v.clone());
    }

    let mut prime = parent_prime;
    for (k, v) in local {
        prime.insert(k.clone(), v.clone());
    }
    top.insert(
        prime_key,
        Slot::Node(std::rc::Rc::new(std::cell::RefCell::new(prime))),
    );

    std::rc::Rc::new(std::cell::RefCell::new(top))
}
