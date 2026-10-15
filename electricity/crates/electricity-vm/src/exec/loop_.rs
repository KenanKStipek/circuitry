//! `loop` effect execution (`core/loop.py::LoopRuntime.execute`) --
//! lane H (issue #461), model-mode `while:` filled in by lane G2
//! (issue #513). Routed from [`crate::exec::execute_op`] once
//! [`electricity_bytecode::refusal::Supported::loop_`] is `true`.
//!
//! `--resume` (C21/C22) and the interactive progress line are both
//! out of scope (M2-G; electricity's non-TTY stdout never prints one)
//! -- every pass here always starts at index 0.

use super::{CtxChain, CtxSource, execute_op, key, normalize_labels, store_err, wrap_effect_error};
use crate::exec::dynamic::now_iso;
use crate::{
    CancellationToken, Frame, NodeRef, RunContext, RunObserver, Slot, Store, VmError,
    node_from_entries, node_with_fallback, node_with_fallback_hiding,
    node_with_fallback_watermarked,
};
use electricity_bytecode::{
    Condition, LoopFlow, LoopId, LoopSpec, OnError, Op, PathSegment, Region,
};
use electricity_tools::{ErrorCause, PyExc};
use electricity_value::Value;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use indexmap::IndexMap;
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;
use std::time::Instant;

/// Accumulates everything a loop run (either mode, either flow) needs
/// to write at the end -- the Rust shape of `core/loop.py::
/// LoopRuntime.execute`'s own local variables (`iteration_count`,
/// `iterations_effects`, `last_completed`, `failed_passes`,
/// `completed_indices`, `termination_reason`, `unvisited`,
/// `termination_detail`).
#[derive(Default)]
struct RunState {
    iteration_count: u32,
    iterations_effects: Vec<Value>,
    last_completed: Option<u32>,
    failed_passes: Vec<u32>,
    completed_indices: Vec<u32>,
    termination_reason: String,
    unvisited: Option<u32>,
    termination_detail: Option<String>,
}

impl RunState {
    fn new() -> Self {
        RunState {
            termination_reason: "max_iterations_reached".to_string(),
            ..Default::default()
        }
    }
}

/// Runs a `loop` effect's own `each:`/`while:` body, chain or tree, as
/// many passes as *spec*/*max_iterations*/*min_iterations* allow. Each
/// pass resolves its own [`electricity_bytecode::LoopId`] placeholder
/// with [`Frame::with_pass`] -- never [`Frame::nested`] -- including a
/// concurrent `flow: tree` pass, whose own concurrent instances that
/// resolution alone already tells apart (`crate::frame`'s own module
/// doc comment).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_loop(
    op: &Op,
    spec: &LoopSpec,
    body: &Region,
    flow: LoopFlow,
    max_concurrency: Option<u32>,
    max_iterations: Option<u32>,
    min_iterations: u32,
    collect: Option<&str>,
    store: &Store,
    parent: &NodeRef,
    ctx_chain: &CtxChain,
    frame: &Frame,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
) -> Result<(), VmError> {
    let loop_name = op.name.as_deref();
    let is_named = loop_name.is_some();

    let (node, meta): (Option<NodeRef>, Option<NodeRef>) = if let Some(name) = loop_name {
        let node = store.ensure_dict(parent, key(name)).map_err(store_err)?;
        // `node.setdefault("value", None)` -- a reused node (a named
        // loop inside an unnamed outer loop) keeps its previous pass's
        // `value` until this run's own write replaces it at the end,
        // the same presence-check convention `execute_tool` uses.
        if !node.borrow().contains_key(&key("value")) {
            store.set_leaf(&node, key("value"), Value::None);
        }
        let meta = store.ensure_dict(&node, key("meta")).map_err(store_err)?;
        write_loop_meta_start(
            store,
            &meta,
            max_iterations,
            min_iterations,
            op.labels.as_ref(),
            spec,
        );
        (Some(node), Some(meta))
    } else {
        (None, None)
    };

    let write_parent: NodeRef = node.clone().unwrap_or_else(|| parent.clone());
    let baseline: HashSet<Value> = write_parent.borrow().keys().cloned().collect();

    let body_ops: &[Op] = match body {
        Region::Block { ops, .. } => ops.as_slice(),
        _ => unreachable!("a loop body is always compiled as Region::Block"),
    };
    let own_names: HashSet<Value> = body_ops
        .iter()
        .filter_map(|o| o.name.clone().map(Value::Str))
        .collect();

    // S8 (M1 gate seam round 2): `Region::Loop` carries no `LoopId` of
    // its own -- the compiler puts the placeholder on the first body
    // op's own path, right after this loop's own path segments
    // (`compile_loop`'s own `loop_path = own_path.push_pass(loop_id)`).
    let loop_id: Option<LoopId> = if is_named {
        body_ops.first().and_then(|first| {
            first.path.0.get(op.path.0.len()).and_then(|seg| match seg {
                PathSegment::Pass(id) => Some(*id),
                PathSegment::Name(_) => None,
            })
        })
    } else {
        None
    };

    let effect_path = frame.effect_path(&op.path);
    let instance = if is_named {
        observer.effect_start(&effect_path)
    } else {
        0
    };

    let t0 = Instant::now();
    let mut state = RunState::new();

    let outcome: Result<(), VmError> = run_loop_body(
        op,
        spec,
        flow,
        max_concurrency,
        max_iterations,
        min_iterations,
        is_named,
        loop_name,
        loop_id,
        &write_parent,
        meta.as_ref(),
        &baseline,
        &own_names,
        body_ops,
        ctx_chain,
        frame,
        store,
        run_ctx,
        observer,
        token,
        t0,
        &mut state,
    )
    .await;

    // `core/loop.py::LoopRuntime.execute`'s own outer `except
    // BaseException` (#574): a cancellation is recorded exactly like any
    // other failure -- `completed_passes`, `completed_at`, and the
    // interrupt text as `meta.error` -- so `--resume` keeps the passes
    // that finished, then propagates unchanged. `VmError::Refused` (#462,
    // lane I) is fatal for *control* only (`VmError::is_fatal`'s own doc
    // comment, ruling R1 on PR #462): it falls through to the same write.
    let error_text: Option<String> = outcome.as_ref().err().map(|err| match err {
        VmError::Cancelled => super::interrupted_text(token),
        other => other.to_string(),
    });

    if let (Some(node), Some(meta)) = (&node, &meta) {
        let termination = build_termination(&state, error_text.as_deref());
        let mut value_map = IndexMap::new();
        value_map.insert(key("iterations"), Value::from(state.iteration_count as i64));
        value_map.insert(key("termination"), termination);
        value_map.insert(
            key("effects_by_iteration"),
            Value::List(state.iterations_effects.clone()),
        );
        store.set_leaf(node, key("value"), Value::Dict(value_map));

        if let Some(err_text) = &error_text {
            store.set_leaf(meta, key("error"), Value::Str(err_text.clone()));
        }
        store.set_leaf(meta, key("completed_at"), Value::Str(now_iso()));

        let mut sorted_completed: Vec<u32> = state.completed_indices.clone();
        sorted_completed.sort_unstable();
        sorted_completed.dedup();
        store.set_leaf(
            meta,
            key("completed_passes"),
            Value::List(
                sorted_completed
                    .into_iter()
                    .map(|i| Value::from(i64::from(i)))
                    .collect(),
            ),
        );
        // `core/loop.py`: the success path pops `failed_passes` when
        // empty; the `except` path only ever sets it (never pops),
        // so a reused node's stale `failed_passes` from an earlier run
        // survives a `LoopBoundsError` under `fail`.
        if state.failed_passes.is_empty() {
            if error_text.is_none() {
                meta.borrow_mut().shift_remove(&key("failed_passes"));
            }
        } else {
            store.set_leaf(
                meta,
                key("failed_passes"),
                Value::List(
                    state
                        .failed_passes
                        .iter()
                        .map(|&i| Value::from(i64::from(i)))
                        .collect(),
                ),
            );
        }

        if let Some(collect_key) = collect {
            let values = collect_values(store, node, collect_key, &state.completed_indices);
            let mut collected_map = IndexMap::new();
            collected_map.insert(key("value"), Value::List(values));
            store.set_leaf(node, key("collected"), Value::Dict(collected_map));
        }
        link_last(store, node, state.last_completed);
    }

    if is_named {
        // Issue #513's totals hook: every named loop counts (any mode,
        // so CEL adds 0) -- `meta.completed_at` is always set by here,
        // a cancellation included (#574).
        if let Some(m) = &meta {
            observer.record_totals(&store.snapshot(m));
        }
        let error_for_complete = meta.as_ref().and_then(|m| node_error(store, m));
        let completed = meta
            .as_ref()
            .is_some_and(|m| super::completed_at_is_truthy(store, m));
        observer.effect_complete(
            &effect_path,
            instance,
            error_for_complete.as_deref(),
            completed,
        );
    }

    outcome
}

