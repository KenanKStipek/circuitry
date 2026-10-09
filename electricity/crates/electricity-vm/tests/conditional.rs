//! Lane B2: `if`/`conditional` in `mode: cel` -- container-only tests
//! (issue #431's Lane B section).

mod support;

use electricity_bytecode::{EffectPath, OnError};
use electricity_tools::ToolRegistry;
use electricity_value::Value;
use electricity_vm::{CancellationToken, Limiter, Store, VmError};
use support::{
    CancelOnStart, Event, RecordingObserver, TOOL_STUB_TEXT, cel_if, chain_dynamic, run_ctx,
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

async fn run(root_op: electricity_bytecode::Op) -> (Store, RecordingObserver, Result<(), VmError>) {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = ToolRegistry::new();
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
    let VmError::Message(text) = err else {
        panic!("expected a Message error, got {err:?}");
    };
    // The branch loop's own bare-name wrap ("inner: ...") happens first,
    // inside the transparent `if`; the *enclosing* chain then wraps
    // that again with its own effect_path -- which, since the `if`
    // contributes no path segment of its own, is just "prime" (the
    // chain's own name) -- exactly as Python's `_effect_path` falls
    // back to `container_name` for an unnamed effect.
    assert_eq!(text, format!("prime: inner: {TOOL_STUB_TEXT}"));

    let snapshot = store.snapshot(&store.root);
    let root_dict = as_dict(&snapshot);
    let prime = as_dict(get(root_dict, "prime").unwrap());
    // No node for the transparent `if` itself, and no `inner` node
    // either -- the tool stub fails before ever writing to the store.
    let prime_keys: Vec<&str> = prime
        .keys()
        .map(|k| match k {
            Value::Str(s) => s.as_str(),
            _ => panic!("expected string keys"),
        })
        .collect();
    assert_eq!(prime_keys, vec!["value", "meta"]);

    let events = observer.events();
    // Only the enclosing root dynamic's own balanced pair fires -- the
    // transparent `if` contributes no hook of its own (lane C's still-
    // stubbed `execute_tool` never reaches far enough to fire one for
    // `inner` either).
    assert_eq!(
        events,
        vec![
            Event::Start("prime".to_string()),
            Event::Complete(
                "prime".to_string(),
                Some(format!("prime: inner: {TOOL_STUB_TEXT}"))
            ),
        ]
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
