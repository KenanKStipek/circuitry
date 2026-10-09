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
    Event, RecordingObserver, TOOL_STUB_TEXT, chain_dynamic, dynamic_with_finally, run_ctx,
    tool_leaf, tree_dynamic,
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
    let registry = ToolRegistry::new();
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
    assert_eq!(text, format!("prime.b: {TOOL_STUB_TEXT}"));

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
    let registry = ToolRegistry::new();
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
    assert_eq!(
        get(meta, "error"),
        Some(&Value::Str(format!("prime.d.b: {TOOL_STUB_TEXT}")))
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
    let registry = ToolRegistry::new();
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
    let registry = ToolRegistry::new();
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
    assert_eq!(text, format!("prime.body_tool: {TOOL_STUB_TEXT}"));

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    assert!(
        get(prime, "cleanup").is_none(),
        "the finally tool leaf also fails through the stub before writing anything"
    );
    let meta = as_dict(get(prime, "meta").unwrap());
    assert_eq!(
        get(meta, "error"),
        Some(&Value::Str(format!("prime.body_tool: {TOOL_STUB_TEXT}")))
    );
    assert_eq!(
        get(meta, "finally_error"),
        Some(&Value::Str(format!("prime.cleanup: {TOOL_STUB_TEXT}")))
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
    assert!(matches!(err, VmError::Interrupted(text) if text == "Interrupted (Ctrl-C/SIGINT)"));

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
    assert!(matches!(err, VmError::Interrupted(text) if text == "Interrupted (SIGTERM)"));

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
