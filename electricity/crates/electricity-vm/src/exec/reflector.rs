//! `reflector` effect execution (`core/reflector.py`) -- issue #506's
//! lane L1. A reflector runs an inner planning `dynamic` whose plan
//! step gets a "prime directive" prepended (`render_reflector_prime`),
//! reads the plan step's text as YAML, normalizes it
//! (`extract_done_flag`: the `done:` flag, the `steps:`/`plan:`
//! spellings, a bare list), re-serializes it with [`electricity_yaml::dump`],
//! and runs the result as a synthesized `use: inline` child under
//! `<reflector>.<generated_key>.iter_<i>` (lane I's own
//! [`super::use_::execute_use_with_ctx`]), for up to `max_iterations`
//! cycles.
//!
//! The reflector makes no model call of its own -- the planner is an
//! ordinary `prompt` (lane G's [`super::prompt::execute_prompt`], run
//! through [`super::dynamic::execute_dynamic`] like any other nested
//! `dynamic`), and the generated document is an ordinary inline `use`
//! child (lane I). This module is the glue between them, plus the
//! pieces only the reflector needs: the prime renderer
//! ([`electricity_value::pycompat::py_str_format`]), the plan
//! normalizer, and [`crate::Frame::rebased`] (the inner dynamic's own
//! paths -- compiled at `<refl>.<step>`, never `<refl>.inner.<step>`,
//! `electricity-compiler::compile::leaves::compile_reflector`'s own
//! scope choice -- have to *report* `<refl>.inner.<step>` at run time).
//!
//! **Node key order, `setdefault` semantics (copied as-is, a documented
//! M1 bug-compatibility item).** `node = store.ensure_dict(name)`, then
//! `value`/`meta`/`generated_key` are every one `setdefault`-shaped: a
//! reused node (an unnamed loop's later pass, a reflector inside an
//! unnamed `if` inside a loop) keeps its own earlier content, so
//! `meta.iterations` keeps growing across passes and a stale
//! `meta.error` from an earlier failed pass survives a later successful
//! one. Success leaves `meta` as `{"iterations": [...]}` only -- no
//! `created_at`/`completed_at` ever, on success (`--resume` is out of
//! scope for M1; a successful reflector always reruns from scratch).
//!
//! **Fatal errors, never absorbed, never wrapped** (`VmError::is_fatal`).
//! [`VmError::Cancelled`] propagates with *no* record at all -- not
//! even the iteration that was in flight -- from wherever it's first
//! observed (the inner dynamic or the generated child), balanced only
//! by this effect's own `effect_complete`. A [`VmError::Refused`]
//! either the inner planner's own dispatch (an unbuildable `adapter:`)
//! or a generated child's own fresh compile turns up (ruling
//! R1 on PR #462) *is* recorded on this effect's own node
//! either way -- the iteration record, `meta.error`, `completed_at`,
//! `value: false` and `effect_complete`'s own error text, exactly like
//! an ordinary failure -- but the bare, unwrapped `Refused` value is
//! what this effect actually returns, never folded into the usual
//! `RuntimeError(rec.error)` wrap, so no enclosing `on_error: continue`
//! can ever absorb it (PR #510's first review: the
//! inner-planner case used to skip every one of those writes, taking
//! the same no-record branch as `VmError::Cancelled`).

use super::live_scope::LiveRoot;
use super::use_::{clean_yaml_fences, execute_use_with_ctx};
use super::{CtxChain, CtxSource, key, store_err};
use crate::exec::dynamic::{execute_dynamic, now_iso};
use crate::{CancellationToken, Frame, NodeRef, RunContext, RunObserver, Slot, Store, VmError};
use electricity_bytecode::{
    Escape, LeafKind, NodeKind, OnError, Op, PromptContent, ReflectorOp, Region, RetryPolicy,
    TemplateText, UseOp, UseSource,
};
use electricity_value::pycompat::{py_str_format, py_strip};
use electricity_value::{Dict, Value};

