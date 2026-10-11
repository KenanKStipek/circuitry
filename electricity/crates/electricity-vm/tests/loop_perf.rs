//! Lane H (#461), #442's own "Interpreter cost" item, follow-up #463:
//! a regression guard for `exec::live_ctx`'s per-render context copy,
//! run through a chain-flow `each` loop (the case that multiplies it
//! most: every render inside pass *N* re-copies every earlier pass's
//! own `iter_<K>` subtree, `K < N`, in full).
//!
//! This is **not** the asymptotic fix #463 tracks -- see `exec::
//! live_ctx`'s own doc comment and this PR's own body for why that
//! needs a change to `electricity-value::Value` outside this lane's
//! footprint. This test only pins today's *measured* behaviour on this
//! machine, generously, so a future accidental regression (an
//! inadvertent cubic blow-up, or a big constant-factor regression) is
//! still caught even though the quadratic shape itself is not yet
//! fixed here.

mod support;

use electricity_bytecode::{
    EffectPath, Escape, LeafKind, LoopFlow, LoopId, LoopSpec, NodeKind, OnError, Op, ParamNode,
    Region, RetryPolicy, TemplateText, ToolOp,
};
use electricity_value::Value;
use electricity_vm::{CancellationToken, Limiter, NullObserver, Store};
use indexmap::IndexMap;
use std::time::Instant;
use support::{json_registry, run_ctx};

fn k(s: &str) -> Value {
    Value::Str(s.to_string())
}

fn stringify_item(path: EffectPath, name: &str) -> Op {
    let mut params = IndexMap::new();
    params.insert(
        k("mode"),
        ParamNode::Literal(Value::Str("stringify".to_string())),
    );
    params.insert(
        k("input"),
        ParamNode::Template(TemplateText::new("{{item}}", false, Escape::None)),
    );
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

fn named_each_chain_loop(name: &str, flow: LoopFlow, max_concurrency: Option<u32>) -> Op {
    let loop_id = LoopId(0);
    let own_path = EffectPath::root().push_name(name);
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![
        stringify_item(body_prefix.push_name("a"), "a"),
        stringify_item(body_prefix.push_name("b"), "b"),
    ];
    Op {
        path: own_path.clone(),
        name: Some(name.to_string()),
        kind: NodeKind::Control(Region::Loop {
            name: Some(name.to_string()),
            spec: LoopSpec::Each {
                in_path: "items".to_string(),
                as_name: "item".to_string(),
                truncate: false,
            },
            body: Box::new(Region::Block {
                ops: body,
                overlay: true,
            }),
            flow,
            max_concurrency,
            max_iterations: None,
            min_iterations: 0,
            collect: None,
        }),
        on_error: OnError::Fail,
        labels: None,
        enabled: true,
    }
}

async fn run_each_chain_of(
    n: usize,
    flow: LoopFlow,
    max_concurrency: Option<u32>,
) -> std::time::Duration {
    let store = Store::new();
    let items: Vec<Value> = (0..n).map(|i| Value::from(i as i64)).collect();
    store.set_leaf(&store.root, k("items"), Value::List(items));

    let loop_op = named_each_chain_loop("shots", flow, max_concurrency);
    let root = support::chain_dynamic(EffectPath::root(), "prime", OnError::Fail, vec![loop_op]);

    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let ctx = run_ctx(&registry, &limiter);

    let t0 = Instant::now();
    electricity_vm::execute_root(
        &support::program(root),
        &store,
        &Value::None,
        &ctx,
        &NullObserver,
        &token,
    )
    .await
    .unwrap();
    t0.elapsed()
}

/// Regression guard, chain flow: the dominant case for `live_ctx`'s own
/// per-render re-copy (`exec::live_ctx`'s doc comment) -- every render
/// inside pass *N* re-walks every earlier, already-frozen pass's own
/// `iter_<K>` subtree in full. `#[ignore]`d (a 1000-pass debug-profile
/// run takes well over a minute -- too slow for the ordinary `cargo
/// test --workspace` gate); run explicitly with `cargo test --release
/// -p electricity-vm --test loop_perf -- --ignored` (`.github/workflows/
/// electricity.yml` runs exactly that in CI, the same convention as
/// this workflow's existing `--release` steps for `electricity-yaml`/
/// `electricity-cel`).
///
/// Measured on the development machine (`cargo test --release ... --
/// --ignored`, one core's worth of real work, no other load): 250
/// passes ≈ 0.67s, 1000 passes (4x the passes) ≈ 12.2s (≈18x the
/// time) -- the quadratic shape #463 tracks (today's `live_ctx`
/// re-copies the whole accumulated state on every render), not yet
/// fixed by this lane (see `exec::live_ctx`'s own doc comment and this
/// PR's own body for why #463's own asymptotic fix, not this guard,
/// is the right place for that). Only the growth *ratio* is asserted,
/// not a wall-clock ceiling (a fixed-seconds ceiling is exactly what
/// flakes on a loaded or slower CI runner): 40x gives headroom over
/// today's measured ≈18x (tolerating the quadratic shape itself plus
/// noise) while still catching a future accidental cubic-or-worse
/// blow-up (4x the passes would cost ≈64x the time at a cubic growth
/// rate).
#[tokio::test]
#[ignore]
async fn chain_each_loop_of_a_thousand_passes_stays_within_a_generous_ceiling() {
    let small = run_each_chain_of(250, LoopFlow::Chain, None).await;
    let large = run_each_chain_of(1000, LoopFlow::Chain, None).await;

    eprintln!("chain each: 250 passes = {small:?}, 1000 passes = {large:?}");

    if small.as_millis() > 0 {
        let ratio = large.as_secs_f64() / small.as_secs_f64();
        assert!(
            ratio < 40.0,
            "1000 passes took {ratio:.1}x as long as 250 passes ({small:?} -> {large:?}); \
             expected well under a 40x blow-up (today's own quadratic shape measures ≈18x)"
        );
    }
}

/// Same guard, tree flow with bounded concurrency: isolated per-pass
/// stores merged once at the end, so the per-render re-copy cost comes
/// from the *other* ancestors in the chain, not from sibling passes --
/// kept as its own test since #461's own test plan calls for both
/// shapes explicitly. Measured: 250 passes ≈ 24ms, 1000 passes ≈
/// 146ms -- far cheaper than chain flow (no accumulated sibling-pass
/// re-copy; each pass's own isolated store starts empty and is merged
/// once, at the end), so this bound is tight by comparison and mostly
/// guards against a regression that makes tree flow pay chain flow's
/// own cost.
#[tokio::test]
#[ignore]
async fn tree_each_loop_of_a_thousand_passes_stays_within_a_generous_ceiling() {
    let small = run_each_chain_of(250, LoopFlow::Tree, Some(8)).await;
    let large = run_each_chain_of(1000, LoopFlow::Tree, Some(8)).await;

    eprintln!("tree each (max_concurrency=8): 250 passes = {small:?}, 1000 passes = {large:?}");

    if small.as_millis() > 0 {
        let ratio = large.as_secs_f64() / small.as_secs_f64();
        assert!(
            ratio < 40.0,
            "1000 passes took {ratio:.1}x as long as 250 passes ({small:?} -> {large:?}); \
             expected well under a 40x blow-up (today's own measured ≈6x)"
        );
    }
}
