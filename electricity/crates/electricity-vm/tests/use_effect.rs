//! M1 lane I (issue #469): `use` effect execution -- integration tests
//! against real, compiled child documents under `tests/fixtures/use/`
//! (the process cwd for a `cargo test` binary is this crate's own
//! root, confirmed directly: `std::env::current_dir()` -- so a
//! `path:` of `"tests/fixtures/use/child_basic.yml"` resolves exactly
//! the way a real run's own cwd-relative step would).

mod support;

use electricity_bytecode::{
    EffectPath, Escape, ExpectCondition, OnError, PREVIEW_MARKER, ParamNode, RetryPolicy, UseSource,
};
use electricity_value::{Dict, Value};
use electricity_vm::{CancellationToken, Limiter, Store, VmError};
use indexmap::IndexMap;
use support::{Event, RecordingObserver, json_registry, run_ctx};

fn root_path() -> EffectPath {
    EffectPath::root()
}

fn get<'a>(dict: &'a indexmap::IndexMap<Value, Value>, key: &str) -> Option<&'a Value> {
    dict.get(&Value::Str(key.to_string()))
}

fn as_dict(value: &Value) -> &indexmap::IndexMap<Value, Value> {
    match value {
        Value::Dict(d) => d,
        other => panic!("expected a dict, got {other:?}"),
    }
}

fn path_source(relative: &str) -> UseSource {
    UseSource::Path(relative.to_string())
}

async fn run_use(
    use_op: electricity_bytecode::UseOp,
    on_error: OnError,
    runtime_config: &Value,
) -> (Store, RecordingObserver, Result<(), VmError>) {
    run_use_with_supported(
        use_op,
        on_error,
        runtime_config,
        electricity_bytecode::Supported::m0(&[]),
    )
    .await
}

/// [`run_use`], with an explicit [`electricity_bytecode::Supported`]
/// (every fixture a nested `use` effect of its own needs `use_: true`
/// for -- `support::run_ctx`'s own default is `Supported::m0`, which
/// has `use_: false`: `first_unsupported`'s own per-child runtime
/// check judges a child's *own* content against this value, so a
/// child whose content is itself a `use` effect is refused outright
/// under the plain default, for a reason that has nothing to do with
/// whatever that test is actually probing).
async fn run_use_with_supported(
    use_op: electricity_bytecode::UseOp,
    on_error: OnError,
    runtime_config: &Value,
    supported: electricity_bytecode::Supported<'_>,
) -> (Store, RecordingObserver, Result<(), VmError>) {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    ctx.runtime_config = runtime_config;
    ctx.supported = supported;
    let observer = RecordingObserver::new();
    let op = support::use_leaf(
        root_path().push_name("step"),
        "step",
        use_op.source,
        use_op.inputs,
        use_op.outputs,
        use_op.validate,
        use_op.retries,
        use_op.expect,
        on_error,
    );
    let root = support::chain_dynamic(root_path(), "prime", OnError::Fail, vec![op]);
    let result = electricity_vm::execute_root(
        &support::program(root),
        &store,
        &Value::None,
        &ctx,
        &observer,
        &token,
    )
    .await;
    (store, observer, result)
}

fn base_use_op(source: UseSource) -> electricity_bytecode::UseOp {
    electricity_bytecode::UseOp {
        parent_trusted: false,
        source,
        inputs: None,
        outputs: None,
        validate: true,
        retries: RetryPolicy::default(),
        expect: None,
        description: None,
    }
}

#[tokio::test]
async fn a_path_child_runs_in_full_passthrough_mode() {
    let use_op = base_use_op(path_source("tests/fixtures/use/child_basic.yml"));
    let (store, _observer, result) = run_use(use_op, OnError::Fail, &Value::None).await;
    result.expect("a basic path child must succeed");

    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let use_node = as_dict(get(prime, "step").unwrap());
    // Full passthrough: the use node's own `value` stays null, and the
    // child's own `step` tool effect is copied directly onto it.
    assert_eq!(get(use_node, "value"), Some(&Value::None));
    let child_step = as_dict(get(use_node, "step").unwrap());
    assert_eq!(
        get(child_step, "value"),
        Some(&Value::from("\"hello-from-child\""))
    );
    let meta = as_dict(get(use_node, "meta").unwrap());
    assert_eq!(get(meta, "inline"), Some(&Value::Bool(false)));
    assert!(matches!(get(meta, "resolved_path"), Some(Value::Str(_))));
    assert!(matches!(
        get(meta, "orchestration_sha256"),
        Some(Value::Str(_))
    ));
    assert_eq!(get(meta, "child_errors"), Some(&Value::None));
}

