//! Lane B2: `dynamic` chain/tree, `finally:`, `on_error`, cancellation --
//! container-only tests (issue #431's Lane B section). See
//! `tests/support/mod.rs` for why a `tool` leaf here always fails
//! deterministically.

mod support;

use electricity_bytecode::{EffectPath, OnError};
use electricity_tools::ToolRegistry;
use electricity_value::Value;
use electricity_vm::{CancellationToken, Limiter, Store, VmError};
use support::{
    CancelOnStart, Event, RecordingObserver, TOOL_FAIL_TEXT, cel_if, chain_dynamic,
    dynamic_with_finally, json_registry, run_ctx, tool_leaf, tree_dynamic,
};

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

#[tokio::test]
async fn chain_stops_at_the_first_failure_and_never_runs_the_rest() {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();

    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![
            chain_dynamic(root_path().push_name("a"), "a", OnError::Fail, vec![]),
            tool_leaf(root_path().push_name("b"), "b"),
            chain_dynamic(root_path().push_name("c"), "c", OnError::Fail, vec![]),
        ],
    );

    let err = electricity_vm::execute_root(
        &support::program(root),
        &store,
        &Value::None,
        &ctx,
        &observer,
        &token,
    )
    .await
    .unwrap_err();

    let VmError::Message(text) = err else {
        panic!("expected a wrapped Message error, got {err:?}");
    };
    assert_eq!(text, format!("prime.b: {TOOL_FAIL_TEXT}"));

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    assert!(get(prime, "a").is_some(), "a must have run before b failed");
    assert!(get(prime, "c").is_none(), "c must never run once b failed");
}

#[tokio::test]
async fn an_absorbed_failure_lets_the_parent_continue_and_records_meta_error() {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();

    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![
            chain_dynamic(
                root_path().push_name("d"),
                "d",
                OnError::Skip,
                vec![tool_leaf(root_path().push_name("d").push_name("b"), "b")],
            ),
            chain_dynamic(
                root_path().push_name("after"),
                "after",
                OnError::Fail,
                vec![],
            ),
        ],
    );

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
    let d = as_dict(get(prime, "d").unwrap());
    assert_eq!(get(d, "value"), Some(&Value::Bool(false)));
    let meta = as_dict(get(d, "meta").unwrap());
    // `d`'s own chain wraps with `d`'s own name plus the child's, never
    // the child's full state path (`core/dynamic.py::_effect_path`:
    // `prefix = container_name if container_name is not None else self.
    // defn.name` -- `self.defn.name` here is `d`, not `d`'s own full
    // path `prime.d`).
    assert_eq!(
        get(meta, "error"),
        Some(&Value::Str(format!("d.b: {TOOL_FAIL_TEXT}")))
    );
    assert!(
        get(prime, "after").is_some(),
        "the parent must continue past an absorbed (on_error: skip) failure"
    );
}

#[tokio::test]
async fn dynamic_meta_key_order_matches_circuitry() {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = ToolRegistry::new();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();

    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![]);
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
    let node_keys: Vec<&str> = prime
        .keys()
        .map(|k| match k {
            Value::Str(s) => s.as_str(),
            _ => panic!("expected string keys"),
        })
        .collect();
    assert_eq!(node_keys, vec!["value", "meta"]);

    let meta = as_dict(get(prime, "meta").unwrap());
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
            "adapter",
            "model",
            "tokens_sent",
            "tokens_received",
            "error",
            "flow",
            "dry_run",
            "labels",
        ]
    );
    assert_eq!(get(meta, "flow"), Some(&Value::Str("chain".to_string())));
    assert_eq!(get(meta, "adapter"), Some(&Value::Str("_noop".to_string())));
    assert_eq!(get(meta, "labels"), Some(&Value::None));
}

#[tokio::test]
async fn tree_dispatch_fires_before_any_branch_and_merges_every_child_in_order() {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = ToolRegistry::new();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();

    let root = tree_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        None,
        false,
        vec![
            chain_dynamic(root_path().push_name("x0"), "x0", OnError::Fail, vec![]),
            chain_dynamic(root_path().push_name("x1"), "x1", OnError::Fail, vec![]),
            chain_dynamic(root_path().push_name("x2"), "x2", OnError::Fail, vec![]),
        ],
    );

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
    let keys: Vec<&str> = prime
        .keys()
        .map(|k| match k {
            Value::Str(s) => s.as_str(),
            _ => panic!("expected string keys"),
        })
        .collect();
    assert_eq!(keys, vec!["value", "meta", "x0", "x1", "x2"]);

    let events = observer.events();
    let dispatch_index = events
        .iter()
        .position(|e| matches!(e, Event::Dispatch(path, _, _) if path == "prime"))
        .expect("dispatch must fire");
    assert_eq!(
        events[dispatch_index],
        Event::Dispatch("prime".to_string(), 3, 3)
    );
    let first_branch_start = events
        .iter()
        .position(|e| matches!(e, Event::Start(path) if path == "prime.x0"));
    assert!(
        first_branch_start.is_some() && first_branch_start.unwrap() > dispatch_index,
        "dispatch must fire before any branch starts"
    );
}