/// `_render_reflector_prime`'s own four fixed lines, verbatim.
const CRITICAL_WARNING: &str = "CRITICAL: The next system will parse your response as YAML. If you include ANY markdown (like **bold**, *italics*, backticks, or headings), parsing fails.\nDo not output any '*' characters except as plain text INSIDE a YAML quoted string.\nNever use *, **, backticks, headings, or bullet prose outside YAML.\nEvery line must be valid YAML.\n";

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_reflector(
    op: &Op,
    reflector: &ReflectorOp,
    store: &Store,
    parent: &NodeRef,
    _ctx_chain: &CtxChain,
    frame: &Frame,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
) -> Result<(), VmError> {
    let name = op
        .name
        .as_deref()
        .expect("a reflector effect's own Op always carries a name");
    let node = store.ensure_dict(parent, key(name)).map_err(store_err)?;
    if !node.borrow().contains_key(&key("value")) {
        store.set_leaf(&node, key("value"), Value::None);
    }
    let meta_node = store.ensure_dict(&node, key("meta")).map_err(store_err)?;
    if !meta_node.borrow().contains_key(&key("iterations")) {
        store.set_leaf(&meta_node, key("iterations"), Value::List(Vec::new()));
    }
    store
        .ensure_dict(&node, key(&reflector.generated_key))
        .map_err(store_err)?;

    let effect_path = frame.effect_path(&op.path);
    let instance = observer.effect_start(&effect_path);

    let iterations = std::cmp::max(1, reflector.max_iterations);

    for i in 0..iterations {
        // No cancellation check of this loop's own -- the issue's own
        // Rust design ("Paths, --resume, loops and finally:") adds
        // none beyond what the containers this calls already respect
        // (PR #510's first review: a check here,
        // between iterations, would suppress the inner dynamic's own
        // start/end pair for a signal that lands in that exact window,
        // where Python's own `_run_iteration` has no such check either
        // and lets the inner `DynamicRuntime.execute`'s own chain check
        // observe it instead, balanced).
        let rec = match run_iteration(
            i, op, reflector, store, &node, parent, frame, run_ctx, observer, token,
        )
        .await
        {
            Ok(rec) => rec,
            Err(e) => {
                // Only ever `VmError::Cancelled` here -- see this
                // module's own doc comment on "Fatal errors": no
                // value/meta write at all, the start is still balanced.
                observer.effect_complete(
                    &effect_path,
                    instance,
                    None,
                    super::completed_at_is_truthy(store, &meta_node),
                );
                observer.write();
                return Err(e);
            }
        };

        append_iteration_record(store, &meta_node, &rec);
        observer.write();

        if let Some(error_text) = rec.error.clone() {
            store.set_leaf(&node, key("value"), Value::Bool(false));
            store.set_leaf(&meta_node, key("error"), Value::Str(error_text.clone()));
            store.set_leaf(&meta_node, key("completed_at"), Value::Str(now_iso()));
            observer.effect_complete(
                &effect_path,
                instance,
                Some(&error_text),
                super::completed_at_is_truthy(store, &meta_node),
            );
            observer.write();
            return match rec.fatal {
                Some(fatal) => Err(fatal),
                None => Err(VmError::tool(error_text)),
            };
        }

        if rec.stop {
            store.set_leaf(&node, key("value"), Value::Bool(true));
            observer.effect_complete(
                &effect_path,
                instance,
                None,
                super::completed_at_is_truthy(store, &meta_node),
            );
            observer.write();
            return Ok(());
        }
    }

    store.set_leaf(&node, key("value"), Value::Bool(true));
    observer.effect_complete(
        &effect_path,
        instance,
        None,
        super::completed_at_is_truthy(store, &meta_node),
    );
    observer.write();
    Ok(())
}