#[tokio::test]
async fn a_path_child_with_explicit_outputs_maps_the_value() {
    let mut outputs = IndexMap::new();
    outputs.insert("greeting".to_string(), "prime.step.value".to_string());
    let mut use_op = base_use_op(path_source("tests/fixtures/use/child_basic.yml"));
    use_op.outputs = Some(outputs);
    let (store, _observer, result) = run_use(use_op, OnError::Fail, &Value::None).await;
    result.expect("explicit-outputs mode must succeed");

    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let use_node = as_dict(get(prime, "step").unwrap());
    let value = as_dict(get(use_node, "value").unwrap());
    assert_eq!(
        get(value, "greeting"),
        Some(&Value::from("\"hello-from-child\""))
    );
    // Explicit-outputs mode without `record_children` grafts nothing
    // extra onto the node.
    assert!(get(use_node, "step").is_none());
}

#[tokio::test]
async fn an_interface_declared_required_input_is_rendered_and_a_default_fills_the_rest() {
    let mut inputs = IndexMap::new();
    inputs.insert(
        Value::from("name"),
        ParamNode::Template(electricity_bytecode::TemplateText::new(
            "Ada".to_string(),
            true,
            Escape::Html,
        )),
    );
    let mut use_op = base_use_op(path_source("tests/fixtures/use/child_with_interface.yml"));
    use_op.inputs = Some(ParamNode::Map(inputs));
    let (store, _observer, result) = run_use(use_op, OnError::Fail, &Value::None).await;
    result.expect("a satisfied required input must succeed");

    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let use_node = as_dict(get(prime, "step").unwrap());
    let value = as_dict(get(use_node, "value").unwrap());
    assert_eq!(get(value, "greeting"), Some(&Value::from("\"Ada\"")));
    let meta = as_dict(get(use_node, "meta").unwrap());
    let recorded_inputs = as_dict(get(meta, "inputs").unwrap());
    assert_eq!(get(recorded_inputs, "name"), Some(&Value::from("Ada")));
    // `times` has no corresponding entry in `use.inputs:` at all -- the
    // interface's own `default: 1` fills it in.
    assert_eq!(get(recorded_inputs, "times"), Some(&Value::from(1i64)));
}

#[tokio::test]
async fn a_missing_required_input_fails_with_the_exact_text_and_is_never_retried() {
    let mut use_op = base_use_op(path_source("tests/fixtures/use/child_with_interface.yml"));
    use_op.retries = RetryPolicy {
        max_attempts: 3,
        backoff_ms: 0,
    };
    let (store, _observer, result) = run_use(use_op, OnError::Fail, &Value::None).await;
    let err = result.expect_err("a missing required input must fail the run");
    let VmError::Raised { message, .. } = err else {
        panic!("expected a VmError::Raised, got {err:?}");
    };
    assert!(
        message.contains("missing required input 'name' declared in orchestration interface"),
        "got: {message}"
    );

    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let use_node = as_dict(get(prime, "step").unwrap());
    let meta = as_dict(get(use_node, "meta").unwrap());
    // A `ValueError`-class failure is never retried, whatever
    // `retries.max_attempts` says -- `retries_used` stays absent.
    assert!(get(meta, "retries_used").is_none());
}

#[tokio::test]
async fn a_cycle_between_two_path_children_is_detected_with_the_exact_arrow_text() {
    let use_op = base_use_op(path_source("tests/fixtures/use/cycle_a.yml"));
    let supported = electricity_bytecode::Supported {
        use_: true,
        ..electricity_bytecode::Supported::m0(&[])
    };
    let (_store, _observer, result) =
        run_use_with_supported(use_op, OnError::Fail, &Value::None, supported).await;
    let err = result.expect_err("a cycle must fail the run");
    let VmError::Raised { message, .. } = err else {
        panic!("expected a VmError::Raised, got {err:?}");
    };
    // Review finding 6 on PR #472: this used to only check `contains("
    // → ")`, but the PR body calls this "exact arrow text" -- so check
    // the full, real message, both resolved identities included.
    let identity_a = std::fs::canonicalize("tests/fixtures/use/cycle_a.yml")
        .expect("cycle_a.yml must exist")
        .display()
        .to_string();
    let identity_b = std::fs::canonicalize("tests/fixtures/use/cycle_b.yml")
        .expect("cycle_b.yml must exist")
        .display()
        .to_string();
    let expected = format!(
        "prime.step: use 'step' -> tests/fixtures/use/cycle_a.yml: prime.to_b: use 'to_b' -> \
         {identity_b}: prime.to_a: use 'to_a' -> {identity_a}: use 'to_a': cycle detected — \
         {identity_a} → {identity_b} → {identity_a}"
    );
    assert_eq!(message, expected);
}

