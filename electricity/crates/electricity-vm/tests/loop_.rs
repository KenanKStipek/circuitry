//! Lane H (#461): `loop` effect tests -- each/while, chain/tree, named/
//! unnamed, `collect`/`prev`/`last`, `meta.progress`, `on_error`, and
//! the `LoopBoundsError`/collection-resolution error texts. See
//! `tests/support/mod.rs` for the shared `json`-tool-backed builders
//! this file reuses.

mod support;

use electricity_bytecode::{
    Condition, EffectPath, Escape, ExpectCondition, LeafKind, LoopFlow, LoopId, LoopSpec, NodeKind,
    OnError, Op, ParamNode, Region, RetryPolicy, TemplateText, ToolOp,
};
use electricity_value::Value;
use electricity_vm::observer::InstanceId;
use electricity_vm::{CancellationToken, Limiter, RunObserver, Store, VmError};
use indexmap::IndexMap;
use support::{json_registry, refused_use_leaf, run_ctx};

fn root_path() -> EffectPath {
    EffectPath::root()
}

fn k(s: &str) -> Value {
    Value::Str(s.to_string())
}

fn get<'a>(dict: &'a IndexMap<Value, Value>, key: &str) -> Option<&'a Value> {
    dict.get(&k(key))
}

fn as_dict(value: &Value) -> &IndexMap<Value, Value> {
    match value {
        Value::Dict(d) => d,
        other => panic!("expected a dict, got {other:?}"),
    }
}

fn as_list(value: &Value) -> &Vec<Value> {
    match value {
        Value::List(l) => l,
        other => panic!("expected a list, got {other:?}"),
    }
}

fn as_str(value: &Value) -> &str {
    match value {
        Value::Str(s) => s,
        other => panic!("expected a str, got {other:?}"),
    }
}

/// A `json` tool leaf, `mode: stringify`, whose own `input:` is a
/// `{from: path, default: ...}` reference -- lets a test prove what a
/// body step can actually see in its own rendering context (an earlier
/// sibling's write, `prime.<loop>.prev`, the loop's own `item`/`iter`
/// binding) by checking the stringified JSON this tool's own `value`
/// ends up holding.
fn stringify_from(path: EffectPath, name: &str, from: &str, default: &str) -> Op {
    let mut params = IndexMap::new();
    params.insert(
        k("mode"),
        ParamNode::Literal(Value::Str("stringify".to_string())),
    );
    params.insert(
        k("input"),
        ParamNode::From {
            path: from.to_string(),
            default: Some(Value::Str(default.to_string())),
        },
    );
    tool_op(path, name, params, None, RetryPolicy::default())
}

/// A `json` tool leaf, `mode: parse`, templating `{{item}}` as its own
/// `input:` and gated by *expect* -- fails deterministically for every
/// item *expect* rejects.
fn parse_item_gated(path: EffectPath, name: &str, expect_expr: &str) -> Op {
    let mut params = IndexMap::new();
    params.insert(
        k("mode"),
        ParamNode::Literal(Value::Str("parse".to_string())),
    );
    params.insert(
        k("input"),
        ParamNode::Template(TemplateText::new("{{item}}", false, Escape::None)),
    );
    tool_op(
        path,
        name,
        params,
        Some(ExpectCondition::Cel {
            expr: expect_expr.to_string(),
        }),
        RetryPolicy::default(),
    )
}

/// Like [`parse_item_gated`], but templates *template* (e.g.
/// `"{{iter.index}}"`) rather than hard-coding `{{item}}` -- lets a
/// `while` loop's own body (no `item` binding) fail deterministically
/// on a given pass.
fn parse_template_gated(path: EffectPath, name: &str, template: &str, expect_expr: &str) -> Op {
    let mut params = IndexMap::new();
    params.insert(
        k("mode"),
        ParamNode::Literal(Value::Str("parse".to_string())),
    );
    params.insert(
        k("input"),
        ParamNode::Template(TemplateText::new(template, false, Escape::None)),
    );
    tool_op(
        path,
        name,
        params,
        Some(ExpectCondition::Cel {
            expr: expect_expr.to_string(),
        }),
        RetryPolicy::default(),
    )
}

/// A `json` tool leaf, `mode: parse`, with a fixed malformed `input:` --
/// always fails.
fn failing_tool(path: EffectPath, name: &str) -> Op {
    let mut params = IndexMap::new();
    params.insert(
        k("mode"),
        ParamNode::Literal(Value::Str("parse".to_string())),
    );
    params.insert(
        k("input"),
        ParamNode::Literal(Value::Str("not json".to_string())),
    );
    tool_op(path, name, params, None, RetryPolicy::default())
}

fn tool_op(
    path: EffectPath,
    name: &str,
    params: IndexMap<Value, ParamNode>,
    expect: Option<ExpectCondition>,
    retries: RetryPolicy,
) -> Op {
    Op {
        path,
        name: Some(name.to_string()),
        kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
            provider: "json".to_string(),
            params: ParamNode::Map(params),
            params_json: None,
            prompt: None,
            model: None,
            timeout_ms: None,
            retries,
            expect,
            description: None,
            group: None,
        }))),
        on_error: OnError::Fail,
        labels: None,
        enabled: true,
    }
}

#[allow(clippy::too_many_arguments)]
fn loop_op(
    path: EffectPath,
    name: Option<&str>,
    spec: LoopSpec,
    flow: LoopFlow,
    max_concurrency: Option<u32>,
    max_iterations: Option<u32>,
    min_iterations: u32,
    on_error: OnError,
    collect: Option<&str>,
    body: Vec<Op>,
) -> Op {
    Op {
        path,
        name: name.map(str::to_string),
        kind: NodeKind::Control(Region::Loop {
            name: name.map(str::to_string),
            spec,
            body: Box::new(Region::Block {
                ops: body,
                overlay: true,
            }),
            flow,
            max_concurrency,
            max_iterations,
            min_iterations,
            collect: collect.map(str::to_string),
        }),
        on_error,
        labels: None,
        enabled: true,
    }
}

