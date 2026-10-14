//! Lane B2: `if`/`conditional` in `mode: cel` -- container-only tests
//! (issue #431's Lane B section).

mod support;

use async_trait::async_trait;
use electricity_bytecode::{EffectPath, Escape, OnError, ParamNode, TemplateText};
use electricity_tools::ToolRegistry;
use electricity_value::Value;
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult,
};
use electricity_vm::{CancellationToken, Limiter, Store, VmError};
use std::rc::Rc;
use support::{
    CancelOnStart, Event, RecordingObserver, TOOL_FAIL_TEXT, cel_if, chain_dynamic, json_registry,
    json_tool_leaf, model_if, refused_use_leaf, run_ctx, tool_leaf, tree_dynamic,
};

/// A `generate` that always answers `"yes"`, with token counts --
/// issue #513 review finding 4: lets a model `if`'s own container-level
/// `record_totals` hook be exercised end to end, not just probed for
/// reaching `NoopAdapter`'s own failure.
struct YesAdapter;

#[async_trait(?Send)]
impl Adapter for YesAdapter {
    fn name(&self) -> &str {
        "yes"
    }

    async fn generate(
        &self,
        _model: &str,
        _prompt: &str,
        _options: &GenerateOptions,
        _call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        Ok(GenerateResult {
            text: "yes".to_string(),
            raw: Value::None,
            tokens_sent: Some(1),
            tokens_received: Some(1),
            finish_reason: None,
            warnings: Vec::new(),
            cost_usd: None,
        })
    }

    async fn check(&self) -> CheckResult {
        CheckResult {
            ok: true,
            ..Default::default()
        }
    }
}

/// A `generate` that never resolves -- lets a cancellation test race
/// the token against a real in-flight call deterministically, rather
/// than against a call that might already have returned.
struct ForeverAdapter;

#[async_trait(?Send)]
impl Adapter for ForeverAdapter {
    fn name(&self) -> &str {
        "forever"
    }

    async fn generate(
        &self,
        _model: &str,
        _prompt: &str,
        _options: &GenerateOptions,
        _call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        std::future::pending().await
    }

    async fn check(&self) -> CheckResult {
        CheckResult {
            ok: true,
            ..Default::default()
        }
    }
}

async fn run_with_adapter(
    root_op: electricity_bytecode::Op,
    adapter: Rc<dyn Adapter>,
    token: CancellationToken,
) -> (Store, RecordingObserver, Result<(), VmError>) {
    let store = Store::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    ctx.default_adapter = adapter;
    let observer = RecordingObserver::new();
    let result = electricity_vm::execute_root(
        &support::program(root_op),
        &store,
        &Value::None,
        &ctx,
        &observer,
        &token,
    )
    .await;
    (store, observer, result)
}

/// Issue #513 review finding 4: a named model `if`'s own successful
/// pass calls `record_totals` exactly once.
#[tokio::test]
async fn a_named_model_if_calls_record_totals_once_on_success() {
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![model_if(
            root_path().push_name("gate"),
            Some("gate"),
            "ok?",
            OnError::Fail,
        )],
    );
    let (_, observer, result) =
        run_with_adapter(root, Rc::new(YesAdapter), CancellationToken::new()).await;
    result.expect("a model if answered 'yes' must succeed");
    let totals = observer
        .events()
        .into_iter()
        .filter(|e| matches!(e, Event::Totals(_)))
        .count();
    assert_eq!(
        totals, 1,
        "a named model if must call record_totals exactly once"
    );
}

/// The unnamed half of the same pair: an unnamed model `if` never calls
/// `record_totals` at all, the same way it writes no `meta`/`value`
/// node (`decide_and_run`'s own `named` guard).
#[tokio::test]
async fn an_unnamed_model_if_never_calls_record_totals() {
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![model_if(
            root_path().push_name("gate"),
            None,
            "ok?",
            OnError::Fail,
        )],
    );
    let (_, observer, result) =
        run_with_adapter(root, Rc::new(YesAdapter), CancellationToken::new()).await;
    result.expect("an unnamed model if answered 'yes' must succeed");
    assert!(
        !observer
            .events()
            .iter()
            .any(|e| matches!(e, Event::Totals(_))),
        "an unnamed model if must never call record_totals"
    );
}

/// A cancelled named model `if` never calls `record_totals` either --
/// `meta.completed_at` stays unset on the `Cancelled` return path
/// (`condition_model::evaluate_model_condition`'s own doc comment), so
/// `execute_conditional`'s own `completed_at_is_truthy` guard must see
/// `false` and skip the call.
#[tokio::test]
async fn a_cancelled_named_model_if_never_calls_record_totals() {
    let token = CancellationToken::new();
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![model_if(
            root_path().push_name("gate"),
            Some("gate"),
            "ok?",
            OnError::Fail,
        )],
    );
    let store = Store::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    ctx.default_adapter = Rc::new(ForeverAdapter);
    let inner = RecordingObserver::new();
    let observer = CancelOnStart {
        inner,
        token: &token,
        target: "prime.gate",
    };
    let result = electricity_vm::execute_root(
        &support::program(root),
        &store,
        &Value::None,
        &ctx,
        &observer,
        &token,
    )
    .await;
    assert!(
        matches!(result, Err(VmError::Cancelled)),
        "expected Cancelled, got {result:?}"
    );
    assert!(
        !observer
            .inner
            .events()
            .iter()
            .any(|e| matches!(e, Event::Totals(_))),
        "a cancelled model if must never call record_totals"
    );
}

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