#[tokio::test]
async fn tree_stop_on_error_never_starts_a_queued_branch() {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();

    // max_concurrency: 1 makes this deterministic -- branches launch one
    // at a time, so the first (failing) branch's own stop_on_error must
    // keep the second and third from ever starting.
    let root = tree_dynamic(
        root_path(),
        "prime",
        OnError::Skip,
        Some(1),
        true,
        vec![
            chain_dynamic(
                root_path().push_name("x0"),
                "x0",
                OnError::Fail,
                vec![tool_leaf(root_path().push_name("x0").push_name("b"), "b")],
            ),
            chain_dynamic(root_path().push_name("x1"), "x1", OnError::Fail, vec![]),
            chain_dynamic(root_path().push_name("x2"), "x2", OnError::Fail, vec![]),
        ],
    );

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
    assert!(get(prime, "x0").is_some(), "x0 ran (and failed)");
    assert!(get(prime, "x1").is_none(), "x1 must never start");
    assert!(get(prime, "x2").is_none(), "x2 must never start");
}

#[tokio::test]
async fn finally_runs_after_a_body_failure_and_reports_finally_error_too() {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();

    let root = dynamic_with_finally(
        root_path(),
        "prime",
        OnError::Fail,
        vec![tool_leaf(root_path().push_name("body_tool"), "body_tool")],
        vec![tool_leaf(root_path().push_name("cleanup"), "cleanup")],
    );

    let err = electricity_vm::execute_root(
        &support::program(root),
        &store,
        &Value::None,
        &ctx,
        &observer,
        &token,
    )
    .await
    .unwrap_err();

    let VmError::Message(text) = err else {
        panic!("expected a wrapped Message error, got {err:?}");
    };
    assert_eq!(text, format!("prime.body_tool: {TOOL_FAIL_TEXT}"));

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    // `cleanup` runs (a real tool node now, unlike the old stub) and
    // fails too -- its own `value` stays `None`, the default
    // `execute_tool` sets before dispatch (`on_error: fail` never
    // writes a null placeholder the way `skip`/`continue` would).
    let cleanup = as_dict(get(prime, "cleanup").unwrap());
    assert_eq!(get(cleanup, "value"), Some(&Value::None));
    let meta = as_dict(get(prime, "meta").unwrap());
    assert_eq!(
        get(meta, "error"),
        Some(&Value::Str(format!("prime.body_tool: {TOOL_FAIL_TEXT}")))
    );
    assert_eq!(
        get(meta, "finally_error"),
        Some(&Value::Str(format!("prime.cleanup: {TOOL_FAIL_TEXT}")))
    );
}

#[tokio::test]
async fn an_already_cancelled_token_stops_the_chain_before_its_first_effect() {
    let store = Store::new();
    let token = CancellationToken::new();
    token.request(2); // SIGINT
    let registry = ToolRegistry::new();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();

    let root = chain_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        vec![chain_dynamic(
            root_path().push_name("a"),
            "a",
            OnError::Fail,
            vec![],
        )],
    );

    let err = electricity_vm::execute_root(
        &support::program(root),
        &store,
        &Value::None,
        &ctx,
        &observer,
        &token,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, VmError::Cancelled));

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    assert!(
        get(prime, "a").is_none(),
        "a must never start once cancelled"
    );
    let meta = as_dict(get(prime, "meta").unwrap());
    assert_eq!(
        get(meta, "error"),
        Some(&Value::Str("Interrupted (Ctrl-C/SIGINT)".to_string()))
    );
}