fn each_spec(in_path: &str, as_name: &str, truncate: bool) -> LoopSpec {
    LoopSpec::Each {
        in_path: in_path.to_string(),
        as_name: as_name.to_string(),
        truncate,
    }
}

fn cel_while(expr: &str) -> LoopSpec {
    LoopSpec::While(Condition::Cel {
        expr: expr.to_string(),
        strict: false,
    })
}

fn seed_items(store: &Store, items: Vec<Value>) {
    store.set_leaf(&store.root, k("items"), Value::List(items));
}

#[derive(Default)]
struct DispatchObserver {
    dispatches: std::cell::RefCell<Vec<(usize, usize)>>,
    paths: std::cell::RefCell<Vec<String>>,
}

impl RunObserver for DispatchObserver {
    fn dispatch(
        &self,
        path: &electricity_bytecode::EffectPath,
        branches: usize,
        concurrency: usize,
    ) {
        self.dispatches.borrow_mut().push((branches, concurrency));
        self.paths.borrow_mut().push(path.to_string());
    }
    fn effect_start(&self, _path: &electricity_bytecode::EffectPath) -> InstanceId {
        0
    }
}

async fn run_with_observer(
    root: Op,
    store: &Store,
    observer: &dyn RunObserver,
) -> Result<(), VmError> {
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    electricity_vm::execute_root(
        &support::program(root),
        store,
        &Value::None,
        &ctx,
        observer,
        &token,
    )
    .await
}

async fn run(root: Op, store: &Store) -> Result<(), VmError> {
    run_with_observer(root, store, &electricity_vm::NullObserver).await
}

fn prime(store: &Store) -> IndexMap<Value, Value> {
    let snapshot = store.snapshot(&store.root);
    let root = as_dict(&snapshot);
    as_dict(get(root, "prime").unwrap()).clone()
}

fn single(op: Op) -> Op {
    support::chain_dynamic(root_path(), "prime", OnError::Fail, vec![op])
}

// ---------------------------------------------------------------------
// Named chain each
// ---------------------------------------------------------------------

#[tokio::test]
async fn named_chain_each_writes_iter_nodes_meta_and_last() {
    let store = Store::new();
    seed_items(
        &store,
        vec![
            Value::from(10_i64),
            Value::from(20_i64),
            Value::from(30_i64),
        ],
    );

    let loop_id = LoopId(0);
    let own_path = root_path().push_name("shots");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![stringify_from(
        body_prefix.push_name("describe"),
        "describe",
        "item",
        "MISSING",
    )];
    let loop_op_ = loop_op(
        own_path,
        Some("shots"),
        each_spec("items", "item", false),
        LoopFlow::Chain,
        None,
        None,
        0,
        OnError::Fail,
        None,
        body,
    );

    run(single(loop_op_), &store).await.unwrap();

    let pr = prime(&store);
    let shots = as_dict(get(&pr, "shots").unwrap());
    let meta = as_dict(get(shots, "meta").unwrap());
    assert_eq!(get(meta, "mode"), Some(&Value::Str("each".to_string())));
    assert_eq!(
        get(meta, "each_in_path"),
        Some(&Value::Str("items".to_string()))
    );
    assert_eq!(get(meta, "each_as"), Some(&Value::Str("item".to_string())));
    assert_eq!(get(meta, "error"), Some(&Value::None));
    let completed = as_list(get(meta, "completed_passes").unwrap());
    assert_eq!(
        completed,
        &vec![Value::from(0_i64), Value::from(1_i64), Value::from(2_i64)]
    );
    assert!(get(meta, "failed_passes").is_none());

    let progress = as_dict(get(meta, "progress").unwrap());
    assert_eq!(get(progress, "done"), Some(&Value::from(3_i64)));
    assert_eq!(get(progress, "total"), Some(&Value::from(3_i64)));

    let value = as_dict(get(shots, "value").unwrap());
    assert_eq!(value.get(&k("iterations")), Some(&Value::from(3_i64)));
    let termination = as_dict(get(value, "termination").unwrap());
    assert_eq!(
        get(termination, "reason"),
        Some(&Value::Str("collection_exhausted".to_string()))
    );
    let effects_by_iteration = as_list(get(value, "effects_by_iteration").unwrap());
    assert_eq!(effects_by_iteration.len(), 3);

    for (idx, expected) in [(0, "10"), (1, "20"), (2, "30")] {
        let iter_node = as_dict(get(shots, &format!("iter_{idx}")).unwrap());
        let describe = as_dict(get(iter_node, "describe").unwrap());
        assert_eq!(
            get(describe, "value"),
            Some(&Value::Str(expected.to_string()))
        );
    }

    // `last` aliases `iter_2` (the final completed pass).
    let last = as_dict(get(shots, "last").unwrap());
    let last_describe = as_dict(get(last, "describe").unwrap());
    assert_eq!(
        get(last_describe, "value"),
        Some(&Value::Str("30".to_string()))
    );
}