async fn run(root_op: electricity_bytecode::Op) -> (Store, RecordingObserver, Result<(), VmError>) {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();
    let result = electricity_vm::execute_root(
        &support::program(root_op),
        &store,
        &Value::None,
        &ctx,
        &observer,
        &token,
    )
    .await;
    (store, observer, result)
}

#[tokio::test]
async fn a_named_if_writes_its_own_node_and_fires_balanced_hooks() {
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![cel_if(
            root_path().push_name("gate"),
            Some("gate"),
            "true",
            OnError::Fail,
            vec![],
            None,
        )],
    );
    let (store, observer, result) = run(root).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let gate = as_dict(get(prime, "gate").unwrap());
    let value = as_dict(get(gate, "value").unwrap());
    assert_eq!(get(value, "result"), Some(&Value::Bool(true)));
    assert_eq!(get(value, "branch"), Some(&Value::Str("then".to_string())));
    assert_eq!(get(value, "effects"), Some(&Value::List(vec![])));

    let meta = as_dict(get(gate, "meta").unwrap());
    let meta_keys: Vec<&str> = meta
        .keys()
        .map(|k| match k {
            Value::Str(s) => s.as_str(),
            _ => panic!("expected string keys"),
        })
        .collect();
    assert_eq!(
        meta_keys,
        vec![
            "created_at",
            "completed_at",
            "error",
            "mode",
            "threshold",
            "labels",
            "condition_result",
            "branch",
        ]
    );
    assert_eq!(get(meta, "condition_result"), Some(&Value::Bool(true)));
    assert_eq!(get(meta, "branch"), Some(&Value::Str("then".to_string())));
    assert_eq!(get(meta, "labels"), Some(&Value::None));

    let events = observer.events();
    assert!(events.contains(&Event::Start("prime.gate".to_string())));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Complete(path, None) if path == "prime.gate"))
    );
}

#[tokio::test]
async fn an_unnamed_if_is_transparent_and_fires_no_hooks() {
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![cel_if(
            root_path(),
            None,
            "true",
            OnError::Fail,
            vec![tool_leaf(root_path().push_name("inner"), "inner")],
            None,
        )],
    );
    let (store, observer, result) = run(root).await;
    let err = result.unwrap_err();
    // Issue #442's first item, closed by #537: a `Raised` branch failure
    // now keeps its own class through an `if`'s wrap (the branch loop's
    // own bare-name wrap, `wrap_effect_error`), so this is a `Raised`,
    // not a bare `Message`, even though nothing here ever reads its
    // class directly -- only a tree combine's own numbered summary does.
    let VmError::Raised { message: text, .. } = &err else {
        panic!("expected a Raised error, got {err:?}");
    };
    // The branch loop's own bare-name wrap ("inner: ...") happens first,
    // inside the transparent `if`; the *enclosing* chain then wraps
    // that again with its own effect_path -- which, since the `if`
    // contributes no path segment of its own, is just "prime" (the
    // chain's own name) -- exactly as Python's `_effect_path` falls
    // back to `container_name` for an unnamed effect.
    assert_eq!(text, &format!("prime: inner: {TOOL_FAIL_TEXT}"));

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    // No node for the transparent `if` itself -- but `inner` is a real
    // tool node now, created (and left behind, failed) before
    // `execute_tool` ever reports the failure back up.
    let prime_keys: Vec<&str> = prime
        .keys()
        .map(|k| match k {
            Value::Str(s) => s.as_str(),
            _ => panic!("expected string keys"),
        })
        .collect();
    assert_eq!(prime_keys, vec!["value", "meta", "inner"]);

    let events = observer.events();
    // The transparent `if` itself contributes no hook of its own, but
    // `inner` (a real, named tool effect) fires its own balanced start/
    // write/complete/write around the one retry attempt it gets --
    // nested inside prime's own start/.../complete, with prime's own
    // chain-level write (one per step, success or failure alike) after
    // `inner`'s own pair settles.
    assert_eq!(
        events,
        vec![
            Event::Start("prime".to_string()),
            Event::Start("prime.inner".to_string()),
            Event::Write,
            Event::Complete("prime.inner".to_string(), Some(TOOL_FAIL_TEXT.to_string())),
            Event::Write,
            Event::Write,
            Event::Complete(
                "prime".to_string(),
                Some(format!("prime: inner: {TOOL_FAIL_TEXT}"))
            ),
        ]
    );
}