#[tokio::test]
async fn record_children_grafts_the_childs_own_effects_alongside_an_explicit_output() {
    let mut outputs = IndexMap::new();
    outputs.insert("greeting".to_string(), "prime.step.value".to_string());
    let mut use_op = base_use_op(path_source("tests/fixtures/use/child_basic.yml"));
    use_op.outputs = Some(outputs);

    let mut state_cfg = Dict::new();
    state_cfg.insert(Value::from("record_children"), Value::Bool(true));
    let mut runtime = Dict::new();
    runtime.insert(Value::from("state"), Value::Dict(state_cfg));
    let runtime_config = Value::Dict(runtime);

    let (store, _observer, result) = run_use(use_op, OnError::Fail, &runtime_config).await;
    result.expect("record_children must not itself fail the run");

    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let use_node = as_dict(get(prime, "step").unwrap());
    let value = as_dict(get(use_node, "value").unwrap());
    assert_eq!(
        get(value, "greeting"),
        Some(&Value::from("\"hello-from-child\""))
    );
    // `record_children` also grafts the child's own `step` tool node
    // onto the use node, alongside the mapped `value`.
    let child_step = as_dict(get(use_node, "step").unwrap());
    assert_eq!(
        get(child_step, "value"),
        Some(&Value::from("\"hello-from-child\""))
    );
}

#[tokio::test]
async fn a_failed_expect_is_retried_and_retries_used_is_recorded_on_final_failure() {
    let mut use_op = base_use_op(path_source("tests/fixtures/use/child_basic.yml"));
    use_op.retries = RetryPolicy {
        max_attempts: 2,
        backoff_ms: 0,
    };
    use_op.expect = Some(ExpectCondition::Cel {
        expr: "false".to_string(),
    });
    let (store, _observer, result) = run_use(use_op, OnError::Fail, &Value::None).await;
    let err = result.expect_err("a failed expect must fail the run once retries are exhausted");
    let VmError::Raised { message, .. } = err else {
        panic!("expected a VmError::Raised, got {err:?}");
    };
    assert!(message.contains("expect failed: false"), "got: {message}");

    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let use_node = as_dict(get(prime, "step").unwrap());
    let meta = as_dict(get(use_node, "meta").unwrap());
    assert_eq!(get(meta, "retries_used"), Some(&Value::from(1i64)));
}

#[tokio::test]
async fn on_error_skip_absorbs_a_final_failure_and_clears_the_value() {
    let mut use_op = base_use_op(path_source("tests/fixtures/use/child_basic.yml"));
    use_op.expect = Some(ExpectCondition::Cel {
        expr: "false".to_string(),
    });
    let (store, _observer, result) = run_use(use_op, OnError::Skip, &Value::None).await;
    result.expect("on_error: skip must absorb the failure");

    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let use_node = as_dict(get(prime, "step").unwrap());
    assert_eq!(get(use_node, "value"), Some(&Value::None));
    let meta = as_dict(get(use_node, "meta").unwrap());
    assert!(matches!(get(meta, "error"), Some(Value::Str(_))));
}

#[tokio::test]
async fn the_child_roots_own_pair_is_suppressed_and_every_other_hook_forwards_namespaced() {
    let use_op = base_use_op(path_source("tests/fixtures/use/child_basic.yml"));
    let (_store, observer, result) = run_use(use_op, OnError::Fail, &Value::None).await;
    result.expect("a basic path child must succeed");

    let events = observer.events();
    // The use node's own pair (fired by `execute_use` itself) is the
    // only `prime.step` start/complete pair -- the child's own root
    // (which would otherwise resolve to the exact same path) never
    // fires a second one.
    let starts_at_step = events
        .iter()
        .filter(|e| matches!(e, Event::Start(p) if p == "prime.step"))
        .count();
    assert_eq!(starts_at_step, 1, "got {events:?}");
    let completes_at_step = events
        .iter()
        .filter(|e| matches!(e, Event::Complete(p, _) if p == "prime.step"))
        .count();
    assert_eq!(completes_at_step, 1, "got {events:?}");

    // The child's own `step` tool effect fires its own pair, correctly
    // namespaced under the use node's own path.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Start(p) if p == "prime.step.step")),
        "got {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Complete(p, None) if p == "prime.step.step")),
        "got {events:?}"
    );

    // Every child write grafts under the use node's own path, and the
    // graft is cleared once the child finishes, before the run ends.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Graft(p, _) if p == "prime.step")),
        "got {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ClearGraft(p) if p == "prime.step")),
        "got {events:?}"
    );
}