#[tokio::test]
async fn finally_still_runs_when_the_token_is_already_cancelled() {
    let store = Store::new();
    let token = CancellationToken::new();
    token.request(15); // SIGTERM
    let registry = ToolRegistry::new();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();

    let root = dynamic_with_finally(
        root_path(),
        "prime",
        OnError::Fail,
        vec![chain_dynamic(
            root_path().push_name("a"),
            "a",
            OnError::Fail,
            vec![],
        )],
        vec![chain_dynamic(
            root_path().push_name("cleanup"),
            "cleanup",
            OnError::Fail,
            vec![],
        )],
    );

    let err = electricity_vm::execute_root(
        &support::program(root),
        &store,
        &Value::None,
        &ctx,
        &observer,
        &token,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, VmError::Cancelled));

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    assert!(
        get(prime, "a").is_none(),
        "a must never start once cancelled"
    );
    assert!(
        get(prime, "cleanup").is_some(),
        "finally must still run to completion despite the cancellation"
    );
}

#[tokio::test]
async fn finally_also_runs_after_a_successful_body() {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = ToolRegistry::new();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();

    let root = dynamic_with_finally(
        root_path(),
        "prime",
        OnError::Fail,
        vec![chain_dynamic(
            root_path().push_name("a"),
            "a",
            OnError::Fail,
            vec![],
        )],
        vec![chain_dynamic(
            root_path().push_name("cleanup"),
            "cleanup",
            OnError::Fail,
            vec![],
        )],
    );

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
    assert!(get(prime, "a").is_some());
    assert!(get(prime, "cleanup").is_some());
    assert_eq!(get(prime, "value"), Some(&Value::Bool(true)));
    let meta = as_dict(get(prime, "meta").unwrap());
    assert_eq!(get(meta, "error"), Some(&Value::None));
}