#[tokio::test]
async fn a_refused_branch_inside_a_named_if_still_writes_meta_and_fires_the_end_event() {
    // Ruling R1 on PR #462: `VmError::Refused` is fatal
    // for *control* (never absorbed by `on_error`, never retried) but
    // recorded exactly like an ordinary failure for *recording*
    // purposes -- `value`/`meta.error` (the refusal text, with no name
    // prefix added), `completed_at`, and the `end` event's own error
    // text -- on every container on the path to it, a named `if`
    // included (`VmError::is_fatal`'s own doc comment).
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![cel_if(
            root_path().push_name("gate"),
            Some("gate"),
            "true",
            OnError::Fail,
            vec![refused_use_leaf(
                root_path().push_name("gate").push_name("child"),
                "child",
                OnError::Fail,
            )],
            None,
        )],
    );
    let (store, observer, result) = run(root).await;
    let err = result.expect_err("a refused branch must still fail the run");
    assert!(matches!(err, VmError::Refused { .. }), "got {err:?}");

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let gate = as_dict(get(prime, "gate").unwrap());
    let meta = as_dict(get(gate, "meta").unwrap());
    let error_text = match get(meta, "error") {
        Some(Value::Str(text)) => text.clone(),
        other => panic!("expected a string meta.error, got {other:?}"),
    };
    assert!(
        error_text.contains("use child inline: prime.unsupported is a prompt effect"),
        "meta.error must carry the refusal text with no name prefix: {error_text}"
    );
    assert!(
        matches!(get(meta, "completed_at"), Some(Value::Str(_))),
        "completed_at must be set, not left null: {:?}",
        get(meta, "completed_at")
    );

    let events = observer.events();
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::Complete(path, Some(text))
                if path == "prime.gate" && text.contains("use child inline: prime.unsupported is a prompt effect")
        )),
        "the end event must report the refusal text: {events:?}"
    );
}

#[tokio::test]
async fn the_else_branch_runs_when_the_condition_is_false() {
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![cel_if(
            root_path().push_name("gate"),
            Some("gate"),
            "false",
            OnError::Fail,
            vec![],
            Some(vec![]),
        )],
    );
    let (store, _observer, result) = run(root).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let gate = as_dict(get(prime, "gate").unwrap());
    let value = as_dict(get(gate, "value").unwrap());
    assert_eq!(get(value, "result"), Some(&Value::Bool(false)));
    assert_eq!(get(value, "branch"), Some(&Value::Str("else".to_string())));
}

#[tokio::test]
async fn an_absent_else_is_an_empty_branch_not_a_skipped_one() {
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![cel_if(
            root_path().push_name("gate"),
            Some("gate"),
            "false",
            OnError::Fail,
            vec![],
            None,
        )],
    );
    let (store, _observer, result) = run(root).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let gate = as_dict(get(prime, "gate").unwrap());
    let value = as_dict(get(gate, "value").unwrap());
    assert_eq!(get(value, "branch"), Some(&Value::Str("else".to_string())));
    assert_eq!(get(value, "effects"), Some(&Value::List(vec![])));
}

#[tokio::test]
async fn on_error_skip_writes_a_null_result_for_a_broken_condition() {
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![cel_if(
            root_path().push_name("gate"),
            Some("gate"),
            // Reading an unset `state.` path under `strict: false` is
            // not itself an error (it is false, by rule) -- a genuinely
            // malformed expression is what `evaluate_condition` fails
            // on.
            "1 +",
            OnError::Skip,
            vec![],
            None,
        )],
    );
    let (store, observer, result) = run(root).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let gate = as_dict(get(prime, "gate").unwrap());
    let value = as_dict(get(gate, "value").unwrap());
    assert_eq!(get(value, "result"), Some(&Value::None));
    assert_eq!(get(value, "branch"), Some(&Value::None));
    assert_eq!(
        get(value, "effects"),
        Some(&Value::Dict(indexmap::IndexMap::new()))
    );

    let meta = as_dict(get(gate, "meta").unwrap());
    assert!(get(meta, "error").unwrap() != &Value::None);
    // Only the first 6 meta keys exist on this path -- `condition_result`/
    // `branch` are never written once the condition itself failed to
    // evaluate.
    assert!(get(meta, "condition_result").is_none());
    assert!(get(meta, "branch").is_none());

    // `cli/events.py::EventLog.on_complete` reads `meta.error`, not
    // whatever `decide_and_run` itself returned -- `on_error: skip`
    // absorbs the condition error into `Ok(())`, but the `end` event
    // must still carry it (`ok: false`, issue #431 review finding on PR
    // #440).
    assert!(
        observer
            .events()
            .iter()
            .any(|e| matches!(e, Event::Complete(path, Some(_)) if path == "prime.gate")),
        "got {:?}",
        observer.events()
    );
}