#[tokio::test]
async fn named_chain_each_on_error_continue_skips_a_failed_pass_in_collect() {
    let store = Store::new();
    seed_items(
        &store,
        vec![Value::from(1_i64), Value::from(2_i64), Value::from(3_i64)],
    );

    let loop_id = LoopId(0);
    let own_path = root_path().push_name("shots");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![
        stringify_from(body_prefix.push_name("step"), "step", "item", "0"),
        // Only `item == 2` ever satisfies this `expect:` -- passes 0
        // and 2 fail here, after `step` already wrote its own value.
        parse_item_gated(body_prefix.push_name("gate"), "gate", "value == 2"),
    ];
    let loop_op_ = loop_op(
        own_path,
        Some("shots"),
        each_spec("items", "item", false),
        LoopFlow::Chain,
        None,
        None,
        0,
        OnError::Continue,
        Some("step"),
        body,
    );

    run(single(loop_op_), &store).await.unwrap();

    let pr = prime(&store);
    let shots = as_dict(get(&pr, "shots").unwrap());
    let meta = as_dict(get(shots, "meta").unwrap());
    let completed = as_list(get(meta, "completed_passes").unwrap());
    assert_eq!(completed, &vec![Value::from(1_i64)]);
    let failed = as_list(get(meta, "failed_passes").unwrap());
    assert_eq!(failed, &vec![Value::from(0_i64), Value::from(2_i64)]);

    let value = as_dict(get(shots, "value").unwrap());
    assert_eq!(value.get(&k("iterations")), Some(&Value::from(1_i64)));
    let termination = as_dict(get(value, "termination").unwrap());
    assert_eq!(
        get(termination, "reason"),
        Some(&Value::Str("collection_exhausted".to_string()))
    );

    let collected = as_dict(get(shots, "collected").unwrap());
    let values = as_list(get(collected, "value").unwrap());
    assert_eq!(values, &vec![Value::Str("2".to_string())]);
}

#[tokio::test]
async fn named_chain_each_truncate_records_unvisited() {
    let store = Store::new();
    seed_items(&store, (1..=5).map(Value::from).collect());

    let loop_id = LoopId(0);
    let own_path = root_path().push_name("shots");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![stringify_from(
        body_prefix.push_name("step"),
        "step",
        "item",
        "0",
    )];
    let loop_op_ = loop_op(
        own_path,
        Some("shots"),
        each_spec("items", "item", true),
        LoopFlow::Chain,
        None,
        Some(2),
        0,
        OnError::Fail,
        None,
        body,
    );

    run(single(loop_op_), &store).await.unwrap();

    let pr = prime(&store);
    let shots = as_dict(get(&pr, "shots").unwrap());
    let value = as_dict(get(shots, "value").unwrap());
    assert_eq!(value.get(&k("iterations")), Some(&Value::from(2_i64)));
    let termination = as_dict(get(value, "termination").unwrap());
    assert_eq!(
        get(termination, "reason"),
        Some(&Value::Str("max_iterations_reached".to_string()))
    );
    assert_eq!(get(termination, "unvisited"), Some(&Value::from(3_i64)));
    assert!(get(shots, "iter_2").is_none());
}

#[tokio::test]
async fn each_loop_bounds_error_without_truncate_fails_under_on_error_fail() {
    let store = Store::new();
    seed_items(&store, (1..=3).map(Value::from).collect());

    let loop_id = LoopId(0);
    let own_path = root_path().push_name("shots");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![stringify_from(
        body_prefix.push_name("step"),
        "step",
        "item",
        "0",
    )];
    let loop_op_ = loop_op(
        own_path,
        Some("shots"),
        each_spec("items", "item", false),
        LoopFlow::Chain,
        None,
        Some(1),
        0,
        OnError::Fail,
        None,
        body,
    );

    let err = run(single(loop_op_), &store).await.unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("collection has 3 items but max_iterations is 1"),
        "unexpected error text: {text}"
    );
    assert!(
        text.contains("each.truncate: true"),
        "unexpected error text: {text}"
    );
}

#[tokio::test]
async fn each_loop_bounds_error_under_on_error_break_records_but_does_not_raise() {
    let store = Store::new();
    seed_items(&store, (1..=3).map(Value::from).collect());

    let loop_id = LoopId(0);
    let own_path = root_path().push_name("shots");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![stringify_from(
        body_prefix.push_name("step"),
        "step",
        "item",
        "0",
    )];
    let loop_op_ = loop_op(
        own_path,
        Some("shots"),
        each_spec("items", "item", false),
        LoopFlow::Chain,
        None,
        Some(1),
        0,
        OnError::Break,
        None,
        body,
    );

    run(single(loop_op_), &store).await.unwrap();

    let pr = prime(&store);
    let shots = as_dict(get(&pr, "shots").unwrap());
    let meta = as_dict(get(shots, "meta").unwrap());
    let error_text = as_str(get(meta, "error").unwrap());
    assert!(error_text.contains("collection has 3 items but max_iterations is 1"));

    let value = as_dict(get(shots, "value").unwrap());
    assert_eq!(value.get(&k("iterations")), Some(&Value::from(0_i64)));
    let termination = as_dict(get(value, "termination").unwrap());
    assert_eq!(
        get(termination, "reason"),
        Some(&Value::Str("error".to_string()))
    );
    assert!(as_str(get(termination, "detail").unwrap()).contains("max_iterations is 1"));
    assert!(get(shots, "iter_0").is_none());
}

#[tokio::test]
async fn an_unresolved_collection_runs_zero_passes_and_records_each_in_error() {
    let store = Store::new();
    // Nothing seeded at `missing` at all.

    let loop_id = LoopId(0);
    let own_path = root_path().push_name("shots");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![stringify_from(
        body_prefix.push_name("step"),
        "step",
        "item",
        "0",
    )];
    let loop_op_ = loop_op(
        own_path,
        Some("shots"),
        each_spec("missing", "item", false),
        LoopFlow::Chain,
        None,
        None,
        0,
        OnError::Fail,
        None,
        body,
    );

    run(single(loop_op_), &store).await.unwrap();

    let pr = prime(&store);
    let shots = as_dict(get(&pr, "shots").unwrap());
    let meta = as_dict(get(shots, "meta").unwrap());
    let each_in_error = as_str(get(meta, "each_in_error").unwrap());
    assert_eq!(
        each_in_error,
        "path 'missing' resolved to None at segment 'missing'"
    );
    assert!(
        get(meta, "progress").is_none(),
        "no progress is written for an unresolved collection"
    );

    let value = as_dict(get(shots, "value").unwrap());
    assert_eq!(value.get(&k("iterations")), Some(&Value::from(0_i64)));
    let termination = as_dict(get(value, "termination").unwrap());
    assert_eq!(
        get(termination, "reason"),
        Some(&Value::Str("collection_unresolved".to_string()))
    );
}