/// Review finding 6 on PR #472: every existing refused-child test used
/// an *inline* child (`support::refused_use_leaf`); this one is a real
/// `path:` child, with an ordinary `json` step placed *before* the
/// unsupported `prompt` step in the same chain -- `first_unsupported`
/// runs on the whole freshly-compiled child program before
/// `run_isolated_inner` ever starts it, so the refusal must stop the
/// `json` step from running too, not just the `prompt` step that
/// triggers it.
#[tokio::test]
async fn a_refused_path_child_never_runs_its_own_earlier_json_step() {
    let use_op = base_use_op(path_source(
        "tests/fixtures/use/child_refused_after_json.yml",
    ));
    let (store, observer, result) = run_use(use_op, OnError::Fail, &Value::None).await;
    let err = result.expect_err("a child naming an unsupported effect must be refused");
    assert!(matches!(err, VmError::Refused { .. }), "got {err:?}");

    let events = observer.events();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::Start(p) if p.contains("ran"))),
        "the child's own earlier json step must never start: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::Complete(p, _) if p.contains("ran"))),
        "the child's own earlier json step must never complete: {events:?}"
    );

    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let use_node = as_dict(get(prime, "step").unwrap());
    assert!(
        get(use_node, "ran").is_none(),
        "a refused child's own json step must never be merged onto the use node: {use_node:?}"
    );
    let meta = as_dict(get(use_node, "meta").unwrap());
    let Some(Value::Str(error_text)) = get(meta, "error") else {
        panic!("expected meta.error to carry the refusal text, got {meta:?}");
    };
    assert!(error_text.contains(PREVIEW_MARKER), "got {error_text:?}");
}

/// Ruling R3 on PR #472's review (issue #469's own test
/// plan: "A use inside a tree dynamic ... no false cycle between
/// sibling passes"). Not a conformance case: a named `dynamic`
/// container's own `meta.adapter` picks up the same use-triggered
/// real-adapter heuristic the root document's does, and the
/// conformance harness's `known_divergence` can only ever name one
/// location -- a Rust test avoids the cross-engine comparison
/// entirely while still exercising `Frame::push_use`'s own fresh
/// `Vec` per branch (this module's own doc comment on why a shared
/// stack would be wrong here).
#[tokio::test]
async fn use_inside_a_tree_dynamic_has_no_false_cycle_between_sibling_branches() {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();

    let branch_path = root_path().push_name("fan");
    let first = support::use_leaf(
        branch_path.push_name("first"),
        "first",
        path_source("tests/fixtures/use/child_basic.yml"),
        None,
        None,
        true,
        RetryPolicy::default(),
        None,
        OnError::Fail,
    );
    let second = support::use_leaf(
        branch_path.push_name("second"),
        "second",
        path_source("tests/fixtures/use/child_basic.yml"),
        None,
        None,
        true,
        RetryPolicy::default(),
        None,
        OnError::Fail,
    );
    let fan = support::tree_dynamic(
        branch_path,
        "fan",
        OnError::Fail,
        None,
        false,
        vec![first, second],
    );
    let root = support::chain_dynamic(root_path(), "prime", OnError::Fail, vec![fan]);

    let result = electricity_vm::execute_root(
        &support::program(root),
        &store,
        &Value::None,
        &ctx,
        &observer,
        &token,
    )
    .await;
    result.expect("two tree-dynamic siblings using the identical child must never false-cycle");

    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let fan_node = as_dict(get(prime, "fan").unwrap());
    for name in ["first", "second"] {
        let use_node = as_dict(get(fan_node, name).unwrap());
        let meta = as_dict(get(use_node, "meta").unwrap());
        assert_eq!(get(meta, "error"), Some(&Value::None), "{name}: {meta:?}");
    }
}