#[tokio::test]
async fn on_error_continue_takes_the_else_branch_for_a_broken_condition() {
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![cel_if(
            root_path().push_name("gate"),
            Some("gate"),
            "1 +",
            OnError::Continue,
            vec![],
            Some(vec![]),
        )],
    );
    let (store, observer, result) = run(root).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let gate = as_dict(get(prime, "gate").unwrap());
    let value = as_dict(get(gate, "value").unwrap());
    assert_eq!(get(value, "result"), Some(&Value::Bool(false)));
    assert_eq!(get(value, "branch"), Some(&Value::Str("else".to_string())));

    // Same as `on_error: skip`: the condition error persists on
    // `meta.error` even though the branch ran to completion, so the
    // observer's own `end` event still carries it.
    assert!(
        observer
            .events()
            .iter()
            .any(|e| matches!(e, Event::Complete(path, Some(_)) if path == "prime.gate")),
        "got {:?}",
        observer.events()
    );
}

#[tokio::test]
async fn on_error_fail_propagates_a_broken_condition() {
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![cel_if(
            root_path().push_name("gate"),
            Some("gate"),
            "1 +",
            OnError::Fail,
            vec![],
            None,
        )],
    );
    let (_store, _observer, result) = run(root).await;
    result.unwrap_err();
}

#[tokio::test]
async fn a_later_branch_effect_sees_an_earlier_siblings_write_by_bare_name() {
    // `scope_ctx`/`local_writes` (Quirk Q1): inside a branch, a sibling's
    // write is visible under *both* the bare and `prime`-qualified
    // spelling to the *next* sibling -- exercised here through a nested,
    // *named* `dynamic` (always succeeds trivially, empty) as the first
    // branch effect, then a CEL condition (the second) that reads it
    // back by its bare name, proving the overlay rebuild actually ran
    // between the two.
    let first = chain_dynamic(
        root_path().push_name("gate").push_name("first"),
        "first",
        OnError::Fail,
        vec![],
    );
    let second = cel_if(
        root_path().push_name("gate").push_name("second"),
        Some("second"),
        "has(state.first.value)",
        OnError::Fail,
        vec![],
        None,
    );
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![cel_if(
            root_path().push_name("gate"),
            Some("gate"),
            "true",
            OnError::Fail,
            vec![first, second],
            None,
        )],
    );
    let (store, _observer, result) = run(root).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let gate = as_dict(get(prime, "gate").unwrap());
    let second_node = as_dict(get(gate, "second").unwrap());
    let second_value = as_dict(get(second_node, "value").unwrap());
    assert_eq!(get(second_value, "result"), Some(&Value::Bool(true)));
}

#[tokio::test]
async fn an_interrupted_named_if_leaves_meta_error_null_and_reports_ok_to_the_observer() {
    // issue #431 review finding on PR #440: `decide_and_run`'s own
    // branch loop re-raises a cancellation with no `value`/`meta.error`
    // write at all (Python's own outer `except Exception`, never
    // `BaseException`) -- the `end` event must read that back as `ok:
    // true`, never the interrupt text.
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = ToolRegistry::new();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = CancelOnStart {
        inner: RecordingObserver::new(),
        token: &token,
        target: "prime.gate",
    };

    // `x`'s own tree-flow dispatch checks the token before launching
    // each branch unconditionally (unlike a chain with no steps) -- the
    // one reliable way to make a branch notice a cancellation that only
    // lands once the `if` itself has already started.
    let branch = tree_dynamic(
        root_path().push_name("gate").push_name("x"),
        "x",
        OnError::Fail,
        None,
        false,
        vec![chain_dynamic(
            root_path().push_name("gate").push_name("x").push_name("y"),
            "y",
            OnError::Fail,
            vec![],
        )],
    );
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![cel_if(
            root_path().push_name("gate"),
            Some("gate"),
            "true",
            OnError::Fail,
            vec![branch],
            None,
        )],
    );

    let result = electricity_vm::execute_root(
        &support::program(root),
        &store,
        &Value::None,
        &ctx,
        &observer,
        &token,
    )
    .await;
    assert!(matches!(result, Err(VmError::Cancelled)), "got {result:?}");

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let gate = as_dict(get(prime, "gate").unwrap());
    let meta = as_dict(get(gate, "meta").unwrap());
    assert_eq!(get(meta, "error"), Some(&Value::None));

    let events = observer.inner.events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Complete(path, None) if path == "prime.gate")),
        "got {events:?}"
    );
}

#[tokio::test]
async fn a_branch_effect_shadowing_an_enclosing_name_wins_inside_the_branch() {
    // `core.scope.local_writes`: a name local to the current branch wins
    // inside it even though the *same* name already exists in the
    // enclosing baseline -- here the enclosing `dynamic` "shared" (a
    // sibling of the `if`, already run) and the `if` branch's own
    // nested `dynamic` of the same name must not collide; the branch's
    // own "shared" is what a later branch sibling (the second `if`
    // reading `state.shared.value`) actually sees.
    let shared_outer = chain_dynamic(
        root_path().push_name("shared"),
        "shared",
        OnError::Fail,
        vec![],
    );
    let shared_inner = cel_if(
        root_path().push_name("gate").push_name("shared"),
        Some("shared"),
        "true",
        OnError::Fail,
        vec![],
        None,
    );
    let sees_the_branchs_own_shared = cel_if(
        root_path().push_name("gate").push_name("second"),
        Some("second"),
        // The branch's own "shared" is a *conditional* node (`value.
        // result`), not the enclosing dynamic's (`value` itself a
        // bool) -- only resolvable at all if shadowing actually won.
        "has(state.shared.value.result)",
        OnError::Fail,
        vec![],
        None,
    );
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![
            shared_outer,
            cel_if(
                root_path().push_name("gate"),
                Some("gate"),
                "true",
                OnError::Fail,
                vec![shared_inner, sees_the_branchs_own_shared],
                None,
            ),
        ],
    );
    let (store, _observer, result) = run(root).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let gate = as_dict(get(prime, "gate").unwrap());
    let second_node = as_dict(get(gate, "second").unwrap());
    let second_value = as_dict(get(second_node, "value").unwrap());
    assert_eq!(
        get(second_value, "result"),
        Some(&Value::Bool(true)),
        "the branch's own `shared` must shadow the enclosing dynamic's"
    );
}