/// One reflector iteration: prime -> plan -> validate -> execute via
/// `use(inline)` -- `ReflectorRuntime._run_iteration`. `Err` only ever
/// carries [`VmError::Cancelled`] (this module's own doc comment,
/// "Fatal errors"); every other failure -- including a generated
/// child's own [`VmError::Refused`] -- comes back as `Ok(rec)` with
/// [`IterationRecord::error`] (and, for `Refused`, [`IterationRecord::fatal`])
/// set, so the caller can still append the record before deciding how
/// to propagate.
#[allow(clippy::too_many_arguments)]
async fn run_iteration(
    i: u32,
    op: &Op,
    reflector: &ReflectorOp,
    store: &Store,
    node: &NodeRef,
    parent: &NodeRef,
    frame: &Frame,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
) -> Result<IterationRecord, VmError> {
    let mut rec = IterationRecord {
        i,
        plan_from_step: reflector.plan_from_step.clone(),
        done: false,
        stop: false,
        parsed: None,
        error: None,
        fatal: None,
        plan_text: String::new(),
    };

    // ── Phase 1: prime ────────────────────────────────────────────
    let goal = best_effort_goal(store);
    let context = best_effort_context(store);
    let fields: [(&str, Value); 3] = [
        ("goal", Value::Str(goal)),
        ("context", Value::Str(context)),
        ("max_effects", Value::from(i64::from(reflector.max_effects))),
    ];
    let prime_text = render_reflector_prime(&reflector.prime_template.source, &fields);

    // ── Phase 2: inner dynamic, with the prime prepended ──────────
    let prepared_inner =
        prepend_prime_to_plan_prompt(&reflector.inner, &reflector.plan_from_step, &prime_text);
    let inner_op = Op {
        path: op.path.clone(),
        name: Some("inner".to_string()),
        kind: NodeKind::Control(prepared_inner),
        on_error: OnError::Fail,
        labels: None,
        enabled: true,
    };
    // Issue #516 §4(c): a reflector's own inner `dynamic` always resets
    // to display depth 0, regardless of the reflector's own ambient
    // depth -- the one case `core/dynamic.py::_execute_effect` never
    // threads `self.depth` into (`ReflectorRuntime` is built with no
    // `depth=` keyword at all, so Python's own default applies).
    let inner_frame = frame
        .rebased(&op.path, &op.path.push_name("inner"))
        .with_display_depth(crate::complexity::depth::REFLECTOR_RESET);
    let empty_chain: CtxChain = Vec::new();
    let inner_result = execute_dynamic(
        &inner_op,
        store,
        node,
        &empty_chain,
        &inner_frame,
        run_ctx,
        observer,
        token,
    )
    .await;
    match inner_result {
        Ok(()) => {}
        Err(VmError::Cancelled) => return Err(VmError::Cancelled),
        // A `VmError::Refused` the inner planner's own dispatch finds
        // (an unbuildable `adapter:`, `exec::prompt::execute_prompt`'s
        // own up-front `RefusalReason::UnsupportedAdapter` check) is
        // fatal for *control* only (`VmError::is_fatal`'s own doc
        // comment, ruling R1 on PR #462) -- recorded here
        // exactly like an ordinary `inner_failed:` (the iteration
        // record, `meta.error`, `completed_at`, `value: false` and
        // `effect_complete`'s own error text, this module's own doc
        // comment on "Fatal errors"), with `IterationRecord::fatal` set
        // so [`execute_reflector`] still re-raises the bare, unwrapped
        // `Refused` afterward rather than folding it into the usual
        // `RuntimeError(rec.error)` wrap -- PR #510's first review:
        // this branch used to fall into the same
        // arm as `Cancelled` below, skipping every one of those writes
        // and `effect_complete` itself.
        Err(e @ VmError::Refused { .. }) => {
            rec.error = Some(format!("inner_failed: {e}"));
            rec.fatal = Some(e);
            return Ok(rec);
        }
        Err(e) => {
            rec.error = Some(format!("inner_failed: {e}"));
            return Ok(rec);
        }
    }

    // ── Phase 3: plan text ─────────────────────────────────────────
    let plan_text = get_plan_text(store, node, &reflector.plan_from_step);
    rec.plan_text = plan_text.clone();

    if run_ctx.dry_run {
        rec.done = true;
        rec.stop = true;
        return Ok(rec);
    }

    // ── Phase 4: done flag + normalize ────────────────────────────
    let (done, plan_yaml) = match extract_done_flag(&plan_text) {
        Ok(v) => v,
        Err(e) => {
            rec.error = Some(format!("parse_failed: {e}"));
            return Ok(rec);
        }
    };
    rec.done = done;

    if py_strip(&plan_yaml).is_empty() {
        rec.stop = true;
        return Ok(rec);
    }

    let stopping = done && reflector.stop_on_done;

    if let Ok(parsed_check) = electricity_yaml::load_yaml_last_key_wins(&plan_yaml) {
        let effects_value = match &parsed_check {
            Value::Dict(d) => d
                .get(&Value::Str("effects".to_string()))
                .cloned()
                .unwrap_or(Value::List(Vec::new())),
            _ => Value::List(Vec::new()),
        };
        rec.parsed = Some(parsed_check);
        if !py_truthy(&effects_value) {
            rec.stop = true;
            return Ok(rec);
        }
        if !stopping {
            if let Value::List(items) = &effects_value {
                if items.len() > reflector.max_effects as usize {
                    rec.error = Some(format!(
                        "invalid_plan: generated plan has {} top-level effects, exceeds max_effects ({})",
                        items.len(),
                        reflector.max_effects
                    ));
                    return Ok(rec);
                }
            }
        }
    }
    if stopping {
        rec.stop = true;
        return Ok(rec);
    }

    // ── Phase 5: validate + compile + execute via use(inline) ─────
    let gen_node = store
        .ensure_dict(node, key(&reflector.generated_key))
        .map_err(store_err)?;
    let iter_name = format!("iter_{i}");
    let use_op = UseOp {
        parent_trusted: false,
        source: UseSource::Inline(TemplateText::new(plan_yaml, true, Escape::Html)),
        inputs: None,
        outputs: None,
        validate: true,
        retries: RetryPolicy::default(),
        expect: None,
        description: None,
    };
    let use_path = op
        .path
        .push_name(reflector.generated_key.clone())
        .push_name(iter_name.clone());
    let use_synth_op = Op {
        path: use_path,
        name: Some(iter_name),
        kind: NodeKind::Leaf(Box::new(LeafKind::Use(use_op.clone()))),
        on_error: OnError::Fail,
        labels: None,
        enabled: true,
    };
    // `_store_root(store)` at the top level: the reflector's own
    // *containing* scope (`parent`), never the inherited `CtxChain` --
    // this module's own doc comment, and the Rust design's own "pass a
    // `ParamsCtx` over the containing parent `NodeRef` alone" ruling. A
    // live chain (not a one-shot snapshot) so a later iteration sees
    // whatever the containing scope has written since, matching
    // Python's own live dict reference.
    let gen_ctx_chain: CtxChain = vec![CtxSource::Live(parent.clone())];
    let live_root = LiveRoot::new(&gen_ctx_chain, store);
    // Same reset as the inner `dynamic` above -- the generated `use`
    // (`UseRuntime` built with no `depth=` either) runs at display
    // depth 0, so its own child root `dynamic` ([`Frame::nested`]'s own
    // `+1`) lands at depth 1, matching issue #516 §4(c)'s last row.
    let gen_frame = frame.with_display_depth(crate::complexity::depth::REFLECTOR_RESET);
    let use_result = execute_use_with_ctx(
        &use_synth_op,
        &use_op,
        store,
        &gen_node,
        &live_root,
        &gen_frame,
        run_ctx,
        observer,
        token,
    )
    .await;
    match use_result {
        Ok(()) => {}
        Err(VmError::Cancelled) => return Err(VmError::Cancelled),
        Err(e @ VmError::Refused { .. }) => {
            rec.error = Some(format!("generated_exec_failed: {e}"));
            rec.fatal = Some(e);
        }
        Err(e) => {
            rec.error = Some(format!("generated_exec_failed: {e}"));
        }
    }
    Ok(rec)
}