#[tokio::test]
async fn a_signal_during_an_otherwise_successful_finally_is_reported_as_interrupted() {
    // issue #431 review finding on PR #440 (P2, orchestrator ruling:
    // take the suggested fix): `finally:` runs on a fresh, never-
    // cancelled token, so a signal that lands *during* it (after a
    // successful body) would otherwise go unnoticed by this dynamic's
    // own `meta.error` -- Python's own main thread still gets a real
    // `KeyboardInterrupt` raised inside `cleanup()` regardless of what
    // that context suppresses. The observer cancels the *real* token
    // the moment `cleanup` itself starts; `cleanup`'s own body still
    // completes (it runs under a separate, never-cancelled token), but
    // the dynamic as a whole must still come out cancelled.
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = ToolRegistry::new();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = CancelOnStart {
        inner: RecordingObserver::new(),
        token: &token,
        target: "prime.cleanup",
    };

    let root = dynamic_with_finally(
        root_path(),
        "prime",
        OnError::Skip,
        vec![chain_dynamic(
            root_path().push_name("a"),
            "a",
            OnError::Fail,
            vec![],
        )],
        vec![chain_dynamic(
            root_path().push_name("cleanup"),
            "cleanup",
            OnError::Fail,
            vec![],
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
    // `on_error: skip` on `prime` itself must not matter: a cancellation
    // always propagates, the same as a body cancellation would.
    assert!(matches!(result, Err(VmError::Cancelled)), "got {result:?}");

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    assert!(get(prime, "cleanup").is_some(), "cleanup must still run");
    assert_eq!(get(prime, "value"), Some(&Value::Bool(false)));
    let meta = as_dict(get(prime, "meta").unwrap());
    assert_eq!(
        get(meta, "error"),
        Some(&Value::Str("Interrupted (Ctrl-C/SIGINT)".to_string()))
    );
}

#[tokio::test]
async fn two_simultaneous_tree_failures_are_combined_into_one_numbered_message() {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();

    // Unbounded concurrency (no max_concurrency, no stop_on_error): both
    // failing branches start before either one can matter to the other.
    let root = tree_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        None,
        false,
        vec![
            chain_dynamic(
                root_path().push_name("x0"),
                "x0",
                OnError::Fail,
                vec![tool_leaf(root_path().push_name("x0").push_name("b"), "b")],
            ),
            chain_dynamic(
                root_path().push_name("x1"),
                "x1",
                OnError::Fail,
                vec![tool_leaf(root_path().push_name("x1").push_name("b"), "b")],
            ),
        ],
    );

    let err = electricity_vm::execute_root(
        &support::program(root),
        &store,
        &Value::None,
        &ctx,
        &observer,
        &token,
    )
    .await
    .unwrap_err();

    let VmError::Message(text) = err else {
        panic!("expected a Message error, got {err:?}");
    };
    assert!(
        text.starts_with("2 effects failed in parallel:\n"),
        "got {text:?}"
    );
    // Double-wrapped, matching Python exactly: `x0`'s own chain already
    // wraps `b`'s failure with `x0`'s own name (never its full state
    // path) before raising out of `x0.execute()`, and the tree's own
    // `_await_tree_branches` wraps *that* again with `x0`'s own name as
    // seen from `prime`'s own dispatch -- `core/dynamic.py::
    // _effect_path`'s `container_name` is always a bare name, not a
    // path (see `an_absorbed_failure_lets_the_parent_continue_and_
    // records_meta_error`'s own identical fix for one level of this).
    assert!(text.contains(&format!("[1] prime.x0: x0.b: {TOOL_FAIL_TEXT}")));
    assert!(text.contains(&format!("[2] prime.x1: x1.b: {TOOL_FAIL_TEXT}")));

    // Both branches still ran (and failed) and both are merged in.
    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    assert!(get(prime, "x0").is_some());
    assert!(get(prime, "x1").is_some());
}

#[tokio::test]
async fn stop_on_error_records_only_the_first_of_two_simultaneous_failures() {
    // Both branches fail with no real await point in between (the
    // lane-C tool stub resolves on its very first poll), so both are
    // already queued in the same `FuturesUnordered` before the drain
    // loop ever runs -- exactly the scenario where a naive "keep
    // appending until the queue is cleared" drain would record both
    // branches' failures instead of just the triggering one
    // (`core/dynamic.py::DynamicRuntime._await_tree_branches` breaks out
    // of its own loop right after the first, so a later completion's
    // error is never even inspected).
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();

    let root = tree_dynamic(
        root_path(),
        "prime",
        OnError::Skip,
        None,
        true,
        vec![
            chain_dynamic(
                root_path().push_name("x0"),
                "x0",
                OnError::Fail,
                vec![tool_leaf(root_path().push_name("x0").push_name("b"), "b")],
            ),
            chain_dynamic(
                root_path().push_name("x1"),
                "x1",
                OnError::Fail,
                vec![tool_leaf(root_path().push_name("x1").push_name("b"), "b")],
            ),
        ],
    );

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
    let meta = as_dict(get(prime, "meta").unwrap());
    let error = get(meta, "error").unwrap();
    let Value::Str(text) = error else {
        panic!("expected a string meta.error, got {error:?}");
    };
    assert!(
        !text.starts_with("2 effects failed in parallel:"),
        "stop_on_error must record only the triggering failure, got {text:?}"
    );
    assert!(
        text == &format!("prime.x0: x0.b: {TOOL_FAIL_TEXT}")
            || text == &format!("prime.x1: x1.b: {TOOL_FAIL_TEXT}"),
        "got {text:?}"
    );
}

#[tokio::test]
async fn dispatch_reports_branches_and_concurrency_in_the_observers_own_parameter_order() {
    // `max_concurrency: 1` over 3 branches makes `branches` (3) and
    // `concurrency` (1) distinguishable -- `tree_dispatch_fires_before_
    // any_branch_and_merges_every_child_in_order`'s own 3-branches/
    // unbounded-concurrency case can't catch a swapped argument order
    // since both values are 3 there.
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = ToolRegistry::new();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();

    let root = tree_dynamic(
        root_path(),
        "prime",
        OnError::Fail,
        Some(1),
        false,
        vec![
            chain_dynamic(root_path().push_name("x0"), "x0", OnError::Fail, vec![]),
            chain_dynamic(root_path().push_name("x1"), "x1", OnError::Fail, vec![]),
            chain_dynamic(root_path().push_name("x2"), "x2", OnError::Fail, vec![]),
        ],
    );

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

    let events = observer.events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Dispatch(path, branches, concurrency)
                if path == "prime" && *branches == 3 && *concurrency == 1)),
        "expected a dispatch with branches=3, concurrency=1, got {events:?}"
    );
}