#[tokio::test]
async fn an_unnamed_ifs_branch_sees_an_earlier_steps_write_through_the_enclosing_dynamics_qualified_path()
 {
    // Issue #431 review, probe A on PR #440's P0 finding: an unnamed
    // `if` nested inside an ordinary `dynamic` (`d`) writes its own
    // first branch step (`first`) directly into `d`'s own node -- the
    // *second* step's condition reads that write back through the
    // fully-qualified `prime.d.first.value` path, which must resolve
    // purely through the live ancestor chain (no `scope_ctx` overlay
    // involved: this is the qualified spelling, not the bare one).
    let first = chain_dynamic(
        root_path().push_name("d").push_name("first"),
        "first",
        OnError::Fail,
        vec![],
    );
    let check = cel_if(
        root_path().push_name("d").push_name("check"),
        Some("check"),
        "has(state.prime.d.first.value)",
        OnError::Fail,
        vec![],
        None,
    );
    let gate = cel_if(
        root_path().push_name("d"),
        None,
        "true",
        OnError::Fail,
        vec![first, check],
        None,
    );
    let d = chain_dynamic(root_path().push_name("d"), "d", OnError::Fail, vec![gate]);
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![d]);
    let (store, _observer, result) = run(root).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let d_node = as_dict(get(prime, "d").unwrap());
    let check_node = as_dict(get(d_node, "check").unwrap());
    let meta = as_dict(get(check_node, "meta").unwrap());
    assert_eq!(
        get(meta, "condition_result"),
        Some(&Value::Bool(true)),
        "`prime.d.first.value` must resolve from inside the if's own branch"
    );
}

#[tokio::test]
async fn a_named_ifs_branch_sees_an_earlier_steps_write_through_its_own_qualified_path() {
    // Probe B: the same shape as the test above, but the gate itself is
    // *named* -- `first`'s write lands in the gate's own node (`prime.
    // gate.first`), not in prime's, and the second step's condition
    // reads it back the same fully-qualified way.
    let first = chain_dynamic(
        root_path().push_name("gate").push_name("first"),
        "first",
        OnError::Fail,
        vec![],
    );
    let check = cel_if(
        root_path().push_name("gate").push_name("check"),
        Some("check"),
        "has(state.prime.gate.first.value)",
        OnError::Fail,
        vec![],
        None,
    );
    let gate = cel_if(
        root_path().push_name("gate"),
        Some("gate"),
        "true",
        OnError::Fail,
        vec![first, check],
        None,
    );
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![gate]);
    let (store, _observer, result) = run(root).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let gate_node = as_dict(get(prime, "gate").unwrap());
    let check_node = as_dict(get(gate_node, "check").unwrap());
    let meta = as_dict(get(check_node, "meta").unwrap());
    assert_eq!(
        get(meta, "condition_result"),
        Some(&Value::Bool(true)),
        "`prime.gate.first.value` must resolve from inside the if's own branch"
    );
}

#[tokio::test]
async fn a_dynamic_created_by_an_ifs_own_first_branch_step_resolves_its_qualified_path() {
    // Probe C (base form, `docs/orchestration-reference.md:244-253`'s
    // own pipeline shape, wrapped in an unnamed gate): `pipeline` is
    // the gate's *only* (so: first) branch step, so it runs against the
    // gate's own unmodified starting chain -- still fully live, all the
    // way up -- and its own second step's condition can read back the
    // *first* step's write through `prime.pipeline.outline.value`, a
    // path that only exists once `pipeline` itself has started.
    let outline = chain_dynamic(
        root_path().push_name("pipeline").push_name("outline"),
        "outline",
        OnError::Fail,
        vec![],
    );
    let check = cel_if(
        root_path().push_name("pipeline").push_name("check"),
        Some("check"),
        "has(state.prime.pipeline.outline.value)",
        OnError::Fail,
        vec![],
        None,
    );
    let pipeline = chain_dynamic(
        root_path().push_name("pipeline"),
        "pipeline",
        OnError::Fail,
        vec![outline, check],
    );
    let gate = cel_if(
        root_path(),
        None,
        "true",
        OnError::Fail,
        vec![pipeline],
        None,
    );
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![gate]);
    let (store, _observer, result) = run(root).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let pipeline_node = as_dict(get(prime, "pipeline").unwrap());
    let check_node = as_dict(get(pipeline_node, "check").unwrap());
    let meta = as_dict(get(check_node, "meta").unwrap());
    assert_eq!(
        get(meta, "condition_result"),
        Some(&Value::Bool(true)),
        "`prime.pipeline.outline.value` must resolve when `pipeline` is the branch's first step"
    );
}