/// `rec = {"i":..., "plan_from_step":..., "done":..., "stop":...,
/// "parsed":..., "error":..., "plan_text":...}` -- key order pinned
/// by [`IterationRecord::to_value`]. `raw_plan_text` (Python's own
/// verbose-only key) is never added -- a documented divergence, M1
/// settled question ("Settled questions" section): electricity has no
/// verbose mode.
struct IterationRecord {
    i: u32,
    plan_from_step: String,
    done: bool,
    stop: bool,
    parsed: Option<Value>,
    error: Option<String>,
    /// `Some` only for a generated child's own [`VmError::Refused`] --
    /// the exact value [`execute_reflector`] re-raises unwrapped
    /// instead of its usual `RuntimeError(rec.error)` wrap. `None`
    /// means an ordinary failure, or no failure at all.
    fatal: Option<VmError>,
    plan_text: String,
}

impl IterationRecord {
    fn to_value(&self) -> Value {
        let mut d = Dict::new();
        d.insert(Value::Str("i".to_string()), Value::from(i64::from(self.i)));
        d.insert(
            Value::Str("plan_from_step".to_string()),
            Value::Str(self.plan_from_step.clone()),
        );
        d.insert(Value::Str("done".to_string()), Value::Bool(self.done));
        d.insert(Value::Str("stop".to_string()), Value::Bool(self.stop));
        d.insert(
            Value::Str("parsed".to_string()),
            self.parsed.clone().unwrap_or(Value::None),
        );
        d.insert(
            Value::Str("error".to_string()),
            self.error.clone().map(Value::Str).unwrap_or(Value::None),
        );
        d.insert(
            Value::Str("plan_text".to_string()),
            Value::Str(self.plan_text.clone()),
        );
        Value::Dict(d)
    }
}