#[tokio::test]
async fn unnamed_chain_each_writes_directly_into_the_enclosing_node_each_pass() {
    let store = Store::new();
    seed_items(&store, vec![Value::from(1_i64), Value::from(2_i64)]);

    let own_path = root_path();
    let body = vec![stringify_from(own_path.push_name("x"), "x", "item", "0")];
    let loop_op_ = loop_op(
        own_path,
        None,
        each_spec("items", "item", false),
        LoopFlow::Chain,
        None,
        None,
        0,
        OnError::Fail,
        None,
        body,
    );

    run(single(loop_op_), &store).await.unwrap();

    let pr = prime(&store);
    assert!(get(&pr, "shots").is_none());
    assert!(get(&pr, "iter_0").is_none());
    let x = as_dict(get(&pr, "x").unwrap());
    // Each pass overwrites the same `x` node -- only the last item's
    // write survives.
    assert_eq!(get(x, "value"), Some(&Value::Str("2".to_string())));
}

// ---------------------------------------------------------------------
// while (CEL only)
// ---------------------------------------------------------------------

#[tokio::test]
async fn named_while_runs_until_the_condition_is_false() {
    let store = Store::new();
    let loop_id = LoopId(0);
    let own_path = root_path().push_name("counter");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![stringify_from(
        body_prefix.push_name("step"),
        "step",
        "iter.index",
        "0",
    )];
    let loop_op_ = loop_op(
        own_path,
        Some("counter"),
        cel_while("state.iter.count < 3"),
        LoopFlow::Chain,
        None,
        None,
        0,
        OnError::Fail,
        None,
        body,
    );

    run(single(loop_op_), &store).await.unwrap();

    let pr = prime(&store);
    let counter = as_dict(get(&pr, "counter").unwrap());
    let meta = as_dict(get(counter, "meta").unwrap());
    assert_eq!(get(meta, "mode"), Some(&Value::Str("cel".to_string())));
    let value = as_dict(get(counter, "value").unwrap());
    assert_eq!(value.get(&k("iterations")), Some(&Value::from(3_i64)));
    let termination = as_dict(get(value, "termination").unwrap());
    assert_eq!(
        get(termination, "reason"),
        Some(&Value::Str("condition_false".to_string()))
    );
    let progress = as_dict(get(meta, "progress").unwrap());
    assert_eq!(get(progress, "total"), Some(&Value::None));
}

#[tokio::test]
async fn named_while_hitting_max_iterations_is_distinct_from_condition_false() {
    let store = Store::new();
    let loop_id = LoopId(0);
    let own_path = root_path().push_name("counter");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![stringify_from(
        body_prefix.push_name("step"),
        "step",
        "iter.index",
        "0",
    )];
    let loop_op_ = loop_op(
        own_path,
        Some("counter"),
        cel_while("true"),
        LoopFlow::Chain,
        None,
        Some(2),
        0,
        OnError::Fail,
        None,
        body,
    );

    run(single(loop_op_), &store).await.unwrap();

    let pr = prime(&store);
    let counter = as_dict(get(&pr, "counter").unwrap());
    let value = as_dict(get(counter, "value").unwrap());
    assert_eq!(value.get(&k("iterations")), Some(&Value::from(2_i64)));
    let termination = as_dict(get(value, "termination").unwrap());
    assert_eq!(
        get(termination, "reason"),
        Some(&Value::Str("max_iterations_reached".to_string()))
    );
}

#[tokio::test]
async fn named_while_min_iterations_runs_without_consulting_the_condition() {
    let store = Store::new();
    let loop_id = LoopId(0);
    let own_path = root_path().push_name("counter");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![stringify_from(
        body_prefix.push_name("step"),
        "step",
        "iter.index",
        "0",
    )];
    // A condition that is always false -- forced to run twice anyway
    // by `min_iterations`, then stops the moment it's actually
    // consulted.
    let loop_op_ = loop_op(
        own_path,
        Some("counter"),
        cel_while("false"),
        LoopFlow::Chain,
        None,
        None,
        2,
        OnError::Fail,
        None,
        body,
    );

    run(single(loop_op_), &store).await.unwrap();

    let pr = prime(&store);
    let counter = as_dict(get(&pr, "counter").unwrap());
    let value = as_dict(get(counter, "value").unwrap());
    assert_eq!(value.get(&k("iterations")), Some(&Value::from(2_i64)));
    let termination = as_dict(get(value, "termination").unwrap());
    assert_eq!(
        get(termination, "reason"),
        Some(&Value::Str("condition_false".to_string()))
    );
}

/// Review of #466, finding 1 (the condition-error half): a failing
/// *condition* under the default `on_error: fail` had the same bug as
/// a failing body (see `named_while_failing_body_under_on_error_fail_
/// keeps_the_real_iteration_count`) -- the `OnError::Fail` arm of the
/// condition-error match returned before copying the local counter
/// into `RunState`. `min_iterations` forces two real passes before the
/// condition (an absent path under `strict: true`, always an error) is
/// ever consulted, so a correct run must still show `iterations: 2`.
#[tokio::test]
async fn named_while_failing_condition_under_on_error_fail_keeps_the_real_iteration_count() {
    let store = Store::new();
    let loop_id = LoopId(0);
    let own_path = root_path().push_name("w");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![stringify_from(
        body_prefix.push_name("step"),
        "step",
        "iter.index",
        "0",
    )];
    let loop_op_ = loop_op(
        own_path,
        Some("w"),
        LoopSpec::While(Condition::Cel {
            expr: "state.prime.typo.value <= 10".to_string(),
            strict: true,
        }),
        LoopFlow::Chain,
        None,
        None,
        2,
        OnError::Fail,
        None,
        body,
    );

    assert!(run(single(loop_op_), &store).await.is_err());

    let pr = prime(&store);
    let w = as_dict(get(&pr, "w").unwrap());
    let value = as_dict(get(w, "value").unwrap());
    assert_eq!(value.get(&k("iterations")), Some(&Value::from(2_i64)));
    let termination = as_dict(get(value, "termination").unwrap());
    assert_eq!(
        get(termination, "reason"),
        Some(&Value::Str("error".to_string()))
    );
}