#[tokio::test]
async fn a_dynamic_created_by_an_ifs_own_second_branch_step_cannot_resolve_its_qualified_path() {
    // Probe C's own documented wrinkle: the exact same `pipeline` as
    // above, but now the gate's *second* branch step (preceded by
    // `warmup`) -- Python's own `scope_ctx` rebuild after `warmup`
    // snapshots `prime` *before* `pipeline` exists, so `pipeline`'s own
    // `ctx_override` carries a `prime` that can never gain a `pipeline`
    // key, no matter what `pipeline` itself goes on to write. Circuitry
    // takes the same branch cof does here: this is not a bug to fix,
    // it is the documented shape this crate reproduces exactly.
    let warmup = chain_dynamic(
        root_path().push_name("warmup"),
        "warmup",
        OnError::Fail,
        vec![],
    );
    let outline = chain_dynamic(
        root_path().push_name("pipeline").push_name("outline"),
        "outline",
        OnError::Fail,
        vec![],
    );
    let check = cel_if(
        root_path().push_name("pipeline").push_name("check"),
        Some("check"),
        "has(state.prime.pipeline.outline.value)",
        OnError::Fail,
        vec![],
        None,
    );
    let pipeline = chain_dynamic(
        root_path().push_name("pipeline"),
        "pipeline",
        OnError::Fail,
        vec![outline, check],
    );
    let gate = cel_if(
        root_path(),
        None,
        "true",
        OnError::Fail,
        vec![warmup, pipeline],
        None,
    );
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![gate]);
    let (store, _observer, result) = run(root).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let pipeline_node = as_dict(get(prime, "pipeline").unwrap());
    let check_node = as_dict(get(pipeline_node, "check").unwrap());
    let meta = as_dict(get(check_node, "meta").unwrap());
    assert_eq!(
        get(meta, "condition_result"),
        Some(&Value::Bool(false)),
        "as in cof: `prime.pipeline` is frozen absent by the overlay rebuilt after `warmup`"
    );
}

#[tokio::test]
async fn an_empty_first_step_leaves_the_next_step_the_received_live_chain() {
    // `core/scope.py::scope_ctx`'s own early return (`if not local:
    // return ctx`): the gate's branch starts with an unnamed inner `if`
    // whose condition is false and has no `else`, so it writes nothing
    // at all -- `local` is empty once it finishes. Python hands the
    // *next* step `ctx` itself, still live; this crate must not swap in
    // an overlay built from that empty `local` instead, since that
    // overlay's own cloned `prime` snapshot predates `pipeline` (the
    // next step) ever existing, and can never gain it. `pipeline`'s own
    // second step reads `prime.pipeline.outline.value` -- a path that
    // only exists once `pipeline` itself (this branch's *second* step)
    // has started -- exactly as in `a_dynamic_created_by_an_ifs_own_
    // first_branch_step_resolves_its_qualified_path` above, but with an
    // empty-write step ahead of `pipeline` rather than `pipeline` being
    // the branch's literal first step.
    let inner_if = cel_if(root_path(), None, "false", OnError::Fail, vec![], None);
    let outline = chain_dynamic(
        root_path().push_name("pipeline").push_name("outline"),
        "outline",
        OnError::Fail,
        vec![],
    );
    let check = cel_if(
        root_path().push_name("pipeline").push_name("check"),
        Some("check"),
        "has(state.prime.pipeline.outline.value)",
        OnError::Fail,
        vec![],
        None,
    );
    let pipeline = chain_dynamic(
        root_path().push_name("pipeline"),
        "pipeline",
        OnError::Fail,
        vec![outline, check],
    );
    let gate = cel_if(
        root_path(),
        None,
        "true",
        OnError::Fail,
        vec![inner_if, pipeline],
        None,
    );
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![gate]);
    let (store, _observer, result) = run(root).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let pipeline_node = as_dict(get(prime, "pipeline").unwrap());
    let check_node = as_dict(get(pipeline_node, "check").unwrap());
    let meta = as_dict(get(check_node, "meta").unwrap());
    assert_eq!(
        get(meta, "condition_result"),
        Some(&Value::Bool(true)),
        "an empty-write step must not freeze `prime` before `pipeline` exists"
    );
}

