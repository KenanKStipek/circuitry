//! `yield` effect execution (M1-P, the run-time half of #406, issue
//! #487) -- a close port of `core/yield_effect.py::YieldRuntime.execute`:
//! renders *yield_op*'s own `template` (with `{{> name}}` partial
//! expansion, [`crate::compose::render_with_composition`], unescaped --
//! it is prompt text) against *ctx_chain* merged with its own raw
//! `inputs:`, and writes the result as this effect's own `value`. No
//! adapter is built, no tokens are counted, and there is nothing to
//! retry: a render failure goes through `on_error` exactly like any
//! other effect's failure.

use crate::exec::CtxChain;
use crate::exec::live_scope::LiveRoot;
use crate::{CancellationToken, Frame, NodeRef, RunContext, RunObserver, Store, VmError};
use electricity_bytecode::{Escape, OnError, Op, ParamNode, YieldOp};
use electricity_template::{PlainCtx, ScopeRoot};
use electricity_value::{Dict, Value};

fn now_iso() -> Value {
    use chrono::SecondsFormat;
    Value::Str(chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false))
}

fn set_leaf(store: &Store, node: &NodeRef, key: &str, value: Value) {
    store.set_leaf(node, Value::Str(key.to_string()), value);
}

fn store_err(err: crate::StoreError) -> VmError {
    VmError::Message(err.to_string())
}

/// A private overlay root for a yield effect's own raw `inputs:`
/// (never rendered -- already `ParamNode::Literal`), wrapping the
/// caller's own [`ScopeRoot`] *inner* -- `effective_ctx = dict(ctx);
/// effective_ctx.update(self.defn.inputs)`'s shallow merge: an
/// `inputs` key shadows a same-named top-level key in *inner*, and
/// every other lookup delegates straight through. The same shape as
/// [`crate::compose`]'s own private `HiddenOverlay`, kept as a separate
/// copy (a `Value`-keyed map here, a `String`-keyed one there) rather
/// than shared across that module boundary for a seam this small.
struct InputsOverlay<'a, R> {
    inputs: &'a Dict,
    inner: &'a R,
}

impl<R: ScopeRoot> ScopeRoot for InputsOverlay<'_, R> {
    fn lookup(&self, key: &str) -> Result<Option<Value>, String> {
        let mut parts = key.split('.');
        let first = parts.next().expect("split yields at least one segment");
        if let Some(value) = self.inputs.get(&Value::Str(first.to_string())) {
            let mut current = value.clone();
            for part in parts {
                match electricity_template::step(&current, part)? {
                    Some(next) => current = next,
                    None => return Ok(None),
                }
            }
            return Ok(Some(current));
        }
        self.inner.lookup(key)
    }

    fn whole(&self) -> Value {
        let mut base = self.inner.whole();
        if let Value::Dict(dict) = &mut base {
            for (k, v) in self.inputs {
                dict.insert(k.clone(), v.clone());
            }
        }
        base
    }

    /// Overrides the default (`lookup`-and-check) impl: *inputs*' own
    /// top-level keys are already-materialized literal `Value`s held
    /// right here, so resolving one through `self.lookup` costs nothing
    /// *inner*-side (no store access at all) -- only the fall-through
    /// branch (the first segment isn't an `inputs` key) needs to reach
    /// *inner* at all, and it does so through *inner*'s own `contains`,
    /// never `lookup` (review finding 1 on issue #492's PR: the
    /// inherited default would otherwise call `self.lookup`, which
    /// falls through to `inner.lookup`, which -- for a store-backed
    /// *inner* whose path resolves onto a `Slot::Node` with no segments
    /// left, e.g. `{{> named_loop.x}}`'s own `prime.named_loop` presence
    /// check -- materializes that whole subtree, a named loop's every
    /// pass included, just to answer a yes/no question).
    fn contains(&self, key: &str) -> bool {
        let first = key
            .split('.')
            .next()
            .expect("split yields at least one segment");
        if self.inputs.contains_key(&Value::Str(first.to_string())) {
            return matches!(self.lookup(key), Ok(Some(_)));
        }
        self.inner.contains(key)
    }
}