/// Review of #466, finding 1: a failing body under the default
/// `on_error: fail` used to leave `state.iteration_count` at its
/// default (0) because the Fail branch returned before the only line
/// that copied the local counter into `RunState`. cof's own
/// `LoopRuntime.execute` records the real count reached in its own
/// `except` block (`core/loop.py`).
#[tokio::test]
async fn named_while_failing_body_under_on_error_fail_keeps_the_real_iteration_count() {
    let store = Store::new();
    let loop_id = LoopId(0);
    let own_path = root_path().push_name("w");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![parse_template_gated(
        body_prefix.push_name("gate"),
        "gate",
        "{{iter.index}}",
        "value < 2",
    )];
    let loop_op_ = loop_op(
        own_path,
        Some("w"),
        cel_while("true"),
        LoopFlow::Chain,
        None,
        Some(5),
        0,
        OnError::Fail,
        None,
        body,
    );

    assert!(run(single(loop_op_), &store).await.is_err());

    let pr = prime(&store);
    let w = as_dict(get(&pr, "w").unwrap());
    let value = as_dict(get(w, "value").unwrap());
    assert_eq!(value.get(&k("iterations")), Some(&Value::from(2_i64)));
    let termination = as_dict(get(value, "termination").unwrap());
    assert_eq!(
        get(termination, "reason"),
        Some(&Value::Str("error".to_string()))
    );
}

/// Review of #466, finding 2: the while-condition's own ctx used to go
/// through `step_overlay` (`scope_ctx`'s own `prime` mirroring), so
/// `prime.<loop>.iter` leaked a `{index, count}` dict that cof's own
/// plain `{**ctx, "iter": ...}` construction never adds. A condition
/// that reads `prime.<loop>.iter` (here, this loop's own `prime.w.iter`)
/// must see it absent.
#[tokio::test]
async fn named_while_condition_ctx_does_not_leak_iter_into_prime() {
    let store = Store::new();
    let loop_id = LoopId(0);
    let own_path = root_path().push_name("w");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![stringify_from(
        body_prefix.push_name("step"),
        "step",
        "iter.index",
        "0",
    )];
    let loop_op_ = loop_op(
        own_path,
        Some("w"),
        cel_while("!has(state.prime.iter) && state.iter.count < 2"),
        LoopFlow::Chain,
        None,
        None,
        0,
        OnError::Fail,
        None,
        body,
    );

    run(single(loop_op_), &store).await.unwrap();

    let pr = prime(&store);
    let w = as_dict(get(&pr, "w").unwrap());
    let value = as_dict(get(w, "value").unwrap());
    assert_eq!(value.get(&k("iterations")), Some(&Value::from(2_i64)));
    let termination = as_dict(get(value, "termination").unwrap());
    assert_eq!(
        get(termination, "reason"),
        Some(&Value::Str("condition_false".to_string()))
    );
}

// ---------------------------------------------------------------------
// tree each
// ---------------------------------------------------------------------

#[tokio::test]
async fn named_tree_each_dispatches_once_and_merges_every_pass_in_order() {
    let store = Store::new();
    seed_items(
        &store,
        vec![
            Value::from(1_i64),
            Value::from(2_i64),
            Value::from(3_i64),
            Value::from(4_i64),
        ],
    );

    let loop_id = LoopId(0);
    let own_path = root_path().push_name("shots");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![stringify_from(
        body_prefix.push_name("step"),
        "step",
        "item",
        "0",
    )];
    let loop_op_ = loop_op(
        own_path,
        Some("shots"),
        each_spec("items", "item", false),
        LoopFlow::Tree,
        Some(2),
        None,
        0,
        OnError::Fail,
        None,
        body,
    );

    let observer = DispatchObserver::default();
    run_with_observer(single(loop_op_), &store, &observer)
        .await
        .unwrap();

    assert_eq!(observer.dispatches.borrow().as_slice(), &[(4, 2)]);

    let pr = prime(&store);
    let shots = as_dict(get(&pr, "shots").unwrap());
    for (idx, expected) in [(0, "1"), (1, "2"), (2, "3"), (3, "4")] {
        let iter_node = as_dict(get(shots, &format!("iter_{idx}")).unwrap());
        let step = as_dict(get(iter_node, "step").unwrap());
        assert_eq!(get(step, "value"), Some(&Value::Str(expected.to_string())));
    }
    // `last_completed` is the highest successful index, not the one
    // that happened to finish last in completion order.
    let last = as_dict(get(shots, "last").unwrap());
    let last_step = as_dict(get(last, "step").unwrap());
    assert_eq!(get(last_step, "value"), Some(&Value::Str("4".to_string())));

    let meta = as_dict(get(shots, "meta").unwrap());
    let completed = as_list(get(meta, "completed_passes").unwrap());
    assert_eq!(
        completed,
        &vec![
            Value::from(0_i64),
            Value::from(1_i64),
            Value::from(2_i64),
            Value::from(3_i64)
        ]
    );
}