#[tokio::test]
async fn a_shadowing_steps_bare_name_is_invisible_without_the_branchs_own_names() {
    // Probe D: unlike the shadowing test above (which uses a *named*
    // gate, so the enclosing baseline never actually contains `shared`
    // to begin with), this one puts the shadowed `dynamic` directly in
    // `prime`'s own chain, ahead of an *unnamed* gate -- so the gate's
    // own branch_parent (prime's own node) already carries a `shared`
    // key in its baseline before the branch starts. Only `own_names`
    // forcing `shared` to count as local, despite already being in
    // that baseline, makes the second step's *bare* `state.shared...`
    // read the branch's own conditional node rather than nothing at
    // all -- dropping `own_names` from `local_writes` makes this false.
    let shared_outer = chain_dynamic(
        root_path().push_name("shared"),
        "shared",
        OnError::Fail,
        vec![],
    );
    let shared_inner = cel_if(
        root_path().push_name("shared"),
        Some("shared"),
        "true",
        OnError::Fail,
        vec![],
        None,
    );
    let second = cel_if(
        root_path().push_name("second"),
        Some("second"),
        "has(state.shared.value.result)",
        OnError::Fail,
        vec![],
        None,
    );
    let gate = cel_if(
        root_path(),
        None,
        "true",
        OnError::Fail,
        vec![shared_inner, second],
        None,
    );
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![shared_outer, gate],
    );
    let (store, _observer, result) = run(root).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let second_node = as_dict(get(prime, "second").unwrap());
    let meta = as_dict(get(second_node, "meta").unwrap());
    assert_eq!(
        get(meta, "condition_result"),
        Some(&Value::Bool(true)),
        "`own_names` must make the branch's own `shared` visible by bare name"
    );
}

#[tokio::test]
async fn an_ifs_second_branch_step_sees_a_sibling_two_dynamic_levels_under_its_own_sibling() {
    // Probe E (a second-review follow-up on PR
    // #440's F2 fix, `a4ff2cd`): `process` (a named `dynamic`) runs an
    // unnamed `if` whose branch is `[prep, pipeline]`. `pipeline`'s own
    // `outline` step writes two `dynamic` levels under `process` --
    // reachable, from `check` (`pipeline`'s *second* step), only through
    // `state.prime.process.pipeline.outline.value`. `a4ff2cd`'s overlay
    // was a `CtxSource::Frozen(Value)` -- a deep copy of `process`'s own
    // subtree taken the moment `prep` finished, *before* `pipeline` (and
    // so `outline`) existed -- so `check` could never see it: an
    // ancestor the branch itself didn't directly write stayed frozen
    // one level below where the branch's own direct write landed. The
    // overlay now has to stay a live `NodeRef` (`CtxSource::Live`),
    // sharing `process`'s own slot by `Rc`, not a snapshot of it.
    let outline = chain_dynamic(
        root_path()
            .push_name("process")
            .push_name("pipeline")
            .push_name("outline"),
        "outline",
        OnError::Fail,
        vec![],
    );
    let check = cel_if(
        root_path()
            .push_name("process")
            .push_name("pipeline")
            .push_name("check"),
        Some("check"),
        "has(state.prime.process.pipeline.outline.value)",
        OnError::Fail,
        vec![],
        None,
    );
    let pipeline = chain_dynamic(
        root_path().push_name("process").push_name("pipeline"),
        "pipeline",
        OnError::Fail,
        vec![outline, check],
    );
    let prep = chain_dynamic(
        root_path().push_name("process").push_name("prep"),
        "prep",
        OnError::Fail,
        vec![],
    );
    let gate = cel_if(
        root_path().push_name("process"),
        None,
        "true",
        OnError::Fail,
        vec![prep, pipeline],
        None,
    );
    let process = chain_dynamic(
        root_path().push_name("process"),
        "process",
        OnError::Fail,
        vec![gate],
    );
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![process]);
    let (store, _observer, result) = run(root).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let process_node = as_dict(get(prime, "process").unwrap());
    let pipeline_node = as_dict(get(process_node, "pipeline").unwrap());
    let check_node = as_dict(get(pipeline_node, "check").unwrap());
    let meta = as_dict(get(check_node, "meta").unwrap());
    assert_eq!(
        get(meta, "condition_result"),
        Some(&Value::Bool(true)),
        "`prime.process.pipeline.outline.value` must resolve through the live overlay"
    );
}