/// Runs a `yield` effect: renders *yield_op*'s own `template` (with
/// `{{> name}}` partial expansion, [`crate::compose::render_with_composition`])
/// against *ctx_chain*, resolved lazily through [`LiveRoot`] and merged
/// with the effect's own raw `inputs:`, and writes the result as this
/// effect's own `value`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_yield(
    op: &Op,
    yield_op: &YieldOp,
    store: &Store,
    parent: &NodeRef,
    ctx_chain: &CtxChain,
    frame: &Frame,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
) -> Result<(), VmError> {
    let live_root = LiveRoot::new(ctx_chain, store);
    execute_yield_with_ctx(
        op, yield_op, store, parent, &live_root, frame, run_ctx, observer, token,
    )
    .await
}

/// [`execute_yield`]'s own real body, generic over *ctx*'s own
/// [`ScopeRoot`] rather than requiring an already-resolved [`CtxChain`]
/// (that type is `pub(crate)` to `exec/mod.rs`, not nameable outside
/// this crate -- `tests/compose_corpus.rs`'s own golden-corpus replay,
/// issue #487's own test plan, is this function's real caller besides
/// [`execute_yield`] itself, the same precedent `exec::use_::
/// execute_use_with_ctx`'s own doc comment already set).
#[allow(clippy::too_many_arguments)]
pub async fn execute_yield_with_ctx<R: ScopeRoot>(
    op: &Op,
    yield_op: &YieldOp,
    store: &Store,
    parent: &NodeRef,
    ctx: &R,
    frame: &Frame,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    _token: &CancellationToken,
) -> Result<(), VmError> {
    let name = op
        .name
        .as_deref()
        .expect("a yield effect's own Op always carries a name");
    let name_key = Value::Str(name.to_string());

    let effect_node = store.ensure_dict(parent, name_key).map_err(store_err)?;
    // `node.setdefault("value", None)` -- a plain presence check, so a
    // node this effect's own name already carries from an earlier pass
    // (an unnamed loop reusing it) keeps its own previous `value`
    // rather than being clobbered back to `None` here (`self.value`'s
    // own "previous pass's value on a node an unnamed loop reuses"
    // self-reference case).
    if !effect_node
        .borrow()
        .contains_key(&Value::Str("value".to_string()))
    {
        set_leaf(store, &effect_node, "value", Value::None);
    }
    let meta_node = store
        .ensure_dict(&effect_node, Value::Str("meta".to_string()))
        .map_err(store_err)?;

    // Keys written in this order, same as Python's own dict-literal
    // construction -- on a node an unnamed loop's later pass reuses,
    // each key keeps its own first position (`IndexMap::insert` on an
    // already-present key never moves it).
    set_leaf(store, &meta_node, "created_at", now_iso());
    set_leaf(store, &meta_node, "completed_at", Value::None);
    set_leaf(store, &meta_node, "error", Value::None);
    set_leaf(store, &meta_node, "dry_run", Value::Bool(run_ctx.dry_run));

    // Unlike a tool/prompt effect, `effect_start` fires *after* these
    // meta writes and *before* the render -- the node (and its fresh
    // meta) must already be visible for a self-reference (`{{> self}}`)
    // to resolve against it.
    let effect_path = frame.effect_path(&op.path);
    let instance = observer.effect_start(&effect_path);
    observer.write();

    // `if self.dry_run: node["value"] = None; meta["completed_at"] =
    // _now_iso(); ...; return` -- a dry run never renders and never
    // touches `meta.error`/`on_error`, the same shape `exec::use_`'s
    // own dry-run branch already gives `execute_use`.
    if run_ctx.dry_run {
        set_leaf(store, &effect_node, "value", Value::None);
        set_leaf(store, &meta_node, "completed_at", now_iso());
        observer.effect_complete(
            &effect_path,
            instance,
            None,
            super::completed_at_is_truthy(store, &meta_node),
        );
        observer.write();
        return Ok(());
    }

    let label = format!("yield '{name}' template");
    let escape = matches!(yield_op.template.escape, Escape::Html);
    // `effective_ctx = dict(ctx); effective_ctx.update(self.defn.inputs)`
    // -- the effect's own raw `inputs:` (already `ParamNode::Literal`,
    // never rendered) as a top-level overlay, only when non-empty: an
    // `inputs` key shadows the same-named top-level key in *ctx*, and
    // every other lookup still resolves lazily through *ctx* itself.
    let render_result = match &yield_op.inputs {
        Some(ParamNode::Literal(Value::Dict(inputs))) if !inputs.is_empty() => {
            let overlay = InputsOverlay { inputs, inner: ctx };
            crate::compose::render_with_composition(
                &yield_op.template.source,
                &overlay,
                run_ctx.declared_prompts,
                run_ctx.effect_names,
                &label,
                &PlainCtx,
                escape,
            )
        }
        _ => crate::compose::render_with_composition(
            &yield_op.template.source,
            ctx,
            run_ctx.declared_prompts,
            run_ctx.effect_names,
            &label,
            &PlainCtx,
            escape,
        ),
    };
    match render_result {
        Ok(rendered) => {
            set_leaf(store, &effect_node, "value", Value::Str(rendered));
            set_leaf(store, &meta_node, "completed_at", now_iso());
            observer.effect_complete(
                &effect_path,
                instance,
                None,
                super::completed_at_is_truthy(store, &meta_node),
            );
            observer.write();
            Ok(())
        }
        Err(message) => {
            // `meta.error = redact(str(e))` -- the *stored* error text
            // is redacted; the propagated failure (below, under
            // `on_error: fail`) carries the message unredacted, same
            // as every other effect's own failure.
            //
            // `Value::Str(s) = redacted` would move `s` out of a type
            // that implements `Drop` (electricity-value's iterative
            // `Drop` impl) -- match on `&mut` and `mem::take` the
            // `String` out instead (same fix as `params.rs`/`render.rs`'s
            // own identical comment).
            let mut redacted_value = electricity_redaction::redact(Value::Str(message.clone()));
            let redacted = match &mut redacted_value {
                Value::Str(s) => std::mem::take(s),
                other => other.py_str(),
            };
            set_leaf(store, &meta_node, "error", Value::Str(redacted.clone()));
            set_leaf(store, &meta_node, "completed_at", now_iso());
            set_leaf(store, &effect_node, "value", Value::None);
            // `--events`'s own end `error` is this *redacted* text, the
            // same one `meta.error` just got -- cof's own `--events`
            // builds its end event's `error` from `meta.error`
            // (`cli/events.py`), never the raw exception text, so a
            // secret-shaped message must never reach the event stream
            // even though `Raised` below still carries it unredacted
            // (review finding 2 on issue #492's PR).
            observer.effect_complete(
                &effect_path,
                instance,
                Some(&redacted),
                super::completed_at_is_truthy(store, &meta_node),
            );
            observer.write();
            if op.on_error == OnError::Fail {
                return Err(VmError::Raised {
                    py_class: electricity_tools::PyExc::TemplateError,
                    message,
                    cause: electricity_tools::ErrorCause::None,
                    cause_class: None,
                });
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::CtxSource;
    use crate::observer::NullObserver;
    use crate::{Slot, Store};
    use electricity_bytecode::{EffectPath, LeafKind, NodeKind, TemplateText};
    use electricity_tools::ToolRegistry;

    fn yield_op_leaf(template: &str, inputs: Option<electricity_bytecode::ParamNode>) -> YieldOp {
        YieldOp {
            template: TemplateText::new(template, false, Escape::None),
            inputs,
            description: None,
        }
    }

    fn yield_node(name: &str, on_error: OnError) -> Op {
        Op {
            path: EffectPath::root().push_name(name),
            name: Some(name.to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Yield(yield_op_leaf("", None)))),
            on_error,
            labels: None,
            enabled: true,
        }
    }

    // Same shape as `exec::tool::tests::default_run_ctx` -- a leaked,
    // `'static` `RunContext` for every M1-A field this module's own
    // tests never read, so a test only has to supply the handful of
    // arguments it actually varies.
    fn default_run_ctx<'a>(
        registry: &'a ToolRegistry,
        limiter: &'a crate::Limiter,
        runtime_config: &'a Value,
        declared_prompts: &'a indexmap::IndexMap<String, String>,
        effect_names: &'a std::collections::BTreeSet<String>,
    ) -> RunContext<'a> {
        let adapters: &'a crate::adapter::AdapterRegistry =
            Box::leak(Box::new(crate::adapter::AdapterRegistry::new()));
        let complexity: &'a electricity_config::ComplexitySettings =
            Box::leak(Box::new(electricity_config::ComplexitySettings::default()));
        let dir: &'static std::path::PathBuf = Box::leak(Box::new(std::path::PathBuf::from(".")));
        RunContext {
            registry,
            limiter,
            model: "",
            adapter: "_noop",
            runtime_config,
            dry_run: false,
            armed: false,
            adapters,
            default_adapter: std::rc::Rc::new(crate::adapter::NoopAdapter),
            model_locked: false,
            adapter_timeout_seconds: 120,
            complexity,
            decomposition_depth: 0,
            orchestration_dir: dir.as_path(),
            declared_prompts,
            effect_names,
            display_depth: 0,
            supported: electricity_bytecode::Supported::m0(&[]),
        }
    }

    fn leaf_value(node: &NodeRef, key: &str) -> Option<Value> {
        match node.borrow().get(&Value::Str(key.to_string())) {
            Some(Slot::Value(v)) => Some(v.clone()),
            _ => None,
        }
    }

    /// Builds a store shaped like a real run (a `prime` node under the
    /// root, holding every effect; any extra top-level *ctx* entries --
    /// `input`, say -- set directly on the root beside it), compiles a
    /// single `yield` leaf named *name* with *template*/*inputs*/
    /// *on_error* under `prime`, runs it against the whole store as the
    /// render context, and returns `(store, prime, result)` -- shared
    /// by every test below. The `prime` node matters here, not just for
    /// realism: `resolve_effect_text`'s own `prime.<head>` presence
    /// check has nothing to find at all if an effect's own node isn't
    /// actually reachable at that path.
    async fn run_yield(
        name: &str,
        template: &str,
        inputs: Option<electricity_bytecode::ParamNode>,
        on_error: OnError,
        ctx: Vec<(&str, Value)>,
    ) -> (Store, NodeRef, Result<(), VmError>) {
        let store = Store::new();
        let prime = store
            .ensure_dict(&store.root, Value::from("prime"))
            .unwrap();
        for (key, value) in ctx {
            store.set_leaf(&store.root, Value::from(key), value);
        }
        let registry = ToolRegistry::new();
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let declared_prompts = indexmap::IndexMap::new();
        let mut effect_names = std::collections::BTreeSet::new();
        effect_names.insert(name.to_string());
        let run_ctx = default_run_ctx(
            &registry,
            &limiter,
            &runtime_config,
            &declared_prompts,
            &effect_names,
        );
        let mut op = yield_node(name, on_error);
        op.kind = NodeKind::Leaf(Box::new(LeafKind::Yield(yield_op_leaf(template, inputs))));
        let LeafKind::Yield(yield_op) = (match &op.kind {
            NodeKind::Leaf(leaf) => leaf.as_ref(),
            NodeKind::Control(_) => unreachable!(),
        }) else {
            unreachable!()
        };
        let frame = Frame::root();
        let token = CancellationToken::new();
        let observer = NullObserver;
        let chain: CtxChain = vec![CtxSource::Live(std::rc::Rc::clone(&store.root))];
        let result = execute_yield(
            &op, yield_op, &store, &prime, &chain, &frame, &run_ctx, &observer, &token,
        )
        .await;
        (store, prime, result)
    }

    #[tokio::test]
    async fn a_successful_render_writes_value_and_completed_at() {
        let (_, prime, result) =
            run_yield("greeting", "hello", None, OnError::Fail, Vec::new()).await;
        result.unwrap();
        let node = prime.borrow();
        let node = match node.get(&Value::from("greeting")) {
            Some(Slot::Node(n)) => n.clone(),
            _ => panic!("expected a dict node"),
        };
        assert_eq!(leaf_value(&node, "value"), Some(Value::from("hello")));
        let meta = match node.borrow().get(&Value::from("meta")) {
            Some(Slot::Node(n)) => n.clone(),
            _ => panic!("expected a dict node"),
        };
        assert_eq!(leaf_value(&meta, "error"), Some(Value::None));
        assert!(leaf_value(&meta, "completed_at").is_some());
        assert_ne!(leaf_value(&meta, "completed_at"), Some(Value::None));
    }

    #[tokio::test]
    async fn a_render_failure_under_fail_raises_template_error() {
        let (_, prime, result) =
            run_yield("say", "{{> nope}}", None, OnError::Fail, Vec::new()).await;
        let err = result.unwrap_err();
        let VmError::Raised {
            py_class, message, ..
        } = err
        else {
            panic!("expected a VmError::Raised failure");
        };
        assert_eq!(py_class, electricity_tools::PyExc::TemplateError);
        assert_eq!(
            message,
            "yield 'say' template: '{{> nope}}' does not name a declared prompt or effect."
        );
        let node = match prime.borrow().get(&Value::from("say")) {
            Some(Slot::Node(n)) => n.clone(),
            _ => panic!("expected a dict node"),
        };
        assert_eq!(leaf_value(&node, "value"), Some(Value::None));
        let meta = match node.borrow().get(&Value::from("meta")) {
            Some(Slot::Node(n)) => n.clone(),
            _ => panic!("expected a dict node"),
        };
        assert_eq!(leaf_value(&meta, "error"), Some(Value::from(message)));
    }

    #[tokio::test]
    async fn a_render_failure_under_continue_is_absorbed() {
        let (_, prime, result) =
            run_yield("say", "{{> nope}}", None, OnError::Continue, Vec::new()).await;
        result.unwrap();
        let node = match prime.borrow().get(&Value::from("say")) {
            Some(Slot::Node(n)) => n.clone(),
            _ => panic!("expected a dict node"),
        };
        assert_eq!(leaf_value(&node, "value"), Some(Value::None));
        let meta = match node.borrow().get(&Value::from("meta")) {
            Some(Slot::Node(n)) => n.clone(),
            _ => panic!("expected a dict node"),
        };
        assert!(leaf_value(&meta, "error").is_some());
        assert_ne!(leaf_value(&meta, "error"), Some(Value::None));
    }

    #[tokio::test]
    async fn raw_inputs_shadow_a_top_level_key() {
        let mut inputs = indexmap::IndexMap::new();
        inputs.insert(Value::from("input"), Value::from("shadowed"));
        let (_, prime, result) = run_yield(
            "say",
            "{{input}}",
            Some(electricity_bytecode::ParamNode::Literal(Value::Dict(
                inputs,
            ))),
            OnError::Fail,
            vec![("input", Value::from("real"))],
        )
        .await;
        result.unwrap();
        let node = match prime.borrow().get(&Value::from("say")) {
            Some(Slot::Node(n)) => n.clone(),
            _ => panic!("expected a dict node"),
        };
        assert_eq!(leaf_value(&node, "value"), Some(Value::from("shadowed")));
    }

    #[tokio::test]
    async fn a_dry_run_never_renders_and_leaves_value_none() {
        let (_, prime, result) = {
            let store = Store::new();
            let prime = store
                .ensure_dict(&store.root, Value::from("prime"))
                .unwrap();
            let registry = ToolRegistry::new();
            let limiter = crate::Limiter::new();
            let runtime_config = Value::None;
            let declared_prompts = indexmap::IndexMap::new();
            let mut effect_names = std::collections::BTreeSet::new();
            effect_names.insert("say".to_string());
            let base_run_ctx = default_run_ctx(
                &registry,
                &limiter,
                &runtime_config,
                &declared_prompts,
                &effect_names,
            );
            let run_ctx = RunContext {
                dry_run: true,
                ..base_run_ctx
            };
            let mut op = yield_node("say", OnError::Fail);
            op.kind = NodeKind::Leaf(Box::new(LeafKind::Yield(yield_op_leaf(
                // A template that would fail if it were ever rendered --
                // proof that a dry run skips the render entirely, the
                // same way `exec::use_`'s own dry-run branch does.
                "{{> nope}}",
                None,
            ))));
            let LeafKind::Yield(yield_op) = (match &op.kind {
                NodeKind::Leaf(leaf) => leaf.as_ref(),
                NodeKind::Control(_) => unreachable!(),
            }) else {
                unreachable!()
            };
            let frame = Frame::root();
            let token = CancellationToken::new();
            let observer = NullObserver;
            let chain: CtxChain = vec![CtxSource::Live(std::rc::Rc::clone(&store.root))];
            let result = execute_yield(
                &op, yield_op, &store, &prime, &chain, &frame, &run_ctx, &observer, &token,
            )
            .await;
            (store, prime, result)
        };
        result.unwrap();
        let node = match prime.borrow().get(&Value::from("say")) {
            Some(Slot::Node(n)) => n.clone(),
            _ => panic!("expected a dict node"),
        };
        assert_eq!(leaf_value(&node, "value"), Some(Value::None));
        let meta = match node.borrow().get(&Value::from("meta")) {
            Some(Slot::Node(n)) => n.clone(),
            _ => panic!("expected a dict node"),
        };
        assert_eq!(leaf_value(&meta, "error"), Some(Value::None));
        assert!(leaf_value(&meta, "completed_at").is_some());
        assert_ne!(leaf_value(&meta, "completed_at"), Some(Value::None));
    }

    #[tokio::test]
    async fn a_self_reference_resolves_to_its_own_current_value() {
        // The node already exists (with `value: None`) by the time the
        // template renders, so `{{> self}}` resolves to that -- `""`,
        // since the resolved value is `None`. It must not error out
        // trying to resolve itself.
        let (_, prime, result) =
            run_yield("self", "[{{> self}}]", None, OnError::Fail, Vec::new()).await;
        result.unwrap();
        let node = match prime.borrow().get(&Value::from("self")) {
            Some(Slot::Node(n)) => n.clone(),
            _ => panic!("expected a dict node"),
        };
        assert_eq!(leaf_value(&node, "value"), Some(Value::from("[]")));
    }

    #[test]
    fn inputs_overlay_contains_never_materializes_a_store_backed_loop_node() {
        let store = Store::new();
        let prime = store
            .ensure_dict(&store.root, Value::from("prime"))
            .unwrap();
        // A named-loop-shaped node: large enough that a full
        // `Store::snapshot` of it would be a real cost, not just
        // incidentally harmless (review finding 1 on issue #492's PR).
        let named_loop = store
            .ensure_dict(&prime, Value::from("named_loop"))
            .unwrap();
        for i in 0..200 {
            let pass = store
                .ensure_dict(&named_loop, Value::from(format!("iter_{i}")))
                .unwrap();
            store.set_leaf(&pass, Value::from("padding"), Value::from("x".repeat(64)));
        }
        let chain: CtxChain = vec![CtxSource::Live(std::rc::Rc::clone(&store.root))];
        let live_root = LiveRoot::new(&chain, &store);
        let mut inputs = Dict::new();
        inputs.insert(Value::from("extra"), Value::from("v"));
        let overlay = InputsOverlay {
            inputs: &inputs,
            inner: &live_root,
        };

        assert!(overlay.contains("prime.named_loop"));
        assert!(!overlay.contains("prime.missing"));
        assert!(overlay.contains("extra"));
        assert_eq!(
            store.snapshot_call_count(),
            0,
            "a presence check must never materialize the whole loop subtree"
        );
    }

    #[tokio::test]
    async fn execute_yield_inputs_presence_check_never_materializes_a_named_loop() {
        let store = Store::new();
        let prime = store
            .ensure_dict(&store.root, Value::from("prime"))
            .unwrap();
        // `{{> named_loop.x}}`'s own presence check probes exactly
        // `prime.named_loop` -- padded with many passes so a
        // `Store::snapshot` of the whole subtree (the pre-fix bug: the
        // `InputsOverlay`'s own missing `contains` override falling
        // through to `LiveRoot::lookup`, which materializes a `Slot::
        // Node` the moment no path segment is left) would be a real,
        // detectable cost, not an incidental one.
        let named_loop = store
            .ensure_dict(&prime, Value::from("named_loop"))
            .unwrap();
        for i in 0..200 {
            let pass = store
                .ensure_dict(&named_loop, Value::from(format!("iter_{i}")))
                .unwrap();
            store.set_leaf(&pass, Value::from("padding"), Value::from("x".repeat(64)));
        }
        let x_node = store.ensure_dict(&named_loop, Value::from("x")).unwrap();
        store.set_leaf(&x_node, Value::from("value"), Value::from("hi"));
        store.set_leaf(&x_node, Value::from("meta"), Value::Dict(Dict::new()));

        let registry = ToolRegistry::new();
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let declared_prompts = indexmap::IndexMap::new();
        let mut effect_names = std::collections::BTreeSet::new();
        effect_names.insert("named_loop".to_string());
        effect_names.insert("say".to_string());
        let run_ctx = default_run_ctx(
            &registry,
            &limiter,
            &runtime_config,
            &declared_prompts,
            &effect_names,
        );

        let mut inputs = indexmap::IndexMap::new();
        inputs.insert(Value::from("other"), Value::from("v"));
        let mut op = yield_node("say", OnError::Fail);
        op.kind = NodeKind::Leaf(Box::new(LeafKind::Yield(yield_op_leaf(
            "{{> named_loop.x}}",
            Some(electricity_bytecode::ParamNode::Literal(Value::Dict(
                inputs,
            ))),
        ))));
        let LeafKind::Yield(yield_op) = (match &op.kind {
            NodeKind::Leaf(leaf) => leaf.as_ref(),
            NodeKind::Control(_) => unreachable!(),
        }) else {
            unreachable!()
        };
        let frame = Frame::root();
        let token = CancellationToken::new();
        let observer = NullObserver;
        let chain: CtxChain = vec![CtxSource::Live(std::rc::Rc::clone(&store.root))];
        let result = execute_yield(
            &op, yield_op, &store, &prime, &chain, &frame, &run_ctx, &observer, &token,
        )
        .await;
        result.unwrap();
        assert_eq!(
            store.snapshot_call_count(),
            0,
            "a yield with raw inputs must never materialize the whole loop subtree \
             just to resolve a {{> name}} presence check"
        );
        let node = match prime.borrow().get(&Value::from("say")) {
            Some(Slot::Node(n)) => n.clone(),
            _ => panic!("expected a dict node"),
        };
        assert_eq!(leaf_value(&node, "value"), Some(Value::from("hi")));
    }

    // -----------------------------------------------------------------
    // Replays `yield_cases` from `electricity/scripts/
    // generate_compose_corpus.py`'s own `tests/golden/compose_corpus.json`
    // -- real output from `core/yield_effect.py::YieldRuntime.execute` --
    // against `execute_yield` itself. In-crate (not `tests/
    // compose_corpus.rs`): a self-reference (`{{> self}}`) needs a
    // *live* root that can see the very node this call creates, which
    // only a `CtxChain`/`LiveRoot`-backed render gives -- both are
    // `pub(crate)`, unreachable from an external integration-test crate.
    // -----------------------------------------------------------------

    /// A corpus case's own JSON fields that need their dict order
    /// preserved (`inputs`/`extra_ctx`/`pre_value`/`node`) are captured
    /// as the original bytes (`Box<RawValue>`, never `serde_json::
    /// Value`) and parsed here through `electricity-json`'s own
    /// hand-rolled reader instead -- unconditionally order-preserving,
    /// with no `serde_json` feature (`electricity-vm`'s own `Cargo.toml`
    /// comment on its `serde_json` dev-dependency explains why
    /// `preserve_order` isn't that feature).
    fn value_from_raw(raw: &serde_json::value::RawValue) -> Value {
        electricity_json::loads(raw.get()).expect("corpus JSON is valid")
    }

    const TS_PLACEHOLDER: &str = "<ts>";

    /// Replaces `meta.created_at`/`meta.completed_at` (when not `None`)
    /// with the fixed placeholder the generator itself writes, so a
    /// freshly-run timestamp never has to byte-match a committed one.
    fn scrub_timestamps(mut node: Value) -> Value {
        if let Value::Dict(node_dict) = &mut node {
            if let Some(Value::Dict(meta)) = node_dict.get_mut(&Value::from("meta")) {
                for key in ["created_at", "completed_at"] {
                    if let Some(value) = meta.get_mut(&Value::from(key)) {
                        if !matches!(value, Value::None) {
                            *value = Value::Str(TS_PLACEHOLDER.to_string());
                        }
                    }
                }
            }
        }
        node
    }

    #[derive(serde::Deserialize)]
    struct YieldCorpusCase {
        name: String,
        template: String,
        inputs: Option<Box<serde_json::value::RawValue>>,
        on_error: String,
        declared: indexmap::IndexMap<String, String>,
        known_effect_names: Vec<String>,
        extra_ctx: Box<serde_json::value::RawValue>,
        pre_value: Option<Box<serde_json::value::RawValue>>,
        node: Box<serde_json::value::RawValue>,
        raised: Option<RaisedInfo>,
    }

    #[derive(serde::Deserialize)]
    struct FileCorpus {
        yield_cases: Vec<YieldCorpusCase>,
    }

    #[derive(serde::Deserialize, Debug)]
    struct RaisedInfo {
        class: String,
        message: String,
    }

    fn on_error_from_str(s: &str) -> OnError {
        match s {
            "fail" => OnError::Fail,
            "skip" => OnError::Skip,
            "continue" => OnError::Continue,
            other => panic!("unexpected on_error {other:?}"),
        }
    }

    #[tokio::test]
    async fn yield_cases_match_pythons_yield_runtime() {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/golden/compose_corpus.json"
        ))
        .expect("compose_corpus.json exists -- run electricity/scripts/generate_compose_corpus.py");
        let corpus: FileCorpus =
            serde_json::from_str(&text).expect("compose_corpus.json is valid JSON");
        assert!(!corpus.yield_cases.is_empty());

        for case in corpus.yield_cases {
            let store = Store::new();
            let prime = store
                .ensure_dict(&store.root, Value::from("prime"))
                .unwrap();
            if let Some(pre) = &case.pre_value {
                let mut node = Dict::new();
                node.insert(Value::from("value"), value_from_raw(pre));
                node.insert(Value::from("meta"), Value::Dict(Dict::new()));
                store.set_leaf(&prime, Value::from(case.name.as_str()), Value::Dict(node));
            }
            // `match &mut ... { Value::Dict(d) => mem::take(d), ... }`, not
            // a move-out pattern -- `Value`'s own `Drop` impl forbids
            // partially moving a variant's payload out of an owned value
            // (same fix as `params.rs`/`render.rs`'s own identical comment).
            let mut extra_ctx_value = value_from_raw(&case.extra_ctx);
            let extra_ctx = match &mut extra_ctx_value {
                Value::Dict(dict) => std::mem::take(dict),
                _ => panic!("extra_ctx is always a JSON object"),
            };
            for (key, value) in &extra_ctx {
                store.set_leaf(&store.root, key.clone(), value.clone());
            }

            let registry = ToolRegistry::new();
            let limiter = crate::Limiter::new();
            let runtime_config = Value::None;
            let known: std::collections::BTreeSet<String> =
                case.known_effect_names.iter().cloned().collect();
            let run_ctx =
                default_run_ctx(&registry, &limiter, &runtime_config, &case.declared, &known);

            let inputs = case.inputs.as_ref().map(|raw| {
                let mut raw_value = value_from_raw(raw);
                let dict = match &mut raw_value {
                    Value::Dict(dict) => std::mem::take(dict),
                    _ => panic!("inputs is always a JSON object"),
                };
                electricity_bytecode::ParamNode::Literal(Value::Dict(dict))
            });
            let mut op = yield_node(&case.name, on_error_from_str(&case.on_error));
            op.kind = NodeKind::Leaf(Box::new(LeafKind::Yield(yield_op_leaf(
                &case.template,
                inputs,
            ))));
            let LeafKind::Yield(yield_op) = (match &op.kind {
                NodeKind::Leaf(leaf) => leaf.as_ref(),
                NodeKind::Control(_) => unreachable!(),
            }) else {
                unreachable!()
            };
            let frame = Frame::root();
            let token = CancellationToken::new();
            let observer = NullObserver;
            let chain: CtxChain = vec![CtxSource::Live(std::rc::Rc::clone(&store.root))];
            let result = execute_yield(
                &op, yield_op, &store, &prime, &chain, &frame, &run_ctx, &observer, &token,
            )
            .await;

            match (&case.raised, result) {
                (
                    Some(expected),
                    Err(VmError::Raised {
                        py_class, message, ..
                    }),
                ) => {
                    assert_eq!(py_class.name(), expected.class, "case {:?}", case.name);
                    assert_eq!(message, expected.message, "case {:?}", case.name);
                }
                (None, Ok(())) => {}
                (expected, actual) => panic!(
                    "case {:?}: expected raised={:?}, got {:?}",
                    case.name, expected, actual
                ),
            }

            let node = match prime.borrow().get(&Value::from(case.name.as_str())) {
                Some(Slot::Node(n)) => n.clone(),
                _ => panic!("expected a dict node for case {:?}", case.name),
            };
            let actual_node = scrub_timestamps(store.snapshot(&node));
            let expected_node = value_from_raw(&case.node);
            assert_eq!(actual_node, expected_node, "case {:?}", case.name);
        }
    }
}