#[tokio::test]
async fn named_tree_each_on_error_fail_reports_the_lowest_failing_index() {
    let store = Store::new();
    seed_items(&store, vec![Value::from(1_i64), Value::from(2_i64)]);

    let loop_id = LoopId(0);
    let own_path = root_path().push_name("shots");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![failing_tool(body_prefix.push_name("step"), "step")];
    let loop_op_ = loop_op(
        own_path,
        Some("shots"),
        each_spec("items", "item", false),
        LoopFlow::Tree,
        None,
        None,
        0,
        OnError::Fail,
        None,
        body,
    );

    let err = run(single(loop_op_), &store).await.unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("loop 'shots' iteration [0]:"),
        "unexpected error text: {text}"
    );
}

// ---------------------------------------------------------------------
// prev
// ---------------------------------------------------------------------

#[tokio::test]
async fn prev_is_absent_before_the_first_completed_pass_and_present_after() {
    let store = Store::new();
    seed_items(&store, vec![Value::from(1_i64), Value::from(2_i64)]);

    let loop_id = LoopId(0);
    let own_path = root_path().push_name("shots");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![
        stringify_from(body_prefix.push_name("echo"), "echo", "item", "0"),
        stringify_from(
            body_prefix.push_name("check_prev"),
            "check_prev",
            "prime.shots.prev.echo.value",
            "ABSENT",
        ),
    ];
    let loop_op_ = loop_op(
        own_path,
        Some("shots"),
        each_spec("items", "item", false),
        LoopFlow::Chain,
        None,
        None,
        0,
        OnError::Fail,
        None,
        body,
    );

    run(single(loop_op_), &store).await.unwrap();

    let pr = prime(&store);
    let shots = as_dict(get(&pr, "shots").unwrap());

    let iter0 = as_dict(get(shots, "iter_0").unwrap());
    let check0 = as_dict(get(iter0, "check_prev").unwrap());
    assert_eq!(
        get(check0, "value"),
        Some(&Value::Str("\"ABSENT\"".to_string()))
    );

    let iter1 = as_dict(get(shots, "iter_1").unwrap());
    let check1 = as_dict(get(iter1, "check_prev").unwrap());
    // Pass 1's own `prev` sees pass 0's `echo.value` ("1", stringified
    // again by `check_prev` itself into `"\"1\""`).
    assert_eq!(get(check1, "value"), Some(&Value::Str("\"1\"".to_string())));
}

// ---------------------------------------------------------------------
// a reused named node (a named loop that runs more than once against
// the same store node -- the shape #448/#461 call "reused node": a
// named loop inside an unnamed outer loop, or a second top-level run
// against persisted state)
// ---------------------------------------------------------------------

/// A `json`-registry-free tool plugin that requests cancellation as
/// its own side effect, then reports success -- lets a unit test
/// trigger a genuine mid-run cancellation (the real [`CancellationToken`]
/// a running loop itself polls) deterministically, with no real signal
/// or sleep involved.
struct CancelOnceTool;

#[async_trait::async_trait(?Send)]
impl electricity_tools::ToolPlugin for CancelOnceTool {
    fn name(&self) -> &str {
        "cancel_once"
    }

    async fn execute(
        &self,
        _params: Value,
        call: &electricity_tools::ToolCall<'_>,
    ) -> Result<electricity_tools::ToolResult, electricity_tools::ToolError> {
        call.token.request(2);
        Ok(electricity_tools::ToolResult::new(
            Value::None,
            Value::Dict(IndexMap::new()),
        ))
    }

    fn check(&self) -> electricity_tools::CheckResult {
        electricity_tools::CheckResult {
            ok: true,
            missing: Vec::new(),
            message: None,
        }
    }
}

fn cancel_once_op(path: EffectPath, name: &str) -> Op {
    Op {
        path,
        name: Some(name.to_string()),
        kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
            provider: "cancel_once".to_string(),
            params: ParamNode::Map(IndexMap::new()),
            params_json: None,
            prompt: None,
            model: None,
            timeout_ms: None,
            retries: RetryPolicy::default(),
            expect: None,
            description: None,
            group: None,
        }))),
        on_error: OnError::Fail,
        labels: None,
        enabled: true,
    }
}

/// Review of #466, finding 3: `execute_loop` used to reset `value` to
/// `None` unconditionally on every call (`store.set_leaf`), rather
/// than `node.setdefault("value", None)` as `core/loop.py` does. Only
/// observable when a call bails out *before* its own final write (a
/// cancellation, [`is_fatal`]) -- the setdefault only matters in the
/// gap between the reset and that final write.
#[tokio::test]
async fn value_on_a_reused_node_survives_a_cancellation_that_skips_the_final_write() {
    let store = Store::new();
    let loop_id = LoopId(0);
    let own_path = root_path().push_name("w");
    let body_prefix = own_path.push_pass(loop_id);

    // First call: a real pass runs and writes a real `value`.
    let body1 = vec![stringify_from(
        body_prefix.push_name("step"),
        "step",
        "iter.index",
        "0",
    )];
    let loop_op_1 = loop_op(
        own_path.clone(),
        Some("w"),
        cel_while("state.iter.count < 1"),
        LoopFlow::Chain,
        None,
        None,
        0,
        OnError::Fail,
        None,
        body1,
    );
    run(single(loop_op_1), &store).await.unwrap();
    let pr = prime(&store);
    let w = as_dict(get(&pr, "w").unwrap());
    assert_ne!(get(w, "value"), Some(&Value::None));

    // Second call on the same node: `each` over two items, whose own
    // body is `cancel_once` -- pass 0 completes normally (it requests
    // cancellation as a side effect, but still reports success), so
    // the loop's own `token.is_set()` check ahead of pass 1 is what
    // actually stops it. `execute_loop`'s own outer `except` never
    // runs for a `VmError::Cancelled` (`is_fatal`), so nothing rewrites
    // `value` at the end -- the reset/setdefault line is the only
    // thing that touches it on this call.
    seed_items(&store, vec![Value::from(1_i64), Value::from(2_i64)]);
    let token = CancellationToken::new();
    let mut registry = json_registry();
    registry.register(Box::new(CancelOnceTool));
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);
    let body2 = vec![cancel_once_op(body_prefix.push_name("cancel"), "cancel")];
    let loop_op_2 = loop_op(
        own_path,
        Some("w"),
        each_spec("items", "item", false),
        LoopFlow::Chain,
        None,
        None,
        0,
        OnError::Fail,
        None,
        body2,
    );
    let result = electricity_vm::execute_root(
        &support::program(single(loop_op_2)),
        &store,
        &Value::None,
        &ctx,
        &electricity_vm::NullObserver,
        &token,
    )
    .await;
    assert!(matches!(result, Err(VmError::Cancelled)));

    let pr = prime(&store);
    let w = as_dict(get(&pr, "w").unwrap());
    assert_ne!(
        get(w, "value"),
        Some(&Value::None),
        "a cancelled re-run must not wipe a reused node's previous value"
    );
}