#[tokio::test]
async fn a_deeply_nested_dynamics_own_if_sees_an_earlier_siblings_write_through_the_qualified_prime_path()
 {
    // The P0 "frozen ancestor" finding on PR #440's review: `d` (nested
    // two levels under the document root, with no `if`/loop overlay of
    // its own anywhere in between) runs an empty dynamic `first`, then a
    // named `if` whose condition reads that write back through the
    // fully-qualified `prime.d.first.value` path -- the same path shape
    // DESIGN.md §2.4's own Quirk Q1 paragraph promises resolves for an
    // ordinary (non-overlay) `dynamic`, because Python's own `ctx` is
    // never actually a frozen copy, however many `dynamic` levels the
    // override was handed down through.
    let first = chain_dynamic(
        root_path().push_name("d").push_name("first"),
        "first",
        OnError::Fail,
        vec![],
    );
    let second = cel_if(
        root_path().push_name("d").push_name("second"),
        Some("second"),
        "has(state.prime.d.first.value)",
        OnError::Fail,
        vec![],
        None,
    );
    let d = chain_dynamic(
        root_path().push_name("d"),
        "d",
        OnError::Fail,
        vec![first, second],
    );
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![d]);

    let store = Store::new();
    let token = CancellationToken::new();
    let registry = ToolRegistry::new();
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
    let d_node = as_dict(get(prime, "d").unwrap());
    let second_node = as_dict(get(d_node, "second").unwrap());
    let meta = as_dict(get(second_node, "meta").unwrap());
    assert_eq!(
        get(meta, "condition_result"),
        Some(&Value::Bool(true)),
        "`prime.d.first.value` must resolve through the live ancestor chain"
    );
}

#[tokio::test]
async fn on_error_continue_never_absorbs_a_cancelled_leaf() {
    // Orchestrator ruling (cross-lane cancellation seam): lane C's own
    // `execute_tool` returns `VmError::Cancelled` -- the *same* variant
    // this crate's own containers use -- when the token cancels it
    // mid-attempt (blocked on a concurrency slot or a retry backoff),
    // not caught by a chain's own `token.is_set()` pre-check (which
    // only ever runs *before* dispatching a step, not while one is
    // already in flight). The cancel fires on `y`'s own `effect_start`
    // -- *after* `x`'s own chain pre-check already let `y` through, but
    // *before* `y`'s own tree dispatch launches `z` -- so it is `y`'s
    // own per-branch `token.is_set()` check in `execute_tree`'s own
    // `launch` closure that notices it (`z` is queued, never started),
    // not `x`'s. `x`'s own `on_error: continue` must not matter either
    // way -- a `Cancelled` is never an ordinary failure `on_error`
    // degrades, so the run must still stop (issue #431 review finding
    // on PR #440: an earlier version of this test cancelled on `x`'s
    // own start instead, so `x`'s own chain pre-check caught it before
    // `y` ever ran, and the comment describing `y`'s own tree noticing
    // it was wrong).
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = ToolRegistry::new();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = CancelOnStart {
        inner: RecordingObserver::new(),
        token: &token,
        target: "prime.x.y",
    };

    let leaf_like = tree_dynamic(
        root_path().push_name("x").push_name("y"),
        "y",
        OnError::Fail,
        None,
        false,
        vec![chain_dynamic(
            root_path().push_name("x").push_name("y").push_name("z"),
            "z",
            OnError::Fail,
            vec![],
        )],
    );
    let x = chain_dynamic(
        root_path().push_name("x"),
        "x",
        OnError::Continue,
        vec![leaf_like],
    );
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![x]);

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
    assert_eq!(
        get(prime, "value"),
        Some(&Value::Bool(false)),
        "`on_error: continue` must never absorb a cancellation"
    );
}

#[tokio::test]
async fn observer_write_fires_once_per_chain_step_once_per_branch_and_once_after_the_merge() {
    // Finding 4 (P2) on PR #440's second review: nothing exercised
    // `RunObserver::write()` at all -- `core/dynamic.py`'s own
    // `store.on_write` call sites are a chain's own per-step `finally:`
    // (`_execute_chain`), a tree branch's own per-branch `finally:`
    // (`_execute_branch`), and the tree merge loop's own trailing call.
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = ToolRegistry::new();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let observer = RecordingObserver::new();

    let branch = |n: &'static str| {
        chain_dynamic(
            root_path().push_name("y").push_name(n),
            n,
            OnError::Fail,
            vec![],
        )
    };
    let x = chain_dynamic(root_path().push_name("x"), "x", OnError::Fail, vec![]);
    let y = tree_dynamic(
        root_path().push_name("y"),
        "y",
        OnError::Fail,
        None,
        false,
        vec![branch("b0"), branch("b1"), branch("b2")],
    );
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![x, y]);

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

    let writes = observer
        .events()
        .iter()
        .filter(|e| matches!(e, Event::Write))
        .count();
    assert_eq!(
        writes, 6,
        "2 chain steps (x, y) in prime's own body + 3 tree branches + 1 post-merge write"
    );
}