#[tokio::test]
async fn a_tool_params_template_resolves_the_same_sibling_path_as_probe_e() {
    // Same shape as the probe-E test above, but through a Mustache
    // template param on a real `tool` leaf rather than a CEL `if`'s own
    // condition -- the review's own concern: "a template `{{prime.
    // process.pipeline.outline.value}}` in a tool param silently
    // renders ''. That is a realistic document." A missing path here
    // renders empty, and `json`'s own `parse` mode fails on an empty
    // `input:` (`"json: parse failed: ..."`). `outline`'s own `value`
    // is `json: stringify`'s own output for `1` -- the *string* `"1"`
    // (not its JSON-quoted form: `json: stringify`'s own `value` is
    // already a plain string) -- which renders, bare, as the text `1`;
    // Python's own `str()` on a *bool* would render the capitalized
    // `True`/`False`, not `parse`'s own lowercase JSON spelling, so an
    // ordinary dynamic's own `value: true` can't tell "resolved" apart
    // from "silently rendered empty" here the way this string can.
    let outline = {
        let mut params = indexmap::IndexMap::new();
        params.insert(
            Value::Str("mode".to_string()),
            ParamNode::Literal(Value::Str("stringify".to_string())),
        );
        params.insert(
            Value::Str("input".to_string()),
            ParamNode::Literal(1i64.into()),
        );
        json_tool_leaf(
            root_path()
                .push_name("process")
                .push_name("pipeline")
                .push_name("outline"),
            "outline",
            params,
        )
    };
    let mut params = indexmap::IndexMap::new();
    params.insert(
        Value::Str("mode".to_string()),
        ParamNode::Literal(Value::Str("parse".to_string())),
    );
    params.insert(
        Value::Str("input".to_string()),
        ParamNode::Template(TemplateText::new(
            "{{prime.process.pipeline.outline.value}}".to_string(),
            true,
            Escape::Html,
        )),
    );
    let render = json_tool_leaf(
        root_path()
            .push_name("process")
            .push_name("pipeline")
            .push_name("render"),
        "render",
        params,
    );
    let pipeline = chain_dynamic(
        root_path().push_name("process").push_name("pipeline"),
        "pipeline",
        OnError::Fail,
        vec![outline, render],
    );
    let prep = chain_dynamic(
        root_path().push_name("process").push_name("prep"),
        "prep",
        OnError::Fail,
        vec![],
    );
    let gate = cel_if(
        root_path().push_name("process"),
        None,
        "true",
        OnError::Fail,
        vec![prep, pipeline],
        None,
    );
    let process = chain_dynamic(
        root_path().push_name("process"),
        "process",
        OnError::Fail,
        vec![gate],
    );
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![process]);

    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();
    electricity_vm::execute_root(
        &support::program(root),
        &store,
        &Value::None,
        &ctx,
        &observer,
        &token,
    )
    .await
    .unwrap();

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let process_node = as_dict(get(prime, "process").unwrap());
    let pipeline_node = as_dict(get(process_node, "pipeline").unwrap());
    let render_node = as_dict(get(pipeline_node, "render").unwrap());
    assert_eq!(
        get(render_node, "value"),
        Some(&Value::from(1i64)),
        "the template must resolve `outline`'s write, not silently render empty"
    );
}

#[tokio::test]
async fn a_model_mode_if_reaches_condition_models_real_dispatch() {
    // `support::run_ctx`'s own default adapter is `NoopAdapter`, so a
    // model `if`'s own dispatch always fails the same way a real run's
    // own unconfigured adapter would -- this probes that `Condition::
    // Model` reaches `condition_model::evaluate_model_condition` for
    // real (issue #513), never `unreachable!()`/a stub error.
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![model_if(
            root_path().push_name("gate"),
            Some("gate"),
            "ok?",
            OnError::Fail,
        )],
    );
    let (_, _, result) = run(root).await;
    let err = result.unwrap_err();
    // Wrapped by the enclosing chain's own `wrap_effect_error`, which
    // prefixes the failed child's own name -- same convention as every
    // other effect failure inside a `dynamic` chain.
    let VmError::Message(message) = err else {
        panic!("expected a chain wrap around condition_model's own error, got {err:?}")
    };
    assert_eq!(
        message,
        "prime.gate: _noop adapter: no real adapter is configured for this run"
    );
}

#[tokio::test]
async fn a_disabled_branch_step_writes_a_skip_node_and_its_own_effect_record_is_marked() {
    // Issue #518 (lane J2): `ConditionalRuntime._effect_record` --
    // `{index, type, name, disabled: true}` for a disabled branch step,
    // and the step's own node is a skip node (never real `json` tool
    // dispatch -- `x` would fail with `TOOL_FAIL_TEXT` otherwise).
    let disabled_x = electricity_bytecode::Op {
        enabled: false,
        ..tool_leaf(root_path().push_name("gate").push_name("x"), "x")
    };
    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![cel_if(
            root_path().push_name("gate"),
            Some("gate"),
            "true",
            OnError::Fail,
            vec![disabled_x],
            None,
        )],
    );
    let (store, _, result) = run(root).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    let gate = as_dict(get(prime, "gate").unwrap());
    let value = as_dict(get(gate, "value").unwrap());
    let Value::List(effects) = get(value, "effects").unwrap() else {
        panic!("expected a list")
    };
    assert_eq!(effects.len(), 1);
    let record = as_dict(&effects[0]);
    assert_eq!(get(record, "index"), Some(&Value::from(0i64)));
    assert_eq!(
        get(record, "type"),
        Some(&Value::Str("ToolDefinition".to_string()))
    );
    assert_eq!(get(record, "name"), Some(&Value::Str("x".to_string())));
    assert_eq!(get(record, "disabled"), Some(&Value::Bool(true)));

    let x = as_dict(get(gate, "x").unwrap());
    assert_eq!(get(x, "value"), Some(&Value::None));
    let x_meta = as_dict(get(x, "meta").unwrap());
    assert_eq!(get(x_meta, "disabled"), Some(&Value::Bool(true)));
}
