//! Lane H (#461), #442's own "Interpreter cost" item, #463: a
//! regression guard for the context a render/condition-eval builds
//! inside a loop -- originally pinned against `exec::live_ctx`'s own
//! per-render whole-subtree copy (chain-flow `each`, the case that
//! multiplied it most: every render inside pass *N* re-copied every
//! earlier pass's own `iter_<K>` subtree, `K < N`, in full), now also
//! covering the `while`-loop shape #463's own acceptance bound names
//! directly (`exec::live_scope`'s own module doc comment has the fix:
//! a tool render / `if`/`while` condition resolves `ctx`/`state` from
//! the live store, a dotted segment at a time, instead of snapshotting
//! the whole thing first). Both loop shapes now grow close to linearly;
//! the ratio ceilings below stay well above that to tolerate machine
//! noise, not because either shape is still quadratic.

mod support;

use electricity_bytecode::{
    Condition, EffectPath, Escape, LeafKind, LoopFlow, LoopId, LoopSpec, NodeKind, OnError, Op,
    ParamNode, Region, RetryPolicy, TemplateText, ToolOp, UseSource,
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
/// Measured on the development machine, release, before this lane's
/// own fix (`cargo test --release ... -- --ignored`, one core's worth
/// of real work, no other load): 250 passes ≈ 0.67s, 1000 passes (4x
/// the passes) ≈ 12.2s (≈18x the time) -- the quadratic shape #463
/// tracked (`live_ctx`'s own `Store::snapshot` re-copying the whole
/// accumulated state on every render). After this lane's own fix
/// (`exec::live_scope`): 250 passes ≈ 9ms, 1000 passes ≈ 63ms, ≈7x --
/// close to the 4x a truly linear cost would give, the rest being
/// this test's own fixed per-pass bookkeeping (two tool leaves, retry/
/// observer/meta writes) rather than anything #463 was ever about.
/// Only the growth *ratio* is asserted, not a wall-clock ceiling (a
/// fixed-seconds ceiling is exactly what flakes on a loaded or slower
/// CI runner): 15x gives several times today's own measured ≈7x
/// headroom for machine noise while still catching a regression back
/// toward the old quadratic shape (≈18x) or worse.
#[tokio::test]
#[ignore]
async fn chain_each_loop_of_a_thousand_passes_stays_within_a_generous_ceiling() {
    let small = run_each_chain_of(250, LoopFlow::Chain, None).await;
    let large = run_each_chain_of(1000, LoopFlow::Chain, None).await;

    eprintln!("chain each: 250 passes = {small:?}, 1000 passes = {large:?}");

    if small.as_millis() > 0 {
        let ratio = large.as_secs_f64() / small.as_secs_f64();
        assert!(
            ratio < 15.0,
            "1000 passes took {ratio:.1}x as long as 250 passes ({small:?} -> {large:?}); \
             expected well under a 15x blow-up (this lane's own fix measures ≈7x)"
        );
    }
}

/// Same guard, tree flow with bounded concurrency: isolated per-pass
/// stores merged once at the end, so the per-render re-copy cost comes
/// from the *other* ancestors in the chain, not from sibling passes --
/// kept as its own test since #461's own test plan calls for both
/// shapes explicitly. Measured, after this lane's own fix: 250 passes
/// ≈ 6ms, 1000 passes ≈ 41ms, ≈7x -- already cheap before this lane
/// (no accumulated sibling-pass re-copy; each pass's own isolated
/// store starts empty and is merged once, at the end), essentially
/// unchanged by it.
#[tokio::test]
#[ignore]
async fn tree_each_loop_of_a_thousand_passes_stays_within_a_generous_ceiling() {
    let small = run_each_chain_of(250, LoopFlow::Tree, Some(8)).await;
    let large = run_each_chain_of(1000, LoopFlow::Tree, Some(8)).await;

    eprintln!("tree each (max_concurrency=8): 250 passes = {small:?}, 1000 passes = {large:?}");

    if small.as_millis() > 0 {
        let ratio = large.as_secs_f64() / small.as_secs_f64();
        assert!(
            ratio < 15.0,
            "1000 passes took {ratio:.1}x as long as 250 passes ({small:?} -> {large:?}); \
             expected well under a 15x blow-up (this lane's own fix measures ≈7x)"
        );
    }
}

fn named_while_loop(name: &str, max_iterations: u32) -> Op {
    let loop_id = LoopId(0);
    let own_path = EffectPath::root().push_name(name);
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![stringify_item(body_prefix.push_name("a"), "a")];
    Op {
        path: own_path.clone(),
        name: Some(name.to_string()),
        kind: NodeKind::Control(Region::Loop {
            name: Some(name.to_string()),
            spec: LoopSpec::While(Condition::Cel {
                expr: "state.iter.index < 999999".to_string(),
                strict: false,
            }),
            body: Box::new(Region::Block {
                ops: body,
                overlay: true,
            }),
            flow: LoopFlow::Chain,
            max_concurrency: None,
            max_iterations: Some(max_iterations),
            min_iterations: 0,
            collect: None,
        }),
        on_error: OnError::Fail,
        labels: None,
        enabled: true,
    }
}

async fn run_while_of(n: u32) -> std::time::Duration {
    let store = Store::new();
    let loop_op = named_while_loop("shots", n);
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

/// Issue #463's own acceptance test: a `while` loop, one `json` tool
/// per pass (the issue's own reproduction) -- a `max_iterations` cap
/// (never the condition itself, kept always-true up to 999999) stops
/// it at exactly *n* passes, so every pass also re-evaluates the
/// `while` condition itself (`exec::loop_::run_while`'s own
/// `cond_chain`), the other per-pass hot path `exec::live_scope::
/// build_cel_state` fixes alongside the tool leaf's own render.
/// `#[ignore]`d for the same reason as the `each` guards above; run
/// explicitly with `cargo test --release -p electricity-vm --test
/// loop_perf -- --ignored`.
///
/// Before this lane's own fix, this shape was the same quadratic cost
/// as the `each` case (the issue's own body, and its design-study
/// comment's own `while`-loop reproduction, measured ≈18-24x for 4x
/// the passes). Measured on the development machine, release, after
/// this lane's own fix: 250 passes ≈ 13ms, 1000 passes ≈ 52ms, ≈4x --
/// at the truly-linear bound itself. 8x is this issue's own
/// acceptance ceiling (its body: "growth from 250 to 1000 passes stays
/// within about 4x"; 8x keeps headroom over that for machine noise
/// while still catching a regression back toward the old ≈20x shape).
#[tokio::test]
#[ignore]
async fn while_loop_of_a_thousand_passes_stays_within_the_issues_own_ceiling() {
    let small = run_while_of(250).await;
    let large = run_while_of(1000).await;

    eprintln!("while: 250 passes = {small:?}, 1000 passes = {large:?}");

    if small.as_millis() > 0 {
        let ratio = large.as_secs_f64() / small.as_secs_f64();
        assert!(
            ratio < 8.0,
            "1000 passes took {ratio:.1}x as long as 250 passes ({small:?} -> {large:?}); \
             expected under #463's own 8x ceiling for a truly linear cost"
        );
    }
}

/// A `use` effect's own body, inline so no file I/O enters the
/// measurement: an empty child (`"effects: []"`) with one `{from:}`
/// input (`iter.index`) -- cheap enough that the *child run* itself
/// stays near-zero, so this isolates `execute_use`'s own per-pass
/// cost, the same way [`stringify_item`]'s single `json` tool isolates
/// [`run_while_of`]'s.
fn use_loop_item(path: EffectPath, name: &str) -> Op {
    let mut inputs = IndexMap::new();
    inputs.insert(
        Value::Str("idx".to_string()),
        ParamNode::From {
            path: "iter.index".to_string(),
            default: None,
        },
    );
    support::use_leaf(
        path,
        name,
        UseSource::Inline(TemplateText::new("effects: []", false, Escape::None)),
        Some(ParamNode::Map(inputs)),
        None,
        true,
        RetryPolicy::default(),
        None,
        OnError::Fail,
    )
}

fn named_while_use_loop(name: &str, max_iterations: u32) -> Op {
    let loop_id = LoopId(0);
    let own_path = EffectPath::root().push_name(name);
    let body_prefix = own_path.push_pass(loop_id);
    let body = vec![use_loop_item(body_prefix.push_name("child"), "child")];
    Op {
        path: own_path.clone(),
        name: Some(name.to_string()),
        kind: NodeKind::Control(Region::Loop {
            name: Some(name.to_string()),
            spec: LoopSpec::While(Condition::Cel {
                expr: "state.iter.index < 999999".to_string(),
                strict: false,
            }),
            body: Box::new(Region::Block {
                ops: body,
                overlay: true,
            }),
            flow: LoopFlow::Chain,
            max_concurrency: None,
            max_iterations: Some(max_iterations),
            min_iterations: 0,
            collect: None,
        }),
        on_error: OnError::Fail,
        labels: None,
        enabled: true,
    }
}

async fn run_while_use_of(n: u32) -> std::time::Duration {
    let store = Store::new();
    let loop_op = named_while_use_loop("shots", n);
    let root = support::chain_dynamic(EffectPath::root(), "prime", OnError::Fail, vec![loop_op]);

    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    // `use_: true` -- the default `run_ctx`'s own `Supported::m0`
    // refuses a `use` effect outright before this loop ever starts.
    ctx.supported = electricity_bytecode::Supported::current(&[]);

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

/// PR #482 review finding 2: `execute_use` still took a full `live_ctx`
/// snapshot on every pass, unlike the tool leaf this lane's own fix
/// covers -- so a `use` effect's own input render stayed O(the live
/// state reachable from the loop) per pass, the same quadratic shape
/// #463 tracks for a tool leaf. Guards the fix (`exec::use_::
/// execute_use` now builds a lazy `exec::live_scope::LiveRoot` the
/// same way `exec::tool` does) the same way
/// [`while_loop_of_a_thousand_passes_stays_within_the_issues_own_ceiling`]
/// guards the tool leaf's own fix. `#[ignore]`d for the same reason as
/// every other guard in this file.
#[tokio::test]
#[ignore]
async fn while_loop_of_a_thousand_passes_with_a_use_effect_stays_within_the_issues_own_ceiling() {
    let small = run_while_use_of(250).await;
    let large = run_while_use_of(1000).await;

    eprintln!("while (use effect): 250 passes = {small:?}, 1000 passes = {large:?}");

    if small.as_millis() > 0 {
        let ratio = large.as_secs_f64() / small.as_secs_f64();
        assert!(
            ratio < 8.0,
            "1000 passes took {ratio:.1}x as long as 250 passes ({small:?} -> {large:?}); \
             expected under #463's own 8x ceiling for a truly linear cost"
        );
    }
}