fn append_iteration_record(store: &Store, meta_node: &NodeRef, rec: &IterationRecord) {
    let mut iterations: Vec<Value> = match store.peek(meta_node, &key("iterations")) {
        Some(Slot::Value(v)) => v.as_list().map(<[Value]>::to_vec).unwrap_or_default(),
        _ => Vec::new(),
    };
    iterations.push(rec.to_value());
    store.set_leaf(meta_node, key("iterations"), Value::List(iterations));
}

/// `_render_reflector_prime`: the four fixed warning lines, plus
/// `prime_template.format(max_effects=..., goal=..., context=...).strip()`,
/// plus `"\n\n"`. On a format failure, falls back to the raw template
/// (still stripped) and logs the same fixed warning line G's own
/// channel uses (`log::warn!`, M1 settled question: first line only,
/// no traceback reproduced).
fn render_reflector_prime(template: &str, fields: &[(&str, Value)]) -> String {
    match py_str_format(template, fields) {
        Ok(rendered) => format!("{CRITICAL_WARNING}{}\n\n", py_strip(&rendered)),
        Err(_) => {
            log::warn!("Reflector prime template formatting failed; using raw template");
            format!("{CRITICAL_WARNING}{}\n\n", py_strip(template))
        }
    }
}

/// `_best_effort_goal`: `root["prime"]["goal"]["value"]` at *store*'s
/// own root (`store.root` -- the run root, or a `use` child's own
/// store; both match Python's `true_root_state`, this module's own
/// doc comment), if it is a non-blank `str`; `""` otherwise.
fn best_effort_goal(store: &Store) -> String {
    let prime = match store.peek(&store.root, &key("prime")) {
        Some(Slot::Node(n)) => n,
        _ => return String::new(),
    };
    let goal = match store.peek(&prime, &key("goal")) {
        Some(Slot::Node(n)) => n,
        _ => return String::new(),
    };
    match store.peek(&goal, &key("value")) {
        Some(Slot::Value(v)) => match v.as_str() {
            Some(s) => {
                let stripped = py_strip(s);
                if stripped.is_empty() {
                    String::new()
                } else {
                    stripped.to_string()
                }
            }
            None => String::new(),
        },
        _ => String::new(),
    }
}