/// Review of #466, finding 4: `core/loop.py`'s success path pops
/// `meta.failed_passes` when empty, but its own `except` path never
/// pops -- it only ever sets the key, when non-empty. A reused node's
/// stale `failed_passes` from an earlier run must survive a
/// `LoopBoundsError` under `on_error: fail` on a later call.
#[tokio::test]
async fn failed_passes_on_a_reused_node_survives_a_later_bounds_error_under_fail() {
    let store = Store::new();
    seed_items(
        &store,
        vec![Value::from(1_i64), Value::from(2_i64), Value::from(3_i64)],
    );

    let loop_id = LoopId(0);
    let own_path = root_path().push_name("shots");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![
        stringify_from(body_prefix.push_name("step"), "step", "item", "0"),
        parse_item_gated(body_prefix.push_name("gate"), "gate", "value == 2"),
    ];
    let loop_op_1 = loop_op(
        own_path.clone(),
        Some("shots"),
        each_spec("items", "item", false),
        LoopFlow::Chain,
        None,
        None,
        0,
        OnError::Continue,
        None,
        body,
    );
    run(single(loop_op_1), &store).await.unwrap();
    let pr = prime(&store);
    let shots = as_dict(get(&pr, "shots").unwrap());
    let meta = as_dict(get(shots, "meta").unwrap());
    let failed = as_list(get(meta, "failed_passes").unwrap());
    assert_eq!(failed, &vec![Value::from(0_i64), Value::from(2_i64)]);

    // Second call on the same node: a `LoopBoundsError` under `fail`
    // raises before any pass runs, so this call's own `failed_passes`
    // stays empty -- cof leaves the stale key from the first call
    // untouched rather than popping it.
    let body2 = vec![stringify_from(
        own_path.push_pass(loop_id).push_name("step"),
        "step",
        "item",
        "0",
    )];
    let loop_op_2 = loop_op(
        own_path,
        Some("shots"),
        each_spec("items", "item", false),
        LoopFlow::Chain,
        None,
        Some(1),
        0,
        OnError::Fail,
        None,
        body2,
    );
    assert!(run(single(loop_op_2), &store).await.is_err());

    let pr = prime(&store);
    let shots = as_dict(get(&pr, "shots").unwrap());
    let meta = as_dict(get(shots, "meta").unwrap());
    let failed = as_list(get(meta, "failed_passes").unwrap());
    assert_eq!(
        failed,
        &vec![Value::from(0_i64), Value::from(2_i64)],
        "a LoopBoundsError under fail must not pop a reused node's stale failed_passes"
    );
}

/// Review of #466, finding 6: Python's own `effective_concurrency`
/// formula (`max(1, min(max_concurrency, total))`) applies even when
/// `total` is 0 -- `electricity-vm` used to special-case `total == 0`
/// back to a flat 0 instead, changing the `dispatch` event's own
/// `concurrency` field.
#[tokio::test]
async fn tree_each_with_zero_passes_and_a_concurrency_cap_still_dispatches_one() {
    let store = Store::new();
    seed_items(&store, vec![Value::from(1_i64), Value::from(2_i64)]);

    let loop_id = LoopId(0);
    let own_path = root_path().push_name("shots");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![stringify_from(
        body_prefix.push_name("step"),
        "step",
        "item",
        "0",
    )];
    let loop_op_ = loop_op(
        own_path,
        Some("shots"),
        each_spec("items", "item", true),
        LoopFlow::Tree,
        Some(4),
        Some(0),
        0,
        OnError::Fail,
        None,
        body,
    );

    let observer = DispatchObserver::default();
    run_with_observer(single(loop_op_), &store, &observer)
        .await
        .unwrap();

    assert_eq!(observer.dispatches.borrow().as_slice(), &[(0, 1)]);
}

/// Conformance case `loop-unnamed-each-tree`'s own discovery: cof's
/// `core/store/store.py::Store.fire_concurrent_dispatch`/`effect_path`
/// take *name* as `str`, but `LoopRuntime.execute`'s tree-each branch
/// passes `self.defn.name` -- `None` for an unnamed loop -- straight
/// through, and `effect_path`'s own f-string literally stringifies
/// that `None` (`f"{prefix}.{name}"`), landing `"prime.None"` in
/// `--events`' own `dispatch` path. Reproduced here, not fixed --
/// "Behaviour must match cof run" -- an unnamed loop's every other
/// event still carries the plain, unsuffixed enclosing path.
#[tokio::test]
async fn unnamed_tree_each_dispatches_with_cofs_own_literal_none_suffix() {
    let store = Store::new();
    seed_items(&store, vec![Value::from(1_i64), Value::from(2_i64)]);

    let own_path = root_path();
    let body = vec![stringify_from(own_path.push_name("x"), "x", "item", "0")];
    let loop_op_ = loop_op(
        own_path,
        None,
        each_spec("items", "item", false),
        LoopFlow::Tree,
        None,
        None,
        0,
        OnError::Fail,
        None,
        body,
    );

    let observer = DispatchObserver::default();
    run_with_observer(single(loop_op_), &store, &observer)
        .await
        .unwrap();

    assert_eq!(
        observer.paths.borrow().as_slice(),
        &["prime.None".to_string()]
    );
}