/// #483: a `use` child's own tree `dynamic` dispatch event carries
/// the same absolute path its own start/end events use, namespaced
/// under the use node like every other child effect (this test used
/// to pin the opposite, unnamespaced behaviour per a now-superseded
/// review finding 3 on PR #472). Also a Rust test, not a conformance
/// case, for the same `known_divergence` single-location reason as
/// the sibling test above (the child's own dynamic container also
/// picks up the use-triggered real-adapter heuristic).
#[tokio::test]
async fn use_child_containing_a_tree_dynamic_dispatch_is_namespaced_under_the_use_node() {
    let inline_yaml = "\
effects:
  - type: dynamic
    name: fan
    flow: tree
    max_concurrency: 2
    effects:
      - type: tool
        name: a
        provider: json
        params: {mode: stringify, input: 1}
      - type: tool
        name: b
        provider: json
        params: {mode: stringify, input: 2}
";
    let use_op = base_use_op(UseSource::Inline(electricity_bytecode::TemplateText::new(
        inline_yaml.to_string(),
        true,
        Escape::Html,
    )));
    let (_store, observer, result) = run_use(use_op, OnError::Fail, &Value::None).await;
    result.expect("a child containing a tree dynamic must succeed");

    let events = observer.events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Dispatch(p, _, _) if p == "prime.step.fan")),
        "the child's dispatch event must carry the same path its start/end events use, \
         prime.step.fan: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::Dispatch(p, _, _) if p == "prime.fan")),
        "the child's dispatch event must never carry its own unnamespaced prime.<loop> path: \
         {events:?}"
    );

    // Issue #465: the child's own nested tree registers its branches,
    // and clears them at merge, under that same namespaced path --
    // `ChildObserver`'s own `register_tree_branches`/`clear_tree_
    // branches` forward unchanged, exactly like `dispatch` above.
    assert!(
        events.iter().any(
            |e| matches!(e, Event::RegisterTreeBranches(p, n) if p == "prime.step.fan" && *n == 2)
        ),
        "the child's own tree must register both branches under its namespaced path: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ClearTreeBranches(p) if p == "prime.step.fan")),
        "the child's own tree must clear its branches once it merges: {events:?}"
    );
}

/// Ruling R4 on PR #472's review: without a `--live-state`
/// mirror (`RunObserver::wants_child_snapshots` default `false`), a
/// `use` child's own writes must never trigger a snapshot-and-graft
/// pair at all -- not even once. A multi-step child (several leaf
/// writes) proves this isn't merely "throttled down to one"; zero
/// `Event::Graft` calls land regardless of how many times the child
/// itself writes.
#[tokio::test]
async fn without_a_live_state_mirror_a_use_child_never_snapshots_for_a_graft() {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new().with_child_snapshots(false);

    let inline_yaml = "\
effects:
  - type: tool
    name: a
    provider: json
    params: {mode: stringify, input: 1}
  - type: tool
    name: b
    provider: json
    params: {mode: stringify, input: 2}
  - type: tool
    name: c
    provider: json
    params: {mode: stringify, input: 3}
";
    let use_op = base_use_op(UseSource::Inline(electricity_bytecode::TemplateText::new(
        inline_yaml.to_string(),
        true,
        Escape::Html,
    )));
    let op = support::use_leaf(
        root_path().push_name("step"),
        "step",
        use_op.source,
        use_op.inputs,
        use_op.outputs,
        use_op.validate,
        use_op.retries,
        use_op.expect,
        OnError::Fail,
    );
    let root = support::chain_dynamic(root_path(), "prime", OnError::Fail, vec![op]);
    let result = electricity_vm::execute_root(
        &support::program(root),
        &store,
        &Value::None,
        &ctx,
        &observer,
        &token,
    )
    .await;
    result.expect("a child with several ordinary steps must succeed");

    let events = observer.events();
    assert!(
        !events.iter().any(|e| matches!(e, Event::Graft(_, _))),
        "no Event::Graft may ever appear when the parent observer doesn't want child \
         snapshots, however many times the child itself wrote: {events:?}"
    );
    // `clear_graft` is still fine to call unconditionally (a no-op for
    // a path that was never grafted) -- this isn't gated on the same
    // capability, only the snapshot-taking `write()` hook is.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ClearGraft(p) if p == "prime.step")),
        "got {events:?}"
    );
}