/// `meta`'s own current `error` value, if it is a string -- the same
/// `effect_complete` convention `exec::conditional::node_error` uses.
fn node_error(store: &Store, meta: &NodeRef) -> Option<String> {
    match &store.snapshot(meta) {
        Value::Dict(map) => match map.get(&key("error")) {
            Some(Value::Str(text)) => Some(text.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Success-path `value.termination` (`state.termination_reason`/
/// `unvisited`/`termination_detail`) when *error_text* is `None`; the
/// failure-path `{"reason": "error", "detail": ...}` shape otherwise --
/// the failure shape never reuses whatever the try body had already
/// set, exactly matching `core/loop.py::LoopRuntime.execute`'s own
/// `except` block building a fresh termination dict of its own.
fn build_termination(state: &RunState, error_text: Option<&str>) -> Value {
    let mut m = IndexMap::new();
    match error_text {
        Some(text) => {
            m.insert(key("reason"), Value::Str("error".to_string()));
            m.insert(key("detail"), Value::Str(text.to_string()));
        }
        None => {
            m.insert(key("reason"), Value::Str(state.termination_reason.clone()));
            if let Some(u) = state.unvisited {
                m.insert(key("unvisited"), Value::from(i64::from(u)));
            }
            if let Some(d) = &state.termination_detail {
                m.insert(key("detail"), Value::Str(d.clone()));
            }
        }
    }
    Value::Dict(m)
}

/// `meta["created_at"]` through `meta["each_as"]`/`meta["mode"]`
/// (`core/loop.py::LoopRuntime.execute`) -- the keys, in exactly this
/// order, every named loop node's `meta` starts with. `progress`/
/// `completed_passes`/`failed_passes` land later, once the run itself
/// knows what to put in them.
fn write_loop_meta_start(
    store: &Store,
    meta: &NodeRef,
    max_iterations: Option<u32>,
    min_iterations: u32,
    labels: Option<&Value>,
    spec: &LoopSpec,
) {
    store.set_leaf(meta, key("created_at"), Value::Str(now_iso()));
    store.set_leaf(meta, key("completed_at"), Value::None);
    store.set_leaf(meta, key("error"), Value::None);
    if let Some(m) = max_iterations {
        store.set_leaf(meta, key("max_iterations"), Value::from(i64::from(m)));
    }
    store.set_leaf(
        meta,
        key("min_iterations"),
        Value::from(i64::from(min_iterations)),
    );
    store.set_leaf(meta, key("labels"), normalize_labels(labels));
    match spec {
        LoopSpec::Each {
            in_path, as_name, ..
        } => {
            store.set_leaf(meta, key("mode"), Value::Str("each".to_string()));
            store.set_leaf(meta, key("each_in_path"), Value::Str(in_path.clone()));
            store.set_leaf(meta, key("each_as"), Value::Str(as_name.clone()));
        }
        LoopSpec::While(cond) => {
            store.set_leaf(meta, key("mode"), Value::Str(cond.mode().to_string()));
        }
    }
}

/// Python `repr(s)` for a plain string -- every `%r`-formatted warning/
/// error text below.
fn py_repr_str(s: &str) -> String {
    Value::Str(s.to_string()).py_repr()
}

/// `self.defn.name or '<unnamed>'!r` -- the one name spelling every
/// loop warning/error text below uses.
fn name_or_unnamed_repr(name: Option<&str>) -> String {
    py_repr_str(name.unwrap_or("<unnamed>"))
}

/// `%s` on `self.defn.on_error` -- the plain config string a loop's own
/// warning text names. Never called with [`OnError::Skip`] (a loop's
/// own `on_error:` is always `fail`/`break`/`continue`,
/// `compile_loop`'s own `normalize_on_error_loop`).
fn on_error_str(on_error: OnError) -> &'static str {
    match on_error {
        OnError::Fail => "fail",
        OnError::Break => "break",
        OnError::Continue => "continue",
        OnError::Skip => unreachable!("a loop's own on_error is never skip"),
    }
}

/// `core/loop.py::LoopRuntime._resolve_collection`. Returns the
/// resolved list, or the exact error text `_resolve_collection` itself
/// would log (never raises on its own; the caller decides what an
/// unresolved path means for the loop as a whole).
fn resolve_collection(in_path: &str, ctx: &Value) -> Result<Vec<Value>, String> {
    let mut current = ctx;
    for part in in_path.split('.') {
        if part.is_empty() {
            continue;
        }
        match current {
            Value::Dict(map) => match map.get(&key(part)) {
                Some(v) => current = v,
                None => {
                    return Err(format!(
                        "path {} resolved to None at segment {}",
                        py_repr_str(in_path),
                        py_repr_str(part)
                    ));
                }
            },
            _ => {
                return Err(format!(
                    "path {} hit non-dict at segment {}",
                    py_repr_str(in_path),
                    py_repr_str(part)
                ));
            }
        }
        if matches!(current, Value::None) {
            return Err(format!(
                "path {} resolved to None at segment {}",
                py_repr_str(in_path),
                py_repr_str(part)
            ));
        }
    }
    match current {
        Value::List(items) => Ok(items.clone()),
        other => Err(format!(
            "path {} resolved to {} instead of list",
            py_repr_str(in_path),
            py_class_name(other)
        )),
    }
}

/// `type(x).__name__` -- unqualified, unlike [`Value::type_name`]'s
/// `"datetime.date"`/`"datetime.datetime"` (meant for a fuller,
/// Python-repr-style name elsewhere). `_resolve_collection`'s own error
/// text is the only place this lane needs the bare class name.
fn py_class_name(v: &Value) -> &'static str {
    match v {
        Value::Date(_) => "date",
        Value::DateTime(..) => "datetime",
        other => other.type_name(),
    }
}

/// `core/loop.py::LoopBoundsError`'s own exact text, raised (fail) or
/// just recorded and warned about (break/continue) -- never reachable
/// once `each.truncate: true` lets the loop run the first
/// *max_iterations* items instead.
fn loop_bounds_error_text(
    loop_name: Option<&str>,
    in_path: &str,
    collection_len: usize,
    max_iterations: u32,
) -> String {
    format!(
        "each loop {} ({in_path}): collection has {collection_len} items but max_iterations is \
         {max_iterations} \u{2014} raise max_iterations, bound the collection, or set \
         each.truncate: true to process the first {max_iterations} and record the rest as \
         unvisited",
        name_or_unnamed_repr(loop_name)
    )
}

fn handle_loop_bounds_error(
    store: &Store,
    meta: Option<&NodeRef>,
    loop_name: Option<&str>,
    on_error: OnError,
    state: &mut RunState,
    bounds_text: String,
) -> Result<(), VmError> {
    match on_error {
        OnError::Fail => Err(VmError::Raised {
            py_class: PyExc::ValueError,
            message: bounds_text,
            cause: ErrorCause::None,
            cause_class: None,
        }),
        OnError::Break | OnError::Continue => {
            log::warn!(
                "Loop {}: {bounds_text}; on_error={}, stopping the loop",
                name_or_unnamed_repr(loop_name),
                on_error_str(on_error)
            );
            state.termination_reason = "error".to_string();
            state.termination_detail = Some(bounds_text.clone());
            if let Some(m) = meta {
                store.set_leaf(m, key("error"), Value::Str(bounds_text));
            }
            Ok(())
        }
        OnError::Skip => unreachable!("a loop's own on_error is never skip"),
    }
}

/// Dispatches to the `each`/`while` mode this loop actually runs --
/// `core/loop.py::LoopRuntime.execute`'s own `if self.defn.each_def: ...
/// elif self.defn.while_def: ...` branch, including the collection
/// resolution/bounds check that gates an `each` loop before any pass
/// ever starts.
#[allow(clippy::too_many_arguments)]
async fn run_loop_body<'a>(
    op: &'a Op,
    spec: &'a LoopSpec,
    flow: LoopFlow,
    max_concurrency: Option<u32>,
    max_iterations: Option<u32>,
    min_iterations: u32,
    is_named: bool,
    loop_name: Option<&'a str>,
    loop_id: Option<LoopId>,
    write_parent: &'a NodeRef,
    meta: Option<&'a NodeRef>,
    baseline: &'a HashSet<Value>,
    own_names: &'a HashSet<Value>,
    body_ops: &'a [Op],
    ctx_chain: &'a CtxChain,
    frame: &'a Frame,
    store: &'a Store,
    run_ctx: &'a RunContext<'a>,
    observer: &'a dyn RunObserver,
    token: &'a CancellationToken,
    t0: Instant,
    state: &mut RunState,
) -> Result<(), VmError> {
    let on_error = op.on_error;
    match spec {
        LoopSpec::Each {
            in_path,
            as_name,
            truncate,
        } => {
            let ctx = super::live_ctx(ctx_chain, store);
            match resolve_collection(in_path, &ctx) {
                Err(error_text) => {
                    state.termination_reason = "collection_unresolved".to_string();
                    if let Some(m) = meta {
                        store.set_leaf(m, key("each_in_error"), Value::Str(error_text.clone()));
                    }
                    log::warn!("Loop collection {error_text}; running zero iterations");
                    Ok(())
                }
                Ok(collection) if collection.is_empty() => {
                    state.termination_reason = "collection_exhausted".to_string();
                    Ok(())
                }
                Ok(collection) => {
                    if let Some(cap) = max_iterations {
                        if !truncate && collection.len() > cap as usize {
                            let bounds_text =
                                loop_bounds_error_text(loop_name, in_path, collection.len(), cap);
                            return handle_loop_bounds_error(
                                store,
                                meta,
                                loop_name,
                                on_error,
                                state,
                                bounds_text,
                            );
                        }
                    }
                    if flow == LoopFlow::Tree {
                        run_each_tree(
                            op,
                            &collection,
                            as_name,
                            max_concurrency,
                            max_iterations,
                            on_error,
                            is_named,
                            loop_name,
                            loop_id,
                            write_parent,
                            meta,
                            baseline,
                            own_names,
                            body_ops,
                            ctx_chain,
                            frame,
                            store,
                            run_ctx,
                            observer,
                            token,
                            t0,
                            state,
                        )
                        .await
                    } else {
                        run_each_chain(
                            &collection,
                            as_name,
                            max_iterations,
                            on_error,
                            is_named,
                            loop_name,
                            loop_id,
                            write_parent,
                            meta,
                            baseline,
                            own_names,
                            body_ops,
                            ctx_chain,
                            frame,
                            store,
                            run_ctx,
                            observer,
                            token,
                            t0,
                            state,
                        )
                        .await
                    }
                }
            }
        }
        LoopSpec::While(cond) => {
            run_while(
                op,
                cond,
                max_iterations,
                min_iterations,
                on_error,
                is_named,
                loop_name,
                loop_id,
                write_parent,
                meta,
                baseline,
                own_names,
                body_ops,
                ctx_chain,
                frame,
                store,
                run_ctx,
                observer,
                token,
                t0,
                state,
            )
            .await
        }
    }
}

/// The per-pass top-level-only overlay (`as_name`/`_loop_index`/
/// `iter`) -- `{**ctx, as_name: item, "_loop_index": idx, "iter":
/// {"index": idx}}` (`core/loop.py`'s own dict-literal construction for
/// `iter_ctx`): no `prime` mirroring at all, unlike a body step's own
/// post-step overlay ([`step_overlay`]).
fn index_overlay(as_name: Option<&str>, item: Option<&Value>, idx: u32) -> IndexMap<Value, Slot> {
    let mut local = IndexMap::new();
    if let (Some(name), Some(v)) = (as_name, item) {
        local.insert(key(name), Slot::Value(v.clone()));
    }
    local.insert(key("_loop_index"), Slot::Value(Value::from(i64::from(idx))));
    let mut iter_map = IndexMap::new();
    iter_map.insert(key("index"), Value::from(i64::from(idx)));
    local.insert(key("iter"), Slot::Value(Value::Dict(iter_map)));
    local
}

/// Every entry of *chain*'s own top-level map, [`Slot`]s rather than
/// resolved values -- the same flattening `exec::conditional`'s own
/// `chain_slots` does (duplicated here, same reason as before: a tiny
/// function, not worth a module boundary). Flattens through
/// [`crate::store::Node::flat_entries`], not a plain `.iter()`/Deref
/// read of a node's own direct entries -- *chain*'s own `Live` entry
/// can itself be one of this module's own overlay nodes
/// ([`plain_overlay`]'s rare multi-entry/`Frozen` fallback path is the
/// only caller left after #484; issue #501 review finding 1 found the
/// same direct-entry bug here, in the slow path a loop-inside-a-
/// `dynamic`-inside-a-loop body takes).
fn chain_slots(chain: &CtxChain) -> IndexMap<Value, Slot> {
    let mut merged: IndexMap<Value, Slot> = IndexMap::new();
    for source in chain {
        match source {
            CtxSource::Live(node) => {
                for (k, v) in node.borrow().flat_entries() {
                    merged.insert(k, v);
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

/// *slot*'s own top-level map as [`Slot`]s, or an empty one -- the same
/// `dict_slots` helper `exec::conditional::build_overlay` uses for its
/// own `ctx.get("prime")` read, duplicated here for the same reason as
/// [`chain_slots`]. Also [`fallback_for`]'s own wrap of a `Slot::Value`
/// dict leaf that isn't a tracked node -- never one that could itself
/// carry a [`crate::store::Node::fallback`], so [`flat_entries`] is just
/// as correct there as the plain clone it replaces.
fn dict_slots(slot: Option<&Slot>) -> IndexMap<Value, Slot> {
    match slot {
        Some(Slot::Node(node)) => node.borrow().flat_entries(),
        Some(Slot::Value(Value::Dict(map))) => map
            .iter()
            .map(|(k, v)| (k.clone(), Slot::Value(v.clone())))
            .collect(),
        _ => IndexMap::new(),
    }
}

/// *outer*'s own [`Slot`] at *key* as a fallback [`NodeRef`] for a
/// fresh overlay at that same key, or `None` if *outer* doesn't carry
/// one -- [`Store::peek`], so this already follows *outer*'s own
/// fallback chain exactly like any other reader, rather than only ever
/// seeing its direct entries the way [`chain_slots`]/[`dict_slots`] do.
/// A [`Slot::Value`] dict leaf (never promoted to a tracked
/// [`Slot::Node`]) is wrapped in a fresh, fallback-less node, the same
/// shape [`dict_slots`]'s own second arm already gives it.
fn fallback_for(store: &Store, outer: &NodeRef, key: &Value) -> Option<NodeRef> {
    match store.peek(outer, key) {
        Some(Slot::Node(node)) => Some(node),
        Some(other) => Some(node_from_entries(dict_slots(Some(&other)))),
        None => None,
    }
}

/// *entries* alone if *fallback* is `None`, or *entries* overlaid on
/// *fallback* ([`node_with_fallback`]) otherwise -- the one-line
/// branch [`plain_overlay`]/[`with_prev_overlay`]/[`step_overlay`]
/// all share once they've resolved whatever they're layering on top
/// of (an `Option<NodeRef>`, [`fallback_for`]'s own return, or
/// [`build_pass_chain`]'s own single live entry).
fn layered(entries: IndexMap<Value, Slot>, fallback: Option<NodeRef>) -> NodeRef {
    match fallback {
        Some(f) => node_with_fallback(entries, f),
        None => node_from_entries(entries),
    }
}

/// [`layered`], plus a single *hidden* key *fallback* must never expose
/// through the result -- [`with_prev_overlay`]'s own use, to keep the
/// current pass's not-yet-created `iter_<idx>` invisible through its
/// own `prime.<loop_name>` overlay (issue #501 review finding 2). A
/// no-op key if *fallback* is `None`: nothing to hide anything from.
fn layered_hiding(
    entries: IndexMap<Value, Slot>,
    fallback: Option<NodeRef>,
    hidden: Value,
) -> NodeRef {
    match fallback {
        Some(f) => node_with_fallback_hiding(entries, f, hidden),
        None => node_from_entries(entries),
    }
}

/// [`layered`], plus [`crate::store::Node::watermark`] -- *fallback*'s
/// own direct-entry count right now, so a key it gains afterward (a
/// later sibling step's own skeleton node, `exec::tool::execute_tool`
/// `ensure_dict`s one before it ever renders its own params) stays
/// invisible through the result -- [`step_overlay`]'s own use (issue
/// #501's second review). A no-op if *fallback* is `None`.
fn layered_watermarked(entries: IndexMap<Value, Slot>, fallback: Option<NodeRef>) -> NodeRef {
    match fallback {
        Some(f) => {
            let watermark = f.borrow().len();
            node_with_fallback_watermarked(entries, f, watermark)
        }
        None => node_from_entries(entries),
    }
}

/// *chain*'s own single [`CtxSource::Live`] entry, if that's all it
/// has -- [`build_pass_chain`]'s own output, and so [`with_prev_overlay`]/
/// [`step_overlay`]'s own *chain*/*pass_chain* argument, is always
/// shaped exactly this way; [`plain_overlay`]'s own *chain* argument
/// (an enclosing scope's, e.g. a tree dispatch's one-time `Frozen`
/// snapshot alongside a `Live` entry, DESIGN.md §5.5) is the one
/// caller that can't assume it.
fn single_live(chain: &CtxChain) -> Option<&NodeRef> {
    match chain.as_slice() {
        [CtxSource::Live(node)] => Some(node),
        _ => None,
    }
}

/// `{**chain, **local}`, no `prime` mirror -- [`index_overlay`]'s own
/// landing spot. A detached [`NodeRef`], pushed as a [`CtxSource::Live`]
/// entry.
///
/// Issue #484 finding 2 (tree `each`'s own, separate growth): *chain*
/// is the loop's own *outer_chain* -- the enclosing scope, unchanged
/// across every single pass -- so the common case (*chain* is one
/// `Live` entry, true for a loop anywhere but inside a tree dispatch's
/// own `Frozen`-snapshotted branch) layers *local* on top of it
/// directly ([`layered`]) instead of flattening it fresh via
/// [`chain_slots`] every pass: *chain*'s own node can carry an
/// unrelated, large plain [`Value::List`]/[`Value::Dict`] leaf (the
/// `each` loop's own collection itself, read once by [`execute_loop`]
/// but still sitting in scope for the body to reference) that
/// [`chain_slots`]'s own `Slot::clone` would deep-copy in full on
/// every single call -- the real source of tree `each`'s own
/// super-linear growth, measured and explained in this issue's own PR
/// body (chain `each` paid the same cost, on top of the `prev`-overlay
/// one this file's own [`with_prev_overlay`] fixes separately). The
/// rare multi-entry/`Frozen` case keeps the old, eager behaviour.
fn plain_overlay(chain: &CtxChain, local: &IndexMap<Value, Slot>) -> NodeRef {
    if let Some(base) = single_live(chain) {
        return node_with_fallback(local.clone(), Rc::clone(base));
    }
    let mut top = chain_slots(chain);
    for (k, v) in local {
        top.insert(k.clone(), v.clone());
    }
    node_from_entries(top)
}

/// `core/loop.py::LoopRuntime._with_prev` -- overlays
/// `prime.<loop_name>.prev = prev` onto *chain*, two levels deep,
/// keeping every other key already in `prime`/`prime.<loop_name>`
/// untouched. Chain flow only, named loops only, never called before
/// the first completed pass (see [`execute_loop`]'s callers).
///
/// Issue #484: the loop's own real node (two [`Store::peek`] hops down
/// from *chain*'s own entry: `prime`, then this loop's own name) --
/// almost always a tracked [`Slot::Node`], since [`execute_loop`]
/// always `ensure_dict`s it before the first pass ever runs -- already
/// carries every earlier pass's own `iter_<N>`. Flattening *chain* (or
/// even just that one real node) into a brand new top-level map the
/// old way, as this function used to with [`chain_slots`]/
/// [`dict_slots`], clones that whole, ever-growing key set on every
/// pass: O(N) for pass N, O(N²) over a loop of N passes. Layering
/// *prev* as a fresh, one-entry overlay with the real node as its own
/// [`crate::store::Node::fallback`] instead ([`fallback_for`]/
/// [`layered`], all the way up through `prime` and *chain*'s own top
/// level) makes this function O(1) regardless of pass count: nothing
/// here ever holds more than *prev* and the handful of sibling keys a
/// real orchestration keeps at the `prime`/`prime.<loop_name>` level,
/// and [`Store::peek`]/[`Store::snapshot`] -- the two readers
/// [`exec::live_scope`]'s lazy template/CEL resolution and
/// [`super::live_ctx`]'s full materialization alike end up calling
/// against the result -- already know to fall through for every key
/// this function doesn't itself carry (store.rs's own `Node`/`peek`/
/// `snapshot` doc comments).
///
/// *idx* -- the pass about to run -- is hidden from `own`'s own
/// fallback ([`layered_hiding`]) **only if it doesn't already exist
/// there**: `core/loop.py::LoopRuntime._with_prev`'s own `dict(
/// own_node)` copy runs *before* `store.child(iter_<idx>)` creates
/// this pass's own node, so it freezes the real loop node's key SET
/// as of that exact moment -- genuinely absent (the common case: a
/// loop running for the first time) stays absent all pass, even
/// though `own_fallback` below is a live `NodeRef` that will
/// eventually gain that key once [`execute_loop`]'s own caller
/// `ensure_dict`s it right after this returns. But a **reused** real
/// loop node (a named loop inside an unnamed outer loop, or
/// `--state`-resumed persisted state, issue #501's second review's
/// own P0) can already carry `iter_<idx>` from an *earlier* run
/// through this same node -- `dict(own_node)`'s own copy, taken at
/// *this* run's pass start, includes it right along with every other
/// key already there, same as any other already-present key. Checked
/// with one extra [`Store::peek`] (O(1), paid once per pass, not per
/// read) rather than unconditionally hiding it.
fn with_prev_overlay(
    store: &Store,
    chain: &CtxChain,
    loop_name: &str,
    prev: &IndexMap<Value, Slot>,
    idx: u32,
) -> NodeRef {
    let prime_key = key("prime");
    let loop_key = key(loop_name);
    let iter_key = key(&format!("iter_{idx}"));

    let base = single_live(chain).expect(
        "with_prev_overlay is only ever called with build_pass_chain's own \
         single-Live base chain",
    );
    let prime_fallback = fallback_for(store, base, &prime_key);
    let own_fallback = prime_fallback
        .as_ref()
        .and_then(|prime| fallback_for(store, prime, &loop_key));

    let mut own_entries = IndexMap::new();
    own_entries.insert(key("prev"), Slot::Node(node_from_entries(prev.clone())));
    let iter_already_exists = own_fallback
        .as_ref()
        .is_some_and(|f| store.peek(f, &iter_key).is_some());
    let own = if iter_already_exists {
        layered(own_entries, own_fallback)
    } else {
        layered_hiding(own_entries, own_fallback, iter_key)
    };

    let mut prime_entries = IndexMap::new();
    prime_entries.insert(loop_key, Slot::Node(own));
    let prime_overlay = layered(prime_entries, prime_fallback);

    let mut top_entries = IndexMap::new();
    top_entries.insert(prime_key, Slot::Node(prime_overlay));
    layered(top_entries, Some(Rc::clone(base)))
}

/// A pass's own starting ctx chain: the top-level-only *local* overlay
/// ([`index_overlay`]), with `prime.<loop_name>.prev` layered on top
/// when *prev* carries a previous completed pass's writes
/// ([`with_prev_overlay`]). *prev*'s third tuple element is the pass
/// about to run, forwarded to [`with_prev_overlay`] to hide that
/// pass's own not-yet-created `iter_<idx>`.
fn build_pass_chain(
    store: &Store,
    outer_chain: &CtxChain,
    local: &IndexMap<Value, Slot>,
    prev: Option<(&str, &IndexMap<Value, Slot>, u32)>,
) -> CtxChain {
    let plain = plain_overlay(outer_chain, local);
    let base: CtxChain = vec![CtxSource::Live(plain)];
    match prev {
        Some((loop_name, prev_writes, idx)) => {
            vec![CtxSource::Live(with_prev_overlay(
                store,
                &base,
                loop_name,
                prev_writes,
                idx,
            ))]
        }
        None => base,
    }
}

/// `core.scope.scope_ctx` -- `{**ctx, **local}`, with *local* also
/// merged one level inside `ctx["prime"]` -- the within-pass overlay a
/// body step's own later sibling sees (Quirk Q1), rebuilt fresh from
/// *pass_chain* (never from the previous step's own overlay) after
/// every single step. Same shape as `exec::conditional::build_overlay`,
/// duplicated here for the same reason as [`chain_slots`].
///
/// Issue #484: *pass_chain* is always [`build_pass_chain`]'s own
/// single-`Live`-entry output (or, for a `step_chain` rebuilt after an
/// earlier step this same pass, this function's own previous result --
/// still single-entry), so this layers *local* straight onto it
/// ([`fallback_for`]/[`layered_watermarked`]) rather than flattening it
/// fresh via [`chain_slots`]/[`dict_slots`] -- the same
/// per-pass-and-per-step saving [`plain_overlay`]'s own doc comment
/// explains, except this one was already guaranteed safe for every
/// caller (no `Frozen`-chain case ever reaches here).
///
/// Issue #501's second review: `prime_fallback` gets
/// [`crate::store::Node::watermark`]ed to its own entry count *before*
/// *local*'s own step ever ran -- `core/scope.py::scope_ctx`'s own
/// `prime = dict(ctx.get("prime") or {})` takes that exact one-time
/// snapshot of `prime`'s key SET after *every* step, not only a named
/// loop's own `_with_prev`. Without it, a *later* sibling step (not the
/// first in its own scope -- the first step still runs against
/// *pass_chain* directly, unwatermarked, matching cof's own live-shared
/// `ctx["prime"]` there too) could see a sibling that starts *after*
/// this overlay was built the instant that sibling's own node is
/// `ensure_dict`-ed, before it has even rendered its own params --
/// something cof's frozen copy never exposes.
fn step_overlay(store: &Store, pass_chain: &CtxChain, local: &IndexMap<Value, Slot>) -> NodeRef {
    let base = single_live(pass_chain)
        .expect("step_overlay is only ever called with a single-Live pass/step chain");
    let prime_key = key("prime");
    let prime_fallback = fallback_for(store, base, &prime_key);
    let prime_overlay = layered_watermarked(local.clone(), prime_fallback);

    let mut top_entries = local.clone();
    top_entries.insert(prime_key, Slot::Node(prime_overlay));
    layered(top_entries, Some(Rc::clone(base)))
}

/// `core.scope.local_writes` -- *node*'s own top-level keys since
/// *baseline*, plus any of *own_names* even if already present in
/// *baseline*.
fn local_writes_of(
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

/// `{"type": ..., "name": ...}` -- `core/loop.py::LoopRuntime.
/// _execute_body`'s own `effect_record` (no `index`, unlike a
/// conditional branch's own effect record). A disabled op's own record
/// additionally carries `disabled: true` (`core/loop.py:1444-1446`),
/// always last; `count` (the caller's own `executed.len()`) includes
/// it like every other record.
fn body_effect_record(op: &Op) -> Value {
    let mut m = IndexMap::new();
    m.insert(key("type"), Value::from(op.python_type_name()));
    m.insert(
        key("name"),
        match &op.name {
            Some(n) => Value::from(n.as_str()),
            None => Value::None,
        },
    );
    if !op.enabled {
        m.insert(key("disabled"), Value::Bool(true));
    }
    Value::Dict(m)
}

/// `core/loop.py::LoopRuntime._execute_body` -- runs *body_ops* once,
/// in order, against *write_node*, rebuilding the within-pass overlay
/// after every step. Returns this pass's own `{"executed_effects": [...],
/// "count": n}` record and its own local writes (for `prev`/the
/// `while`-condition's own `last_writes`) -- only once every body op has
/// run; any failure (including a cancellation) returns `Err` instead,
/// with no record built at all, matching Python's own "only on actual
/// completion" contract.
#[allow(clippy::too_many_arguments)]
async fn execute_body<'a>(
    body_ops: &'a [Op],
    store: &'a Store,
    write_node: &'a NodeRef,
    pass_chain: &'a CtxChain,
    baseline: &'a HashSet<Value>,
    own_names: &'a HashSet<Value>,
    pass_frame: &'a Frame,
    run_ctx: &'a RunContext<'a>,
    observer: &'a dyn RunObserver,
    token: &'a CancellationToken,
) -> Result<(Value, IndexMap<Value, Slot>), VmError> {
    if token.is_set() {
        return Err(VmError::Cancelled);
    }
    let mut executed: Vec<Value> = Vec::with_capacity(body_ops.len());
    let mut step_chain: CtxChain = pass_chain.clone();
    for child_op in body_ops {
        if token.is_set() {
            return Err(VmError::Cancelled);
        }
        match execute_op(
            child_op,
            store,
            write_node,
            &step_chain,
            pass_frame,
            run_ctx,
            observer,
            token,
        )
        .await
        {
            Ok(()) => {
                executed.push(body_effect_record(child_op));
                let local = local_writes_of(write_node, baseline, own_names);
                step_chain = if local.is_empty() {
                    pass_chain.clone()
                } else {
                    vec![CtxSource::Live(step_overlay(store, pass_chain, &local))]
                };
            }
            Err(e) if e.is_fatal() => return Err(e),
            Err(e) => {
                return Err(match &child_op.name {
                    Some(name) => wrap_effect_error(name, e),
                    None => e,
                });
            }
        }
    }
    let record = {
        let mut m = IndexMap::new();
        m.insert(key("executed_effects"), Value::List(executed.clone()));
        m.insert(key("count"), Value::from(executed.len() as i64));
        Value::Dict(m)
    };
    let writes = local_writes_of(write_node, baseline, own_names);
    Ok((record, writes))
}

/// `core/loop.py::LoopRuntime.execute`'s own chain-flow `each` branch.
#[allow(clippy::too_many_arguments)]
async fn run_each_chain<'a>(
    collection: &'a [Value],
    as_name: &'a str,
    max_iterations: Option<u32>,
    on_error: OnError,
    is_named: bool,
    loop_name: Option<&'a str>,
    loop_id: Option<LoopId>,
    write_parent: &'a NodeRef,
    meta: Option<&'a NodeRef>,
    baseline: &'a HashSet<Value>,
    own_names: &'a HashSet<Value>,
    body_ops: &'a [Op],
    outer_chain: &'a CtxChain,
    frame: &'a Frame,
    store: &'a Store,
    run_ctx: &'a RunContext<'a>,
    observer: &'a dyn RunObserver,
    token: &'a CancellationToken,
    t0: Instant,
    state: &mut RunState,
) -> Result<(), VmError> {
    let total = collection.len() as u32;
    if let Some(m) = meta {
        store.set_leaf(
            m,
            key("progress"),
            loop_progress(0, Some(total), t0.elapsed().as_secs_f64()),
        );
    }
    let mut prev_writes: Option<IndexMap<Value, Slot>> = None;

    for (idx_usize, item) in collection.iter().enumerate() {
        let idx = idx_usize as u32;
        if let Some(cap) = max_iterations {
            if idx >= cap {
                state.termination_reason = "max_iterations_reached".to_string();
                state.unvisited = Some(total - idx);
                return Ok(());
            }
        }
        if token.is_set() {
            return Err(VmError::Cancelled);
        }

        let local = index_overlay(Some(as_name), Some(item), idx);
        let prev_arg = if is_named {
            prev_writes
                .as_ref()
                .map(|p| (loop_name.expect("named"), p, idx))
        } else {
            None
        };
        let pass_chain = build_pass_chain(store, outer_chain, &local, prev_arg);
        let pass_write_node: NodeRef = if is_named {
            store
                .ensure_dict(write_parent, key(&format!("iter_{idx}")))
                .map_err(store_err)?
        } else {
            write_parent.clone()
        };
        // Issue #516 §4(c): a loop body's own children all run one
        // display level deeper than the loop itself, same as an `if`
        // branch's own bump.
        let pass_frame = match loop_id {
            Some(id) => frame.with_pass(id, idx),
            None => frame.clone(),
        }
        .with_display_depth(crate::complexity::depth::branch_or_body_ambient(
            frame.display_depth(),
        ));

        match execute_body(
            body_ops,
            store,
            &pass_write_node,
            &pass_chain,
            baseline,
            own_names,
            &pass_frame,
            run_ctx,
            observer,
            token,
        )
        .await
        {
            Ok((record, writes)) => {
                state.iterations_effects.push(record);
                state.iteration_count += 1;
                state.last_completed = Some(idx);
                state.completed_indices.push(idx);
                prev_writes = Some(writes);
                if let Some(m) = meta {
                    store.set_leaf(
                        m,
                        key("progress"),
                        loop_progress(idx + 1, Some(total), t0.elapsed().as_secs_f64()),
                    );
                }
                observer.write();
            }
            Err(e) if e.is_fatal() => return Err(e),
            Err(e) => {
                state.failed_passes.push(idx);
                if let Some(m) = meta {
                    store.set_leaf(
                        m,
                        key("progress"),
                        loop_progress(idx + 1, Some(total), t0.elapsed().as_secs_f64()),
                    );
                }
                observer.write();
                match on_error {
                    OnError::Fail => {
                        state.termination_reason = "error".to_string();
                        return Err(e);
                    }
                    OnError::Break => {
                        state.termination_reason = "error".to_string();
                        return Ok(());
                    }
                    OnError::Continue => {}
                    OnError::Skip => unreachable!("a loop's own on_error is never skip"),
                }
            }
        }
    }
    state.termination_reason = "collection_exhausted".to_string();
    Ok(())
}

/// `core/loop.py::LoopRuntime.execute`'s own tree-flow `each` branch:
/// one `dispatch` before any branch starts, isolated per-pass stores,
/// merged back in index order only once nothing was cancelled.
#[allow(clippy::too_many_arguments)]
async fn run_each_tree<'a>(
    op: &'a Op,
    collection: &'a [Value],
    as_name: &'a str,
    max_concurrency: Option<u32>,
    max_iterations: Option<u32>,
    on_error: OnError,
    is_named: bool,
    loop_name: Option<&'a str>,
    loop_id: Option<LoopId>,
    write_parent: &'a NodeRef,
    meta: Option<&'a NodeRef>,
    baseline: &'a HashSet<Value>,
    own_names: &'a HashSet<Value>,
    body_ops: &'a [Op],
    outer_chain: &'a CtxChain,
    frame: &'a Frame,
    store: &'a Store,
    run_ctx: &'a RunContext<'a>,
    observer: &'a dyn RunObserver,
    token: &'a CancellationToken,
    t0: Instant,
    state: &mut RunState,
) -> Result<(), VmError> {
    let full_total = collection.len() as u32;
    let capped: &[Value] = match max_iterations {
        Some(cap) if (cap as usize) < collection.len() => &collection[..cap as usize],
        _ => collection,
    };
    let total = capped.len() as u32;

    if let Some(m) = meta {
        store.set_leaf(
            m,
            key("progress"),
            loop_progress(0, Some(full_total), t0.elapsed().as_secs_f64()),
        );
    }

    let effective_concurrency: u32 = match max_concurrency {
        Some(mc) => std::cmp::max(1, std::cmp::min(mc, total)),
        None => total,
    };
    // `core/store/store.py::Store.fire_concurrent_dispatch` (#470): an
    // unnamed loop has no name of its own to append, so its `dispatch`
    // belongs to the enclosing container the passes actually run in --
    // `op.path` already is that path (transparent, no `Name` segment of
    // its own), same as a named loop's own child path is already correct
    // as `effect_path` stands. No `is_named` branch needed here at all.
    let dispatch_path = frame.effect_path(&op.path);
    observer.dispatch(
        &dispatch_path,
        total as usize,
        effective_concurrency as usize,
    );

    if total == 0 {
        if full_total > 0 {
            state.termination_reason = "max_iterations_reached".to_string();
            state.unvisited = Some(full_total);
        } else {
            state.termination_reason = "collection_exhausted".to_string();
        }
        return Ok(());
    }

    let branches = store
        .parallel_branches(write_parent, total as usize)
        .map_err(store_err)?;
    observer.register_tree_branches(&dispatch_path, &branches);

    let mut results: std::collections::BTreeMap<u32, (Value, IndexMap<Value, Slot>)> =
        Default::default();
    let mut errors: std::collections::BTreeMap<u32, VmError> = Default::default();
    // `exec::dynamic::execute_tree`'s own split (finding O1
    // on PR #462): `Cancelled` and a fatal-but-not-`Cancelled` result
    // (`VmError::Refused`) both stop dispatching new passes, like
    // `stop_on_error`, regardless of `on_error` -- but only `Cancelled`
    // skips the merge below entirely (cof's own no-state-write
    // cancellation contract); a `refused` pass still merges like any
    // other `stop_on_error` failure (`VmError::is_fatal`'s own doc
    // comment: fatal for *control*, not for *recording*), then is
    // returned unfolded, never folded into `errors`/the combined
    // multi-failure text the way an ordinary pass failure is.
    let mut interrupted = false;
    let mut refused: Option<VmError> = None;

    {
        let mut pending: VecDeque<u32> = (0..total).collect();
        let mut in_flight = FuturesUnordered::new();

        let launch = |idx: u32| {
            let branch_root = &branches[idx as usize];
            let item = &capped[idx as usize];
            async move {
                if token.is_set() {
                    return (idx, Err(VmError::Cancelled));
                }
                let local = index_overlay(Some(as_name), Some(item), idx);
                let pass_chain = build_pass_chain(store, outer_chain, &local, None);
                let pass_write_node: NodeRef = if is_named {
                    match store.ensure_dict(branch_root, key(&format!("iter_{idx}"))) {
                        Ok(n) => n,
                        Err(e) => return (idx, Err(store_err(e))),
                    }
                } else {
                    branch_root.clone()
                };
                let pass_frame = match loop_id {
                    Some(id) => frame.with_pass(id, idx),
                    None => frame.clone(),
                }
                .with_display_depth(
                    crate::complexity::depth::branch_or_body_ambient(frame.display_depth()),
                );
                let result = execute_body(
                    body_ops,
                    store,
                    &pass_write_node,
                    &pass_chain,
                    baseline,
                    own_names,
                    &pass_frame,
                    run_ctx,
                    observer,
                    token,
                )
                .await;
                observer.write();
                (idx, result)
            }
        };

        let launch_count = std::cmp::min(effective_concurrency as usize, pending.len());
        for _ in 0..launch_count {
            if let Some(idx) = pending.pop_front() {
                in_flight.push(launch(idx));
            }
        }

        while let Some((idx, result)) = in_flight.next().await {
            match result {
                Ok((record, writes)) => {
                    results.insert(idx, (record, writes));
                }
                Err(VmError::Cancelled) => {
                    interrupted = true;
                }
                Err(e) if e.is_fatal() => {
                    refused.get_or_insert(e);
                }
                Err(e) => {
                    errors.insert(idx, e);
                }
            }
            if let Some(m) = meta {
                let done = (results.len() + errors.len()) as u32;
                store.set_leaf(
                    m,
                    key("progress"),
                    loop_progress(done, Some(full_total), t0.elapsed().as_secs_f64()),
                );
            }
            if token.is_set() {
                interrupted = true;
            }
            if interrupted || refused.is_some() {
                pending.clear();
            } else if let Some(next_idx) = pending.pop_front() {
                in_flight.push(launch(next_idx));
            }
        }
    }

    if interrupted {
        // PR #549's review, finding 3: see `exec::dynamic::
        // execute_tree`'s own identical comment -- a cancelled tree
        // loop never merges, but its overlay must still come down here.
        observer.clear_tree_branches(&dispatch_path, &branches);
        return Err(VmError::Cancelled);
    }

    // Cleared *before* the merge below, which takes *branches* by
    // value -- see `exec::dynamic::execute_tree`'s own identical
    // comment.
    observer.clear_tree_branches(&dispatch_path, &branches);
    // A refused branch still merges like any other `stop_on_error`
    // failure -- every in-flight branch has already drained above, so
    // there is nothing left to lose by keeping whatever every branch
    // (refused or not) already wrote.
    store.merge(write_parent, branches).map_err(store_err)?;
    observer.write();

    for idx in 0..total {
        if let Some((record, _writes)) = results.get(&idx) {
            state.iterations_effects.push(record.clone());
            state.iteration_count += 1;
            state.last_completed = Some(idx);
            state.completed_indices.push(idx);
        }
    }

    if let Some(e) = refused {
        return Err(e);
    }

    if total < full_total {
        state.termination_reason = "max_iterations_reached".to_string();
        state.unvisited = Some(full_total - total);
    } else {
        state.termination_reason = "collection_exhausted".to_string();
    }

    if !errors.is_empty() {
        state.failed_passes = errors.keys().copied().collect();
        match on_error {
            OnError::Fail => {
                state.termination_reason = "error".to_string();
                let lowest_idx = *errors.keys().next().expect("errors is non-empty");
                let failing = &errors[&lowest_idx];
                let cause_class = failing.displayed_class();
                let message = format!(
                    "loop {} iteration [{lowest_idx}]: {failing}",
                    name_or_unnamed_repr(loop_name)
                );
                return Err(VmError::Raised {
                    py_class: PyExc::RuntimeError,
                    message,
                    cause: ErrorCause::None,
                    cause_class,
                });
            }
            OnError::Break => {
                state.termination_reason = "error".to_string();
            }
            OnError::Continue => {}
            OnError::Skip => unreachable!("a loop's own on_error is never skip"),
        }
    }
    Ok(())
}

/// `core/loop.py::LoopRuntime.execute`'s own `while` branch -- CEL or
/// model mode (lane G2/#513), via a `Condition::Model` arm below.
#[allow(clippy::too_many_arguments)]
async fn run_while<'a>(
    op: &'a Op,
    cond: &'a Condition,
    max_iterations: Option<u32>,
    min_iterations: u32,
    on_error: OnError,
    is_named: bool,
    loop_name: Option<&'a str>,
    loop_id: Option<LoopId>,
    write_parent: &'a NodeRef,
    meta: Option<&'a NodeRef>,
    baseline: &'a HashSet<Value>,
    own_names: &'a HashSet<Value>,
    body_ops: &'a [Op],
    outer_chain: &'a CtxChain,
    frame: &'a Frame,
    store: &'a Store,
    run_ctx: &'a RunContext<'a>,
    observer: &'a dyn RunObserver,
    token: &'a CancellationToken,
    t0: Instant,
    state: &mut RunState,
) -> Result<(), VmError> {
    let mut iteration_count: u32 = 0;
    let mut last_writes: IndexMap<Value, Slot> = IndexMap::new();
    let mut prev_writes: Option<IndexMap<Value, Slot>> = None;
    // `_model_tokens_sent_total`/`_model_tokens_received_total`
    // (`core/loop.py::LoopRuntime.__init__`'s own doc comment) --
    // local to this one `run_while` call, so a fresh `execute_loop`
    // invocation (every outer pass of an enclosing loop) starts them
    // over, same as Python's own reset at the top of `execute()`.
    // `None` until the first check whose own count isn't `None`.
    let mut tokens_sent_total: Option<i64> = None;
    let mut tokens_received_total: Option<i64> = None;

    if let Some(m) = meta {
        store.set_leaf(
            m,
            key("progress"),
            loop_progress(iteration_count, max_iterations, t0.elapsed().as_secs_f64()),
        );
    }

    loop {
        if let Some(cap) = max_iterations {
            if iteration_count >= cap {
                break;
            }
        }

        let should_continue = if iteration_count < min_iterations {
            true
        } else {
            if token.is_set() {
                return Err(VmError::Cancelled);
            }
            let mut iter_map = IndexMap::new();
            iter_map.insert(key("index"), Value::from(i64::from(iteration_count) - 1));
            iter_map.insert(key("count"), Value::from(i64::from(iteration_count)));
            let mut cond_local = IndexMap::new();
            cond_local.insert(key("iter"), Slot::Value(Value::Dict(iter_map)));
            // `{**ctx, "iter": {...}}` -- a top-level-only overlay, no
            // `prime` mirror, unlike the body's own `step_overlay`
            // (Q1): Python builds this dict literally, never through
            // `scope_ctx`. `last_writes` is layered on top of *this*
            // with `step_overlay` below, which *is* `scope_ctx` and
            // does mirror into `prime` -- matching `core/loop.py`'s own
            // `scope_ctx({**ctx, "iter": ...}, last_writes)`.
            let cond_base: CtxChain =
                vec![CtxSource::Live(plain_overlay(outer_chain, &cond_local))];
            let cond_chain: CtxChain = if last_writes.is_empty() {
                cond_base
            } else {
                vec![CtxSource::Live(step_overlay(
                    store,
                    &cond_base,
                    &last_writes,
                ))]
            };
            match cond {
                Condition::Cel { expr, strict } => {
                    match evaluate_while_condition(expr, *strict, &cond_chain, store) {
                        Ok(b) => b,
                        Err(e) => match on_error {
                            OnError::Fail => {
                                state.termination_reason = "error".to_string();
                                state.iteration_count = iteration_count;
                                return Err(e);
                            }
                            OnError::Break | OnError::Continue => {
                                log::warn!(
                                    "Loop {}: while-condition failed ({e}); on_error={}, stopping the loop",
                                    name_or_unnamed_repr(loop_name),
                                    on_error_str(on_error)
                                );
                                state.termination_reason = "condition_error".to_string();
                                if let Some(m) = meta {
                                    store.set_leaf(m, key("error"), Value::Str(e.to_string()));
                                }
                                break;
                            }
                            OnError::Skip => unreachable!("a loop's own on_error is never skip"),
                        },
                    }
                }
                // `condition_model::evaluate_model_condition` -- issue
                // #513. Written after every check, success or failure,
                // for a named loop only (`write_model_while_reply`'s
                // own doc comment) -- unlike a model `if`, there is no
                // "write after the decision" ordering quirk here: the
                // 7 keys land in the same place regardless of outcome.
                Condition::Model { template } => {
                    let ctx = super::live_ctx(&cond_chain, store);
                    let effect_path = frame.effect_path(&op.path);
                    let outcome = super::condition_model::evaluate_model_condition(
                        super::condition_model::ConditionKind::While,
                        template,
                        &ctx,
                        &effect_path,
                        run_ctx,
                        token,
                    )
                    .await?;
                    if let Some(ts) = outcome.generated.as_ref().and_then(|g| g.tokens_sent) {
                        tokens_sent_total = Some(tokens_sent_total.unwrap_or(0) + ts);
                    }
                    if let Some(tr) = outcome.generated.as_ref().and_then(|g| g.tokens_received) {
                        tokens_received_total = Some(tokens_received_total.unwrap_or(0) + tr);
                    }
                    if let Some(m) = meta {
                        write_model_while_reply(
                            store,
                            m,
                            run_ctx,
                            outcome.generated.as_ref(),
                            tokens_sent_total,
                            tokens_received_total,
                        );
                    }
                    match outcome.verdict {
                        Ok(b) => b,
                        Err(failure) => {
                            // The failure's own class (`TemplateError` for a
                            // render failure, the adapter's own class for a
                            // dispatch failure, `AnswerParseError` for an
                            // unparseable reply) survives here, never a
                            // bare `VmError::Message`.
                            let e = VmError::Raised {
                                py_class: failure.py_class,
                                message: failure.message,
                                cause: electricity_tools::ErrorCause::None,
                                cause_class: None,
                            };
                            match on_error {
                                OnError::Fail => {
                                    state.termination_reason = "error".to_string();
                                    state.iteration_count = iteration_count;
                                    return Err(e);
                                }
                                OnError::Break | OnError::Continue => {
                                    log::warn!(
                                        "Loop {}: while-condition failed ({e}); on_error={}, stopping the loop",
                                        name_or_unnamed_repr(loop_name),
                                        on_error_str(on_error)
                                    );
                                    state.termination_reason = "condition_error".to_string();
                                    if let Some(m) = meta {
                                        store.set_leaf(m, key("error"), Value::Str(e.to_string()));
                                    }
                                    break;
                                }
                                OnError::Skip => {
                                    unreachable!("a loop's own on_error is never skip")
                                }
                            }
                        }
                    }
                }
            }
        };

        if !should_continue && iteration_count >= min_iterations {
            state.termination_reason = "condition_false".to_string();
            break;
        }

        if token.is_set() {
            return Err(VmError::Cancelled);
        }

        let local = index_overlay(None, None, iteration_count);
        let prev_arg = if is_named {
            prev_writes
                .as_ref()
                .map(|p| (loop_name.expect("named"), p, iteration_count))
        } else {
            None
        };
        let pass_chain = build_pass_chain(store, outer_chain, &local, prev_arg);
        let pass_write_node: NodeRef = if is_named {
            store
                .ensure_dict(write_parent, key(&format!("iter_{iteration_count}")))
                .map_err(store_err)?
        } else {
            write_parent.clone()
        };
        let pass_frame = match loop_id {
            Some(id) => frame.with_pass(id, iteration_count),
            None => frame.clone(),
        }
        .with_display_depth(crate::complexity::depth::branch_or_body_ambient(
            frame.display_depth(),
        ));

        match execute_body(
            body_ops,
            store,
            &pass_write_node,
            &pass_chain,
            baseline,
            own_names,
            &pass_frame,
            run_ctx,
            observer,
            token,
        )
        .await
        {
            Ok((record, writes)) => {
                state.iterations_effects.push(record);
                state.last_completed = Some(iteration_count);
                state.completed_indices.push(iteration_count);
                iteration_count += 1;
                prev_writes = Some(writes.clone());
                last_writes = writes;
                if let Some(m) = meta {
                    store.set_leaf(
                        m,
                        key("progress"),
                        loop_progress(iteration_count, max_iterations, t0.elapsed().as_secs_f64()),
                    );
                }
                observer.write();
            }
            Err(e) if e.is_fatal() => return Err(e),
            Err(e) => {
                state.failed_passes.push(iteration_count);
                if let Some(m) = meta {
                    store.set_leaf(
                        m,
                        key("progress"),
                        loop_progress(
                            iteration_count + 1,
                            max_iterations,
                            t0.elapsed().as_secs_f64(),
                        ),
                    );
                }
                observer.write();
                match on_error {
                    OnError::Fail => {
                        state.termination_reason = "error".to_string();
                        state.iteration_count = iteration_count;
                        return Err(e);
                    }
                    OnError::Break => {
                        state.termination_reason = "error".to_string();
                        break;
                    }
                    OnError::Continue => {
                        iteration_count += 1;
                    }
                    OnError::Skip => unreachable!("a loop's own on_error is never skip"),
                }
            }
        }
    }

    if state.termination_reason == "max_iterations_reached" {
        log::warn!(
            "Loop {}: stopped after {iteration_count} iterations because max_iterations ({}) \
             was reached, not because the while-condition became false",
            name_or_unnamed_repr(loop_name),
            max_iterations.unwrap_or(0)
        );
    }
    state.iteration_count = iteration_count;
    Ok(())
}

/// Evaluates a CEL `while`-condition and reports the boolean verdict.
/// `run_while`'s own outer `match cond` calls this only for
/// `Condition::Cel` -- a `Condition::Model` check is handled directly
/// there instead, since it needs to write this loop's own `meta`
/// (`write_model_while_reply`) after every check, something this
/// CEL-only helper has no occasion to do.
///
/// Issue #463: *state* is only the subtrees *expr* actually reads,
/// walked live, not a whole-tree `live_ctx` snapshot of *cond_chain*
/// -- see `live_scope::build_cel_state`'s own doc comment for why
/// this is exactly `evaluate_condition`'s own internal narrowing,
/// just performed before the snapshot rather than after it.
fn evaluate_while_condition(
    expr: &str,
    strict: bool,
    cond_chain: &CtxChain,
    store: &Store,
) -> Result<bool, VmError> {
    let state = super::live_scope::build_cel_state(cond_chain, store, expr);
    // `core/cel_eval.py::CelEvaluationError` propagates bare, same as
    // the `if` CEL arm (`exec::conditional::decide_and_run`) -- never a
    // bare `VmError::Message`.
    electricity_cel::evaluate_condition(expr, &state, strict).map_err(|e| VmError::Raised {
        py_class: electricity_tools::PyExc::CelEvaluationError,
        message: e.to_string(),
        cause: electricity_tools::ErrorCause::None,
        cause_class: None,
    })
}

/// `meta["answer"]` through `meta["tokens_received_total"]`, in that
/// order -- the 7 keys a named model-mode `while`'s own
/// `_evaluate_model` writes after *every* check, success or failure
/// (issue #513 Scope §2) -- unlike a model `if`, there is no
/// write-order quirk tied to the verdict: these always land right
/// after `mode`/`progress`. *generated* is `None` for a render
/// failure or an adapter failure (`answer`/`tokens_sent`/
/// `tokens_received` are `null` then, but the `_total` fields keep
/// whatever they already summed from earlier checks); *tokens_sent_
/// total*/*tokens_received_total* are the running sums [`run_while`]
/// keeps across this call's own checks.
fn write_model_while_reply(
    store: &Store,
    meta: &NodeRef,
    run_ctx: &RunContext<'_>,
    generated: Option<&crate::adapter::GenerateResult>,
    tokens_sent_total: Option<i64>,
    tokens_received_total: Option<i64>,
) {
    store.set_leaf(
        meta,
        key("answer"),
        generated
            .map(|g| Value::Str(g.text.clone()))
            .unwrap_or(Value::None),
    );
    store.set_leaf(
        meta,
        key("adapter"),
        Value::Str(run_ctx.default_adapter.name().to_string()),
    );
    store.set_leaf(meta, key("model"), Value::Str(run_ctx.model.to_string()));
    store.set_leaf(
        meta,
        key("tokens_sent"),
        generated
            .and_then(|g| g.tokens_sent)
            .map(Value::from)
            .unwrap_or(Value::None),
    );
    store.set_leaf(
        meta,
        key("tokens_received"),
        generated
            .and_then(|g| g.tokens_received)
            .map(Value::from)
            .unwrap_or(Value::None),
    );
    store.set_leaf(
        meta,
        key("tokens_sent_total"),
        tokens_sent_total.map(Value::from).unwrap_or(Value::None),
    );
    store.set_leaf(
        meta,
        key("tokens_received_total"),
        tokens_received_total
            .map(Value::from)
            .unwrap_or(Value::None),
    );
}

/// `core/loop.py::LoopRuntime._collect_values` -- reads only
/// *completed_indices* (this call's own record of which passes
/// finished), never by scanning *node* for stale `iter_<N>` keys left
/// over from an earlier run on a reused node. A disabled collect target
/// (`meta.disabled` exactly `true`, `core/loop.py:1190`; `core/
/// disabled.py::is_disabled_node`) is left out entirely, same as a pass
/// that failed before the target ever produced a value -- never a
/// `None` entry standing in for it (issue #518, lane J2).
fn collect_values(
    store: &Store,
    node: &NodeRef,
    collect_key: &str,
    completed_indices: &[u32],
) -> Vec<Value> {
    let mut out = Vec::with_capacity(completed_indices.len());
    for &i in completed_indices {
        let Ok(Some(iter_node)) = store.child(node, &key(&format!("iter_{i}"))) else {
            continue;
        };
        let Ok(Some(effect_node)) = store.child(&iter_node, &key(collect_key)) else {
            continue;
        };
        let snapshot = store.snapshot(&effect_node);
        let Value::Dict(ref map) = snapshot else {
            continue;
        };
        let is_disabled = matches!(
            map.get(&key("meta"))
                .and_then(Value::as_dict)
                .and_then(|m| m.get(&key("disabled"))),
            Some(Value::Bool(true))
        );
        if is_disabled {
            continue;
        }
        out.push(map.get(&key("value")).cloned().unwrap_or(Value::None));
    }
    out
}

/// `core/loop.py::LoopRuntime._link_last` -- `last` aliases the final
/// *completed* pass's own `iter_<N>` node, with no copy; absent
/// entirely when no pass ever completed.
fn link_last(store: &Store, node: &NodeRef, last_completed: Option<u32>) {
    if let Some(idx) = last_completed {
        let target = key(&format!("iter_{idx}"));
        let _ = store.alias(node, key("last"), &target);
    }
}

/// `core/loop.py::_loop_progress` -- `meta.progress`'s own
/// `{done, total, elapsed_s, eta_s}` shape, recomputed after every pass
/// (failed ones included).
pub fn loop_progress(done: u32, total: Option<u32>, elapsed_s: f64) -> Value {
    let eta = match total {
        Some(t) if done > 0 => {
            let remaining = t.saturating_sub(done);
            Some((elapsed_s / f64::from(done)) * f64::from(remaining))
        }
        _ => None,
    };
    let mut m = IndexMap::new();
    m.insert(key("done"), Value::from(i64::from(done)));
    m.insert(
        key("total"),
        match total {
            Some(t) => Value::from(i64::from(t)),
            None => Value::None,
        },
    );
    m.insert(key("elapsed_s"), Value::Float(elapsed_s));
    m.insert(
        key("eta_s"),
        match eta {
            Some(e) => Value::Float(e),
            None => Value::None,
        },
    );
    Value::Dict(m)
}

/// Issue #484's own REQUIRED differential test: for a representative
/// set of loop-shaped store trees, the *old* ctx-overlay construction
/// (this module's own `old_*` helpers below -- a direct port of this
/// file's own `with_prev_overlay`/`plain_overlay`/`step_overlay` from
/// before this issue's fix, eagerly flattening/cloning every level
/// instead of layering a [`crate::store::Node::fallback`]) and the
/// *new* one (this file's own, current `with_prev_overlay`/
/// `plain_overlay`/`step_overlay`/`build_pass_chain`) must resolve
/// every path identically: a full [`Store::snapshot`] of the built
/// chain, and a battery of individual dotted-path reads covering
/// `prev`, `last`, a specific `iter_<N>`, a sibling under `prime`, the
/// outer scope's own `input`, and the pass-local `item`/`iter`
/// bindings. Covers chain `each`/`while` (mechanically identical here:
/// both just call `with_prev_overlay` with this pass's own `prev`),
/// two named loops chained one's own ctx construction on top of the
/// other's (the multi-level-fallback shape a loop nested inside
/// another loop's body builds), a step overlay within a pass, and a
/// loop running against a `use` child's own, separate `Store` (the
/// mechanism is store-instance-independent, but kept as its own case
/// for the issue's own required test list).
#[cfg(test)]
mod prev_overlay_differential_tests {
    use super::*;

    fn k(s: &str) -> Value {
        Value::Str(s.to_string())
    }

    fn old_chain_slots(chain: &CtxChain) -> IndexMap<Value, Slot> {
        let mut merged: IndexMap<Value, Slot> = IndexMap::new();
        for source in chain {
            match source {
                CtxSource::Live(node) => {
                    for (key, slot) in node.borrow().entries().iter() {
                        merged.insert(key.clone(), slot.clone());
                    }
                }
                CtxSource::Frozen(Value::Dict(map)) => {
                    for (key, value) in map {
                        merged.insert(key.clone(), Slot::Value(value.clone()));
                    }
                }
                CtxSource::Frozen(_) => {}
            }
        }
        merged
    }

    fn old_dict_slots(slot: Option<&Slot>) -> IndexMap<Value, Slot> {
        match slot {
            Some(Slot::Node(node)) => node.borrow().entries().clone(),
            Some(Slot::Value(Value::Dict(map))) => map
                .iter()
                .map(|(key, value)| (key.clone(), Slot::Value(value.clone())))
                .collect(),
            _ => IndexMap::new(),
        }
    }

    fn old_plain_overlay(chain: &CtxChain, local: &IndexMap<Value, Slot>) -> NodeRef {
        let mut top = old_chain_slots(chain);
        for (key, value) in local {
            top.insert(key.clone(), value.clone());
        }
        node_from_entries(top)
    }

    fn old_with_prev_overlay(
        chain: &CtxChain,
        loop_name: &str,
        prev: &IndexMap<Value, Slot>,
    ) -> NodeRef {
        let mut top = old_chain_slots(chain);
        let prime_key = key("prime");
        let mut prime = old_dict_slots(top.get(&prime_key));
        let loop_key = key(loop_name);
        let mut own = old_dict_slots(prime.get(&loop_key));
        own.insert(key("prev"), Slot::Node(node_from_entries(prev.clone())));
        prime.insert(loop_key, Slot::Node(node_from_entries(own)));
        top.insert(prime_key, Slot::Node(node_from_entries(prime)));
        node_from_entries(top)
    }

    fn old_build_pass_chain(
        outer_chain: &CtxChain,
        local: &IndexMap<Value, Slot>,
        prev: Option<(&str, &IndexMap<Value, Slot>)>,
    ) -> CtxChain {
        let plain = old_plain_overlay(outer_chain, local);
        let base: CtxChain = vec![CtxSource::Live(plain)];
        match prev {
            Some((loop_name, prev_writes)) => {
                vec![CtxSource::Live(old_with_prev_overlay(
                    &base,
                    loop_name,
                    prev_writes,
                ))]
            }
            None => base,
        }
    }

    fn old_step_overlay(pass_chain: &CtxChain, local: &IndexMap<Value, Slot>) -> NodeRef {
        let base = old_chain_slots(pass_chain);
        let prime_key = key("prime");
        let parent_prime = old_dict_slots(base.get(&prime_key));

        let mut top = base;
        for (key, value) in local {
            top.insert(key.clone(), value.clone());
        }
        let mut prime = parent_prime;
        for (key, value) in local {
            prime.insert(key.clone(), value.clone());
        }
        top.insert(prime_key, Slot::Node(node_from_entries(prime)));
        node_from_entries(top)
    }

    /// A loop-shaped store: `prime.shots.iter_0`/`iter_1` (two finished
    /// passes), `prime.shots.meta`, a sibling `prime.other`, and a
    /// top-level `input` -- `prev`/`last`/an outer scope's own key/a
    /// sibling's own key are all exercised against the same store.
    fn loop_shaped_store() -> Store {
        let store = Store::new();
        let prime = store.ensure_dict(&store.root, k("prime")).unwrap();
        let shots = store.ensure_dict(&prime, k("shots")).unwrap();
        let iter0 = store.ensure_dict(&shots, k("iter_0")).unwrap();
        store.set_leaf(&iter0, k("echo"), Value::from("zero"));
        let iter1 = store.ensure_dict(&shots, k("iter_1")).unwrap();
        store.set_leaf(&iter1, k("echo"), Value::from("one"));
        store.alias(&shots, k("last"), &k("iter_1")).unwrap();
        let meta = store.ensure_dict(&shots, k("meta")).unwrap();
        store.set_leaf(&meta, k("mode"), Value::from("each"));

        let other = store.ensure_dict(&prime, k("other")).unwrap();
        store.set_leaf(&other, k("value"), Value::from("sibling"));

        store.set_leaf(&store.root, k("input"), Value::from("hi"));
        store
    }

    fn prev_writes(value: &str) -> IndexMap<Value, Slot> {
        let mut m = IndexMap::new();
        m.insert(k("echo"), Slot::Value(Value::from(value)));
        m
    }

    fn outer_chain(store: &Store) -> CtxChain {
        vec![CtxSource::Live(Rc::clone(&store.root))]
    }

    fn local_overlay(idx: i64) -> IndexMap<Value, Slot> {
        let mut local = IndexMap::new();
        local.insert(k("item"), Slot::Value(Value::from(idx)));
        local.insert(k("_loop_index"), Slot::Value(Value::from(idx)));
        let mut iter_map = IndexMap::new();
        iter_map.insert(k("index"), Value::from(idx));
        local.insert(k("iter"), Slot::Value(Value::Dict(iter_map)));
        local
    }

    fn walk<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
        let mut current = value;
        for part in path.split('.') {
            match current {
                Value::Dict(d) => current = d.get(&k(part))?,
                _ => return None,
            }
        }
        Some(current)
    }

    fn chain_node(chain: &CtxChain) -> NodeRef {
        match chain
            .last()
            .expect("a built pass/step chain is never empty")
        {
            CtxSource::Live(node) => Rc::clone(node),
            CtxSource::Frozen(_) => unreachable!("this test never builds a Frozen chain"),
        }
    }

    /// Every probe path this module's own cases care about: `prev`/
    /// `last`/a specific `iter_<N>`/`meta` under the loop's own node,
    /// a sibling under `prime`, the outer scope's own `input`, and the
    /// pass-local `item`/`iter` bindings.
    const PROBE_PATHS: &[&str] = &[
        "prime.shots.prev.echo",
        "prime.shots.last.echo",
        "prime.shots.iter_0.echo",
        "prime.shots.meta.mode",
        "prime.other.value",
        "input",
        "item",
        "iter.index",
    ];

    fn assert_same_chain(store: &Store, old: &CtxChain, new: &CtxChain, case: &str) {
        let old_snapshot = store.snapshot(&chain_node(old));
        let new_snapshot = store.snapshot(&chain_node(new));
        assert_eq!(
            old_snapshot, new_snapshot,
            "{case}: whole-chain snapshot diverged"
        );
        for path in PROBE_PATHS {
            assert_eq!(
                walk(&old_snapshot, path),
                walk(&new_snapshot, path),
                "{case}: path {path:?} diverged"
            );
        }
    }

    #[test]
    fn chain_each_or_while_pass_with_prev() {
        let store = loop_shaped_store();
        let outer = outer_chain(&store);
        let local = local_overlay(2);
        let prev = prev_writes("one-stringified");

        let old = old_build_pass_chain(&outer, &local, Some(("shots", &prev)));
        let new = build_pass_chain(&store, &outer, &local, Some(("shots", &prev, 2)));

        assert_same_chain(&store, &old, &new, "chain each/while, prev present");
    }

    #[test]
    fn chain_each_or_while_first_pass_has_no_prev() {
        let store = loop_shaped_store();
        let outer = outer_chain(&store);
        let local = local_overlay(0);

        let old = old_build_pass_chain(&outer, &local, None);
        let new = build_pass_chain(&store, &outer, &local, None);

        assert_same_chain(
            &store,
            &old,
            &new,
            "chain each/while, first pass (no prev yet)",
        );
    }

    #[test]
    fn step_overlay_within_a_pass() {
        let store = loop_shaped_store();
        let outer = outer_chain(&store);
        let local = local_overlay(2);
        let prev = prev_writes("one-stringified");

        let old_pass = old_build_pass_chain(&outer, &local, Some(("shots", &prev)));
        let new_pass = build_pass_chain(&store, &outer, &local, Some(("shots", &prev, 2)));

        let mut step_local = IndexMap::new();
        step_local.insert(k("first_step"), Slot::Value(Value::from("done")));

        let old_step: CtxChain = vec![CtxSource::Live(old_step_overlay(&old_pass, &step_local))];
        let new_step: CtxChain = vec![CtxSource::Live(step_overlay(
            &store,
            &new_pass,
            &step_local,
        ))];

        assert_same_chain(
            &store,
            &old_step,
            &new_step,
            "step overlay after one body step",
        );

        let old_snapshot = store.snapshot(&chain_node(&old_step));
        let new_snapshot = store.snapshot(&chain_node(&new_step));
        for path in ["first_step", "prime.first_step"] {
            assert_eq!(
                walk(&old_snapshot, path),
                walk(&new_snapshot, path),
                "step overlay: path {path:?} diverged"
            );
        }
    }

    /// Two named loops, chained: the second's own ctx construction
    /// (`with_prev_overlay`) built directly on top of the first's own
    /// result -- the multi-level-fallback shape a loop nested inside
    /// another loop's body (or two independent named loops in the same
    /// scope) builds. Both loops' own `prev` must stay independently
    /// visible, and both loops' own `iter_<N>`/`meta` through either
    /// one's own overlay.
    #[test]
    fn two_named_loops_chained_each_with_its_own_independent_prev() {
        let store = loop_shaped_store();
        let prime = store.ensure_dict(&store.root, k("prime")).unwrap();
        let widgets = store.ensure_dict(&prime, k("widgets")).unwrap();
        let w_iter0 = store.ensure_dict(&widgets, k("iter_0")).unwrap();
        store.set_leaf(&w_iter0, k("ping"), Value::from("w0"));

        let outer = outer_chain(&store);
        let shots_local = local_overlay(2);
        let shots_prev = prev_writes("one-stringified");
        let widgets_local = local_overlay(1);
        let widgets_prev = {
            let mut m = IndexMap::new();
            m.insert(k("ping"), Slot::Value(Value::from("w0-stringified")));
            m
        };

        let old_shots = old_build_pass_chain(&outer, &shots_local, Some(("shots", &shots_prev)));
        let old_both =
            old_build_pass_chain(&old_shots, &widgets_local, Some(("widgets", &widgets_prev)));

        let new_shots = build_pass_chain(
            &store,
            &outer,
            &shots_local,
            Some(("shots", &shots_prev, 2)),
        );
        let new_both = build_pass_chain(
            &store,
            &new_shots,
            &widgets_local,
            Some(("widgets", &widgets_prev, 1)),
        );

        let old_snapshot = store.snapshot(&chain_node(&old_both));
        let new_snapshot = store.snapshot(&chain_node(&new_both));
        assert_eq!(
            old_snapshot, new_snapshot,
            "two chained named loops: whole-chain snapshot diverged"
        );
        for path in [
            "prime.shots.prev.echo",
            "prime.shots.iter_0.echo",
            "prime.widgets.prev.ping",
            "prime.widgets.iter_0.ping",
            "prime.other.value",
            "input",
            "item",
            "iter.index",
        ] {
            assert_eq!(
                walk(&old_snapshot, path),
                walk(&new_snapshot, path),
                "two chained named loops: path {path:?} diverged"
            );
        }
    }

    /// A loop running against a `use` child's own, separate `Store`
    /// (`exec::use_::execute_use`'s own `child_store`, a fresh,
    /// independent tree -- never *store*'s own) -- proves the fix is
    /// store-instance-independent, the same shape a loop inside a
    /// `use` child's own body actually runs against.
    #[test]
    fn loop_inside_a_use_childs_own_store() {
        let child_store = loop_shaped_store();
        let outer = outer_chain(&child_store);
        let local = local_overlay(2);
        let prev = prev_writes("one-stringified");

        let old = old_build_pass_chain(&outer, &local, Some(("shots", &prev)));
        let new = build_pass_chain(&child_store, &outer, &local, Some(("shots", &prev, 2)));

        assert_same_chain(
            &child_store,
            &old,
            &new,
            "loop inside a use child's own store",
        );
    }

    /// Issue #501's second review, finding on `Store::snapshot`: a
    /// shadowed key (`shots`, shadowed by `with_prev_overlay`'s own
    /// `prime` overlay once `prev` exists) must keep its own natural
    /// position -- here, before `other`, exactly as `loop_shaped_store`
    /// built it -- not move to the end. `Value: PartialEq`'s own
    /// `py_eq` ignores dict key order (same as Python's `==`), so this
    /// checks the actual [`IndexMap`] key order directly rather than
    /// `assert_eq!`ing two [`Value`]s.
    #[test]
    fn snapshot_keeps_a_shadowed_keys_own_position_not_the_end() {
        let store = loop_shaped_store();
        let outer = outer_chain(&store);
        let local = local_overlay(2);
        let prev = prev_writes("one-stringified");
        let pass_chain = build_pass_chain(&store, &outer, &local, Some(("shots", &prev, 2)));

        let snapshot = store.snapshot(&chain_node(&pass_chain));
        let prime_dict = match walk(&snapshot, "prime") {
            Some(Value::Dict(d)) => d,
            other => panic!("expected prime to be a dict, got {other:?}"),
        };
        let order: Vec<&str> = prime_dict
            .keys()
            .map(|key| match key {
                Value::Str(s) => s.as_str(),
                _ => panic!("expected string keys"),
            })
            .collect();
        let shots_pos = order.iter().position(|k| *k == "shots").unwrap();
        let other_pos = order.iter().position(|k| *k == "other").unwrap();
        assert!(
            shots_pos < other_pos,
            "shots (built before other in loop_shaped_store) must still \
             sort before other once shots has grown a prev overlay, not \
             move to the end: order was {order:?}"
        );
    }
}