/// Review of #466, finding 5: `_resolve_collection`'s own error text
/// uses `type(x).__name__` (`"date"`/`"datetime"`), not
/// `Value::type_name`'s fuller `"datetime.date"`/`"datetime.datetime"`.
#[tokio::test]
async fn each_in_resolving_to_a_date_names_the_bare_python_class_in_the_error() {
    let store = Store::new();
    store.set_leaf(
        &store.root,
        k("items"),
        Value::Date(chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
    );

    let loop_id = LoopId(0);
    let own_path = root_path().push_name("shots");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![stringify_from(
        body_prefix.push_name("step"),
        "step",
        "item",
        "0",
    )];
    let loop_op_ = loop_op(
        own_path,
        Some("shots"),
        each_spec("items", "item", false),
        LoopFlow::Chain,
        None,
        None,
        0,
        OnError::Fail,
        None,
        body,
    );

    run(single(loop_op_), &store).await.unwrap();

    let pr = prime(&store);
    let shots = as_dict(get(&pr, "shots").unwrap());
    let meta = as_dict(get(shots, "meta").unwrap());
    let err = as_str(get(meta, "each_in_error").unwrap());
    assert!(
        err.contains("resolved to date instead of list"),
        "unexpected each_in_error: {err}"
    );
}

// ---------------------------------------------------------------------
// VmError::Refused (#462, lane I prerequisites) -- fatal for control
// only, recorded on the loop's own node like an ordinary failure
// ---------------------------------------------------------------------

/// A chain each pass's own refused `use` child (the sentinel
/// `exec::use_::execute_use` recognizes) is never absorbed by
/// `on_error: continue`, propagates unwrapped (no `step: ` prefix),
/// and still leaves the loop's own node recorded exactly like an
/// ordinary failure -- `value`/`meta.error`/`completed_at` all
/// written, unlike a real cancellation (`VmError::is_fatal`'s own doc
/// comment, orchestrator ruling R1 on PR #462).
#[tokio::test]
async fn a_refused_chain_pass_is_never_absorbed_and_is_still_recorded_on_the_node() {
    let store = Store::new();
    seed_items(&store, vec![Value::from(1_i64), Value::from(2_i64)]);

    let loop_id = LoopId(0);
    let own_path = root_path().push_name("shots");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![refused_use_leaf(
        body_prefix.push_name("child"),
        "child",
        OnError::Fail,
    )];
    let loop_op_ = loop_op(
        own_path,
        Some("shots"),
        each_spec("items", "item", false),
        LoopFlow::Chain,
        None,
        None,
        0,
        OnError::Continue,
        None,
        body,
    );

    let err = run(single(loop_op_), &store)
        .await
        .expect_err("a refused use child must never be absorbed by on_error: continue");
    let VmError::Refused { child_label, .. } = err else {
        panic!("expected VmError::Refused, got {err:?}");
    };
    assert_eq!(child_label, "__test_only_refused__");

    let pr = prime(&store);
    let shots = as_dict(get(&pr, "shots").unwrap());
    let meta = as_dict(get(shots, "meta").unwrap());
    assert!(
        get(meta, "completed_at").is_some(),
        "a refused pass is recorded like an ordinary failure, not bypassed like a cancellation: {meta:?}"
    );
    let error_text = match get(meta, "error") {
        Some(Value::Str(text)) => text.clone(),
        other => panic!("expected a string meta.error, got {other:?}"),
    };
    assert!(
        error_text.contains("use child __test_only_refused__"),
        "unexpected meta.error: {error_text}"
    );
    assert!(get(shots, "value").is_some());
}

/// The tree-flow twin: `max_concurrency: 1` makes pass 0's own refusal
/// deterministic -- it must stop the loop from ever dispatching pass 1
/// at all (`exec::dynamic::execute_tree`'s own split, mirrored here),
/// yet still merge pass 0's own isolated store back (a refused branch
/// is recorded, not bypassed).
#[tokio::test]
async fn a_refused_tree_pass_stops_dispatch_but_still_merges() {
    let store = Store::new();
    seed_items(&store, vec![Value::from(1_i64), Value::from(2_i64)]);

    let loop_id = LoopId(0);
    let own_path = root_path().push_name("shots");
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![refused_use_leaf(
        body_prefix.push_name("child"),
        "child",
        OnError::Fail,
    )];
    let loop_op_ = loop_op(
        own_path,
        Some("shots"),
        each_spec("items", "item", false),
        LoopFlow::Tree,
        Some(1),
        None,
        0,
        OnError::Continue,
        None,
        body,
    );

    let err = run(single(loop_op_), &store)
        .await
        .expect_err("a refused use child must never be absorbed by on_error: continue");
    let VmError::Refused { child_label, .. } = err else {
        panic!("expected VmError::Refused, got {err:?}");
    };
    assert_eq!(child_label, "__test_only_refused__");

    let pr = prime(&store);
    let shots = as_dict(get(&pr, "shots").unwrap());
    assert!(
        get(shots, "iter_1").is_none(),
        "max_concurrency: 1 means pass 1 must never have been dispatched: {shots:?}"
    );
    let meta = as_dict(get(shots, "meta").unwrap());
    assert!(
        get(meta, "completed_at").is_some(),
        "a refused pass is recorded like an ordinary failure, not bypassed like a cancellation: {meta:?}"
    );
}