/// `_best_effort_context`: `{"model", "adapter", "plugins"}` off
/// *store*'s own root's `runtime.effective_settings` (already written
/// into state by the time any effect runs -- `electricity` (lib)'s own
/// run wiring, step 13, before step 14 compiles/dispatches anything),
/// redacted again, `json.dumps(..., indent=2, sort_keys=True)`, capped
/// at 2000 **code points**. `""` on any missing piece or write failure.
fn best_effort_context(store: &Store) -> String {
    let runtime = match store.peek(&store.root, &key("runtime")) {
        Some(Slot::Node(n)) => n,
        _ => return String::new(),
    };
    let effective = match store.peek(&runtime, &key("effective_settings")) {
        Some(Slot::Node(n)) => store.snapshot(&n),
        Some(Slot::Value(v @ Value::Dict(_))) => v,
        _ => return String::new(),
    };
    let Some(eff_dict) = effective.as_dict() else {
        return String::new();
    };
    let mut summary = Dict::new();
    for field in ["model", "adapter", "plugins"] {
        let value = eff_dict
            .get(&Value::Str(field.to_string()))
            .cloned()
            .unwrap_or(Value::None);
        summary.insert(Value::Str(field.to_string()), value);
    }
    let redacted = electricity_redaction::redact(Value::Dict(summary));
    let text = match electricity_json::dumps(&redacted, electricity_json::WriteMode::PRETTY) {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    if text.chars().count() > 2000 {
        let truncated: String = text.chars().take(2000).collect();
        format!(
            "{}\n... (truncated)",
            truncated.trim_end_matches(electricity_value::pycompat::is_py_whitespace)
        )
    } else {
        text
    }
}

/// `_prepend_prime_to_plan_prompt`: replaces only a **top-level**
/// `prompt` named *plan_from_step* inside *region*'s own ops, setting
/// its `content` to `template = prime_text + (old template source or
/// "")` -- dropping `messages:` entirely, the same as Python's own
/// `replace(s, template=...)` (a dataclass can't carry both). Every
/// other op (a different name, a different leaf kind, a nested
/// container) passes through unchanged.
fn prepend_prime_to_plan_prompt(region: &Region, plan_from_step: &str, prime_text: &str) -> Region {
    let rewrite = |ops: &[Op]| -> Vec<Op> {
        ops.iter()
            .map(|op| {
                if op.name.as_deref() == Some(plan_from_step) {
                    if let NodeKind::Leaf(leaf) = &op.kind {
                        if let LeafKind::Prompt(prompt_op) = leaf.as_ref() {
                            let old_source = match &prompt_op.content {
                                PromptContent::Template(t) => t.source.clone(),
                                PromptContent::TemplateFile(_) | PromptContent::Messages(_) => {
                                    String::new()
                                }
                            };
                            let mut new_prompt = prompt_op.clone();
                            new_prompt.content = PromptContent::Template(TemplateText::new(
                                format!("{prime_text}{old_source}"),
                                true,
                                Escape::None,
                            ));
                            return Op {
                                path: op.path.clone(),
                                name: op.name.clone(),
                                kind: NodeKind::Leaf(Box::new(LeafKind::Prompt(new_prompt))),
                                on_error: op.on_error,
                                labels: op.labels.clone(),
                                enabled: op.enabled,
                            };
                        }
                    }
                }
                op.clone()
            })
            .collect()
    };
    match region {
        Region::Block { ops, overlay } => Region::Block {
            ops: rewrite(ops),
            overlay: *overlay,
        },
        Region::Parallel {
            branches,
            max_concurrency,
            stop_on_error,
        } => Region::Parallel {
            branches: rewrite(branches),
            max_concurrency: *max_concurrency,
            stop_on_error: *stop_on_error,
        },
        // `compile_reflector` only ever builds a reflector's own
        // `inner` as `Block`/`Parallel` (one `flow:` choice) -- never
        // reached.
        other => other.clone(),
    }
}

/// `_get_plan_text`: `node["inner"][plan_from_step]["value"]` if it is
/// a `str`; `""` otherwise (including a `json`/`object` plan prompt, a
/// missing/non-prompt plan step).
fn get_plan_text(store: &Store, node: &NodeRef, plan_from_step: &str) -> String {
    let inner = match store.peek(node, &key("inner")) {
        Some(Slot::Node(n)) => n,
        _ => return String::new(),
    };
    let step = match store.peek(&inner, &Value::Str(plan_from_step.to_string())) {
        Some(Slot::Node(n)) => n,
        _ => return String::new(),
    };
    match store.peek(&step, &key("value")) {
        Some(Slot::Value(v)) => v.as_str().map(str::to_string).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Python truthiness over a [`Value`] -- `_extract_done_flag`'s own
/// `bool(parsed.get("done", False))` and phase 4's own `if not
/// effects`. A private copy of `exec::use_`'s own `is_truthy` (that
/// one takes `Option<&Value>` for a different call shape; not worth
/// sharing a single-line body across modules for this).
fn py_truthy(value: &Value) -> bool {
    match value {
        Value::None => false,
        Value::Bool(b) => *b,
        Value::Int(i) => !i.is_zero(),
        Value::Float(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::Bytes(b) => !b.is_empty(),
        Value::List(items) => !items.is_empty(),
        Value::Dict(d) => !d.is_empty(),
        Value::Date(_) | Value::DateTime(..) => true,
    }
}

/// `_extract_done_flag`: [`clean_yaml_fences`] (lane I's own port, now
/// fixed to Python `splitlines`/`strip` semantics), blank -> `(true,
/// "")`, a YAML parse error -> `(false, cleaned)` un-dumped, a bare
/// list -> `(false, dump({"effects": parsed}))`, any other non-dict ->
/// `(false, cleaned)`; a dict gets `done` extracted (Python
/// truthiness), `effects` filled from `steps`/`plan.effects`/`plan` in
/// that order, `done` popped, and is re-dumped
/// (`electricity_yaml::dump`, `sort_keys: true` -- `yaml.dump`'s own
/// default, matching `_extract_done_flag`'s own un-keyworded call).
fn extract_done_flag(plan_text: &str) -> Result<(bool, String), String> {
    let cleaned = clean_yaml_fences(plan_text);
    if py_strip(&cleaned).is_empty() {
        return Ok((true, String::new()));
    }
    let parsed = match electricity_yaml::load_yaml_last_key_wins(&cleaned) {
        // `_extract_done_flag`'s own `except _yaml.YAMLError:` -- a
        // calendar-invalid timestamp's bare `ValueError` (issue #528's
        // own comment) is never one, so it escapes uncaught, past
        // `_extract_done_flag` entirely, to its one caller's own outer
        // `except Exception as e: rec["error"] = f"parse_failed: {e}"`
        // -- never this function's own "let use(inline) handle it"
        // swallow, which only an *ordinary* `yaml.YAMLError` gets.
        Err(electricity_yaml::YamlError::InvalidTimestampValue { message, .. }) => {
            return Err(message);
        }
        Err(_) => return Ok((false, cleaned)),
        Ok(v) => v,
    };
    let dump_opts = electricity_yaml::DumpOptions { sort_keys: true };
    let mut dict: Dict = if let Some(d) = parsed.as_dict() {
        d.clone()
    } else if let Some(items) = parsed.as_list() {
        let mut wrapper = Dict::new();
        wrapper.insert(
            Value::Str("effects".to_string()),
            Value::List(items.to_vec()),
        );
        let dumped =
            electricity_yaml::dump(&Value::Dict(wrapper), &dump_opts).map_err(|e| e.to_string())?;
        return Ok((false, dumped));
    } else {
        return Ok((false, cleaned));
    };

    let done_key = Value::Str("done".to_string());
    let effects_key = Value::Str("effects".to_string());
    let done = py_truthy(&dict.get(&done_key).cloned().unwrap_or(Value::Bool(false)));

    if !dict.contains_key(&effects_key) {
        let steps_key = Value::Str("steps".to_string());
        let plan_key = Value::Str("plan".to_string());
        let steps_list = match dict.get(&steps_key) {
            Some(Value::List(items)) => Some(items.clone()),
            _ => None,
        };
        if let Some(steps) = steps_list {
            dict.shift_remove(&steps_key);
            dict.insert(effects_key.clone(), Value::List(steps));
        } else {
            let plan_dict_effects = match dict.get(&plan_key) {
                Some(Value::Dict(plan_dict)) => plan_dict.get(&effects_key).cloned(),
                _ => None,
            };
            if let Some(effects) = plan_dict_effects {
                dict.shift_remove(&plan_key);
                dict.insert(effects_key.clone(), effects);
            } else {
                let plan_list = match dict.get(&plan_key) {
                    Some(Value::List(items)) => Some(items.clone()),
                    _ => None,
                };
                if let Some(items) = plan_list {
                    dict.shift_remove(&plan_key);
                    dict.insert(effects_key.clone(), Value::List(items));
                } else {
                    return Ok((done, cleaned));
                }
            }
        }
    }

    dict.shift_remove(&done_key);
    let dumped =
        electricity_yaml::dump(&Value::Dict(dict), &dump_opts).map_err(|e| e.to_string())?;
    Ok((done, dumped))
}

/// Replays `tests/golden/reflector_corpus.json`
/// (`scripts/generate_reflector_corpus.py`) -- real Circuitry output
/// for `_clean_yaml_fences`/`_extract_done_flag`/`_render_reflector_prime`
/// and the redact/`json.dumps`/truncate transform `_best_effort_context`
/// wraps, compared against this module's own ports.
#[cfg(test)]
mod corpus_tests {
    use super::*;

    fn json_to_value(j: &serde_json::Value) -> Value {
        match j {
            serde_json::Value::Null => Value::None,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::from(i)
                } else {
                    Value::Float(n.as_f64().expect("json number is representable as f64"))
                }
            }
            serde_json::Value::String(s) => Value::Str(s.clone()),
            serde_json::Value::Array(items) => {
                Value::List(items.iter().map(json_to_value).collect())
            }
            serde_json::Value::Object(map) => {
                let mut d = Dict::new();
                for (k, v) in map {
                    d.insert(Value::Str(k.clone()), json_to_value(v));
                }
                Value::Dict(d)
            }
        }
    }

    /// Writes *obj* (a JSON object) into *node* as a real tracked
    /// `Store` tree -- every nested object becomes its own
    /// [`Store::ensure_dict`] node (so a later [`Store::peek`] sees
    /// `Slot::Node`, matching what [`best_effort_goal`]/
    /// [`best_effort_context`] expect for a container), every other
    /// value a plain leaf. A `set_leaf` of the whole object at once
    /// (`Value::Dict`) would instead read back as `Slot::Value`, which
    /// those two functions treat as absent.
    fn populate_tree(store: &Store, node: &NodeRef, obj: &serde_json::Value) {
        let serde_json::Value::Object(map) = obj else {
            return;
        };
        for (k, v) in map {
            if v.is_object() {
                let child = store.ensure_dict(node, Value::Str(k.clone())).unwrap();
                populate_tree(store, &child, v);
            } else {
                store.set_leaf(node, Value::Str(k.clone()), json_to_value(v));
            }
        }
    }

    #[test]
    fn matches_circuitrys_own_reflector_helpers() {
        let data = include_str!("../../tests/golden/reflector_corpus.json");
        let cases: Vec<serde_json::Value> = serde_json::from_str(data).unwrap();
        assert!(
            cases.len() > 20,
            "corpus should hold every documented category"
        );

        let mut failures = Vec::new();
        for (i, case) in cases.iter().enumerate() {
            match case["kind"].as_str().unwrap() {
                "clean_fences" => {
                    let input = case["input"].as_str().unwrap();
                    let expected = case["cleaned"].as_str().unwrap();
                    let got = clean_yaml_fences(input);
                    if got != expected {
                        failures.push(format!(
                            "case {i} (clean_fences): expected {expected:?}, got {got:?}"
                        ));
                    }
                }
                "extract_done" => {
                    let input = case["input"].as_str().unwrap();
                    let ok = case["ok"].as_bool().unwrap();
                    match extract_done_flag(input) {
                        Ok((done, plan_yaml)) if ok => {
                            let expected_done = case["done"].as_bool().unwrap();
                            let expected_plan = case["plan_yaml"].as_str().unwrap();
                            if done != expected_done || plan_yaml != expected_plan {
                                failures.push(format!(
                                    "case {i} (extract_done): expected ({expected_done}, {expected_plan:?}), got ({done}, {plan_yaml:?})"
                                ));
                            }
                        }
                        Ok((done, plan_yaml)) => failures.push(format!(
                            "case {i} (extract_done): expected an error, got Ok(({done}, {plan_yaml:?}))"
                        )),
                        Err(e) if ok => failures.push(format!(
                            "case {i} (extract_done): expected success, got Err({e})"
                        )),
                        Err(_) => {}
                    }
                }
                "prime" => {
                    let template = case["template"].as_str().unwrap();
                    let goal = case["goal"].as_str().unwrap().to_string();
                    let context = case["context"].as_str().unwrap().to_string();
                    let max_effects = case["max_effects"].as_i64().unwrap();
                    let fields: [(&str, Value); 3] = [
                        ("goal", Value::Str(goal)),
                        ("context", Value::Str(context)),
                        ("max_effects", Value::from(max_effects)),
                    ];
                    let got = render_reflector_prime(template, &fields);
                    // A documented divergence (DESIGN.md §13): `py_str_format`
                    // never does attribute/index access, so it falls back
                    // where Python's own `.format()` would have succeeded.
                    // Only check that *our own* output took that fallback
                    // branch (the raw template text survives unexpanded),
                    // never Python's own (here, deterministic) text.
                    if case["divergence"].as_bool().unwrap_or(false) {
                        let expected_fallback =
                            format!("{CRITICAL_WARNING}{}\n\n", py_strip(template));
                        if got != expected_fallback {
                            failures.push(format!(
                                "case {i} (prime, divergence): expected the raw-template fallback, got {got:?}"
                            ));
                        }
                        continue;
                    }
                    let expected = case["rendered"].as_str().unwrap();
                    if got != expected {
                        failures.push(format!(
                            "case {i} (prime): expected {expected:?}, got {got:?}"
                        ));
                    }
                }
                "context" => {
                    let store = Store::new();
                    let runtime_node = store.ensure_dict(&store.root, key("runtime")).unwrap();
                    let effective = json_to_value(&case["effective_settings"]);
                    store.set_leaf(&runtime_node, key("effective_settings"), effective);
                    let expected = case["context"].as_str().unwrap();
                    let got = best_effort_context(&store);
                    if got != expected {
                        failures.push(format!(
                            "case {i} (context): expected {expected:?}, got {got:?}"
                        ));
                    }
                }
                "goal" => {
                    let store = Store::new();
                    let prime_node = store.ensure_dict(&store.root, key("prime")).unwrap();
                    populate_tree(&store, &prime_node, &case["prime"]);
                    let expected = case["goal"].as_str().unwrap();
                    let got = best_effort_goal(&store);
                    if got != expected {
                        failures.push(format!(
                            "case {i} (goal): expected {expected:?}, got {got:?}"
                        ));
                    }
                }
                other => panic!("unknown corpus case kind {other:?}"),
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {} cases failed:\n{}",
            failures.len(),
            cases.len(),
            failures.join("\n")
        );
    }
}
