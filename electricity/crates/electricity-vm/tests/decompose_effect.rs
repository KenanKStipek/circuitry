//! M1 lane L2b (issue #522): runtime decomposition wired into the
//! `prompt` effect's own dispatch seam -- integration tests against a
//! real, compiled document (`execute_root`), the same convention
//! `tests/prompt_effect.rs`/`tests/use_effect.rs` use. `decompose.rs`'s
//! own unit tests cover the pure helpers directly; these exercise
//! `decompose::maybe_decompose` through `exec::prompt::execute_prompt`
//! itself, so what a real run actually writes to state/events is
//! pinned here, not just the function's own return value.

mod support;

use async_trait::async_trait;
use electricity_bytecode::effects::{PromptContent, PromptOp, PromptType};
use electricity_bytecode::template::{Escape, TemplateText};
use electricity_bytecode::{EffectPath, LeafKind, NodeKind, OnError, Op};
use electricity_config::{ComplexitySettings, DecompositionSettings};
use electricity_value::{Dict, Value};
use electricity_vm::adapter::{
    Adapter, AdapterError, AdapterRegistry, CallContext, CheckResult, GenerateOptions,
    GenerateResult, PyExc,
};
use electricity_vm::retry::RetryInfo;
use electricity_vm::{CancellationToken, Limiter, Store, VmError};
use indexmap::IndexMap;
use std::rc::Rc;
use support::{Event, RecordingObserver, chain_dynamic, json_registry, program, run_ctx};

fn root_path() -> EffectPath {
    EffectPath::root()
}

fn template_prompt(template: &str) -> PromptOp {
    PromptOp {
        content: PromptContent::Template(TemplateText::new(template, true, Escape::None)),
        prompt_type: PromptType::Text,
        schema: None,
        model: None,
        provider: None,
        provider_fallbacks: Vec::new(),
        routing_override: None,
        model_params: None,
        timeout_ms: None,
        deterministic: false,
        inputs: None,
        assets: Vec::new(),
        retries: None,
        description: None,
        group: None,
    }
}

fn prompt_leaf(path: EffectPath, name: &str, op: PromptOp, on_error: OnError) -> Op {
    Op {
        path,
        name: Some(name.to_string()),
        kind: NodeKind::Leaf(Box::new(LeafKind::Prompt(op))),
        on_error,
        labels: None,
        enabled: true,
    }
}

fn get<'a>(dict: &'a IndexMap<Value, Value>, key: &str) -> Option<&'a Value> {
    dict.get(&Value::Str(key.to_string()))
}

fn as_dict(value: &Value) -> &IndexMap<Value, Value> {
    match value {
        Value::Dict(d) => d,
        other => panic!("expected a dict, got {other:?}"),
    }
}

/// A fixed-queue fake adapter, shared by the triggering prompt's own
/// default adapter, the planner's prompts and the plan's own chunk
/// prompts alike (`decompose::maybe_decompose` hands both the planner
/// and the plan the caller's own `default_adapter`, unchanged) -- same
/// shape as `tests/prompt_effect.rs`'s own `FakeAdapter`, duplicated
/// rather than shared, since integration-test binaries don't share
/// code except through `mod support`.
struct FakeAdapter {
    name: String,
    replies: std::cell::RefCell<std::collections::VecDeque<Result<GenerateResult, AdapterError>>>,
}

impl FakeAdapter {
    fn new(name: &str, replies: Vec<Result<GenerateResult, AdapterError>>) -> Self {
        FakeAdapter {
            name: name.to_string(),
            replies: std::cell::RefCell::new(replies.into()),
        }
    }

    fn text(text: &str) -> Result<GenerateResult, AdapterError> {
        Ok(GenerateResult {
            text: text.to_string(),
            raw: Value::None,
            tokens_sent: Some(3),
            tokens_received: Some(5),
            finish_reason: None,
            warnings: Vec::new(),
            cost_usd: None,
        })
    }
}

#[async_trait(?Send)]
impl Adapter for FakeAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    async fn generate(
        &self,
        _model: &str,
        _prompt: &str,
        _options: &GenerateOptions,
        _call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        self.replies.borrow_mut().pop_front().unwrap_or_else(|| {
            Err(AdapterError {
                message: "FakeAdapter: no reply queued".to_string(),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::RuntimeError,
            })
        })
    }

    async fn check(&self) -> CheckResult {
        CheckResult {
            ok: true,
            ..Default::default()
        }
    }
}

fn registry_with_scripted() -> AdapterRegistry {
    let mut adapters = AdapterRegistry::new();
    adapters.register(
        "scripted",
        Box::new(|_name, _cfg| {
            Ok(Box::new(FakeAdapter::new("scripted", Vec::new())) as Box<dyn Adapter>)
        }),
    );
    adapters
}

/// Decomposition enabled with *threshold* strictly below any real
/// score a one-line template produces, so a genuine K1 score (scoring
/// must actually run -- `maybe_decompose` reads `meta.complexity`,
/// never a value a test hands it directly) always triggers.
fn decomposing_complexity(threshold: f64, max_depth: i64, max_chunks: i64) -> ComplexitySettings {
    let mut settings = ComplexitySettings::default();
    settings.scoring.enabled = true;
    settings.decomposition = DecompositionSettings {
        enabled: true,
        threshold,
        max_depth,
        max_chunks,
        on_failure: "route_up".to_string(),
    };
    settings
}

/// A genuinely high-scoring template (K1's real scorer, never a
/// value a test hands it directly -- `maybe_decompose` only ever
/// reads `meta.complexity`) -- long, repeats several of K1's own
/// keyword-weighted phrases, scores around 18-19 on a 0-100 scale, well
/// above a short one-line chunk template's own ~2.7. Used as the one
/// prompt a test actually wants to trigger on; every chunk inside an
/// emitted plan stays short on purpose, so it never re-triggers
/// decomposition itself at `max_depth: 2`.
fn big_template() -> String {
    "Analyze and critique the following in great depth, cross-reference \
     every claim, and deduce the underlying causes. "
        .repeat(10)
}

/// The planner's own schema-shaped reply: `{say, chunks, yaml}`, JSON
/// text, via `electricity_json::dumps` so the embedded multi-line YAML
/// comes back properly escaped.
fn planner_reply(yaml: &str, chunk_names: &[&str]) -> Result<GenerateResult, AdapterError> {
    let mut dict = Dict::new();
    dict.insert(
        Value::Str("say".to_string()),
        Value::Str("split into chunks".to_string()),
    );
    let chunks: Vec<Value> = chunk_names
        .iter()
        .map(|name| {
            let mut chunk = Dict::new();
            chunk.insert(Value::Str("name".to_string()), Value::Str(name.to_string()));
            chunk.insert(
                Value::Str("job".to_string()),
                Value::Str(format!("do {name}")),
            );
            Value::Dict(chunk)
        })
        .collect();
    dict.insert(Value::Str("chunks".to_string()), Value::List(chunks));
    dict.insert(Value::Str("yaml".to_string()), Value::Str(yaml.to_string()));
    let text = electricity_json::dumps(&Value::Dict(dict), electricity_json::WriteMode::COMPACT)
        .expect("a flat {say, chunks, yaml} mapping always dumps");
    Ok(GenerateResult {
        text,
        raw: Value::None,
        tokens_sent: Some(11),
        tokens_received: Some(13),
        finish_reason: None,
        warnings: Vec::new(),
        cost_usd: None,
    })
}

/// A sound two-chunk plan: `a`/`b` each a `prompt`, `merge` a `tool`
/// (no adapter call of its own) parsing a constant -- the FakeAdapter's
/// own queue only ever needs one reply per *prompt* dispatch, not per
/// top-level effect.
const SOUND_PLAN_YAML: &str = "\
effects:\n\
\x20\x20- type: prompt\n\
\x20\x20\x20\x20name: a\n\
\x20\x20\x20\x20template: \"chunk a\"\n\
\x20\x20- type: prompt\n\
\x20\x20\x20\x20name: b\n\
\x20\x20\x20\x20template: \"chunk b\"\n\
\x20\x20- type: tool\n\
\x20\x20\x20\x20name: merge\n\
\x20\x20\x20\x20provider: json\n\
\x20\x20\x20\x20params: {mode: parse, input: '\"merged\"'}\n\
";

async fn run_decomposing_prompt(
    op: PromptOp,
    on_error: OnError,
    complexity: &ComplexitySettings,
    replies: Vec<Result<GenerateResult, AdapterError>>,
) -> (Store, RecordingObserver, Result<(), VmError>) {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    ctx.default_adapter = Rc::new(FakeAdapter::new("scripted", replies));
    let adapters = registry_with_scripted();
    ctx.adapters = &adapters;
    ctx.model = "m";
    ctx.complexity = complexity;
    ctx.supported = electricity_bytecode::Supported::current(&[]);
    let leaf = prompt_leaf(root_path().push_name("big"), "big", op, on_error);
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![leaf]);
    let observer = RecordingObserver::new();
    let result = electricity_vm::execute_root(
        &program(root),
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
async fn a_successful_decomposition_writes_the_merged_value_and_the_record() {
    let complexity = decomposing_complexity(10.0, 2, 5);
    let replies = vec![
        planner_reply(SOUND_PLAN_YAML, &["a", "b"]),
        FakeAdapter::text("chunk a reply"),
        FakeAdapter::text("chunk b reply"),
    ];
    let big = big_template();
    let (store, observer, result) =
        run_decomposing_prompt(template_prompt(&big), OnError::Fail, &complexity, replies).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let big = as_dict(get(prime, "big").unwrap());
    assert_eq!(get(big, "value"), Some(&Value::Str("merged".to_string())));

    let meta = as_dict(get(big, "meta").unwrap());
    let decomposition = as_dict(get(meta, "decomposition").unwrap());
    assert_eq!(get(decomposition, "decomposed"), Some(&Value::Bool(true)));
    assert_eq!(
        get(decomposition, "outcome"),
        Some(&Value::Str("decomposed".to_string()))
    );
    assert_eq!(get(decomposition, "reason"), Some(&Value::None));
    assert_eq!(get(decomposition, "chunk_count"), Some(&Value::from(2i64)));

    // `fallback_attempts: []`, `fallback_recovered: false` and
    // `tokens_*: null` stay from the pre-dispatch block -- issue #522's
    // own scope section 1.
    assert_eq!(get(meta, "fallback_attempts"), Some(&Value::List(vec![])));
    assert_eq!(get(meta, "fallback_recovered"), Some(&Value::Bool(false)));
    assert_eq!(get(meta, "tokens_sent"), Some(&Value::None));
    assert_eq!(get(meta, "tokens_received"), Some(&Value::None));
    // No `retries_used`, `warnings`, `assets`, `finish_reason` or
    // `cost_usd` is ever written for a decomposed prompt.
    for absent in [
        "retries_used",
        "warnings",
        "assets",
        "finish_reason",
        "cost_usd",
    ] {
        assert_eq!(get(meta, absent), None, "{absent} must be absent");
    }

    // The planner's effects, then the plan's own, share the prompt's
    // own path as prefix (issue #522's Settled question 8).
    let events = observer.events();
    let paths: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            Event::Start(p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    assert!(paths.contains(&"prime.big.plan".to_string()), "{paths:?}");
    assert!(paths.contains(&"prime.big.check".to_string()), "{paths:?}");
    assert!(
        paths.contains(&"prime.big.result_path".to_string()),
        "{paths:?}"
    );
    assert!(paths.contains(&"prime.big.done".to_string()), "{paths:?}");
    assert!(paths.contains(&"prime.big.a".to_string()), "{paths:?}");
    assert!(paths.contains(&"prime.big.b".to_string()), "{paths:?}");
    assert!(paths.contains(&"prime.big.merge".to_string()), "{paths:?}");
    let plan_index = paths.iter().position(|p| p == "prime.big.plan").unwrap();
    let a_index = paths.iter().position(|p| p == "prime.big.a").unwrap();
    assert!(
        plan_index < a_index,
        "the planner's own effects must dispatch before the plan's own: {paths:?}"
    );

    // Every child effect counts in the run's own totals.
    assert!(
        events
            .iter()
            .filter(|e| matches!(e, Event::Totals(_)))
            .count()
            >= 3,
        "expected record_totals from the planner's plan, and the plan's own a/b: {events:?}"
    );

    // The triggering prompt `big` itself calls `record_totals` exactly
    // once -- issue #522's own Scope section 1's `Decomposed(value)`
    // arm, never once per child effect. Only `big`'s own meta snapshot
    // carries a `decomposition` key (the planner's own prompt and the
    // plan's chunks never decompose themselves at this threshold), so
    // filtering the recorded `Totals` events on that key isolates it
    // from the child effects' own totals counted above.
    let bigs_own_totals = events
        .iter()
        .filter(|e| matches!(e, Event::Totals(meta) if meta.contains("\"decomposition\"")))
        .count();
    assert_eq!(bigs_own_totals, 1, "{events:?}");
}

#[tokio::test]
async fn decomposition_does_not_trigger_when_disabled() {
    let mut complexity = decomposing_complexity(-1.0, 2, 5);
    complexity.decomposition.enabled = false;
    let (store, _observer, result) = run_decomposing_prompt(
        template_prompt("hi"),
        OnError::Fail,
        &complexity,
        vec![FakeAdapter::text("ran as is")],
    )
    .await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let big = as_dict(get(prime, "big").unwrap());
    assert_eq!(
        get(big, "value"),
        Some(&Value::Str("ran as is".to_string()))
    );
    let meta = as_dict(get(big, "meta").unwrap());
    assert_eq!(get(meta, "decomposition"), None);
}

#[tokio::test]
async fn decomposition_does_not_trigger_without_a_complexity_meta() {
    // Scoring off -> no `meta.complexity` at all -> `maybe_decompose`
    // returns `None` regardless of the threshold.
    let mut complexity = decomposing_complexity(-1.0, 2, 5);
    complexity.scoring.enabled = false;
    let (store, _observer, result) = run_decomposing_prompt(
        template_prompt("hi"),
        OnError::Fail,
        &complexity,
        vec![FakeAdapter::text("ran as is")],
    )
    .await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let big = as_dict(get(prime, "big").unwrap());
    assert_eq!(
        get(big, "value"),
        Some(&Value::Str("ran as is".to_string()))
    );
    let meta = as_dict(get(big, "meta").unwrap());
    assert_eq!(get(meta, "complexity"), None);
    assert_eq!(get(meta, "decomposition"), None);
}

#[tokio::test]
async fn a_score_exactly_at_the_threshold_does_not_trigger() {
    // First run scoring-only to learn the real score this template
    // gets, then set `threshold` to exactly that value -- `score >
    // threshold` is strict, so an equal score must run as is.
    let probe_complexity = {
        let mut c = ComplexitySettings::default();
        c.scoring.enabled = true;
        c
    };
    let (store, _observer, result) = run_decomposing_prompt(
        template_prompt("hi"),
        OnError::Fail,
        &probe_complexity,
        vec![FakeAdapter::text("probe")],
    )
    .await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let big = as_dict(get(prime, "big").unwrap());
    let meta = as_dict(get(big, "meta").unwrap());
    let complexity_meta = as_dict(get(meta, "complexity").unwrap());
    let score = match get(complexity_meta, "score").unwrap() {
        Value::Float(f) => *f,
        other => panic!("expected a float score, got {other:?}"),
    };

    let complexity = decomposing_complexity(score, 2, 5);
    let (store, _observer, result) = run_decomposing_prompt(
        template_prompt("hi"),
        OnError::Fail,
        &complexity,
        vec![FakeAdapter::text("ran as is, not decomposed")],
    )
    .await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let big = as_dict(get(prime, "big").unwrap());
    assert_eq!(
        get(big, "value"),
        Some(&Value::Str("ran as is, not decomposed".to_string()))
    );
    let meta = as_dict(get(big, "meta").unwrap());
    assert_eq!(get(meta, "decomposition"), None);
}

#[tokio::test]
async fn the_depth_ceiling_dispatches_without_ever_calling_the_planner() {
    let mut complexity = decomposing_complexity(-1.0, 0, 5);
    complexity.decomposition.max_depth = 0;
    let (store, _observer, result) = run_decomposing_prompt(
        template_prompt("hi"),
        OnError::Fail,
        &complexity,
        // One reply only -- if the planner (or anything else) made an
        // extra adapter call, the second pop would panic with "no
        // reply queued" via a failed dispatch instead of this value.
        vec![FakeAdapter::text("ran as is at the ceiling")],
    )
    .await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let big = as_dict(get(prime, "big").unwrap());
    assert_eq!(
        get(big, "value"),
        Some(&Value::Str("ran as is at the ceiling".to_string()))
    );
    let meta = as_dict(get(big, "meta").unwrap());
    let decomposition = as_dict(get(meta, "decomposition").unwrap());
    assert_eq!(
        get(decomposition, "outcome"),
        Some(&Value::Str("run_as_is".to_string()))
    );
    assert_eq!(
        get(decomposition, "reason"),
        Some(&Value::Str("max_depth".to_string()))
    );
    assert_eq!(get(decomposition, "error"), None);
}

#[tokio::test]
async fn a_plan_with_content_this_build_cant_run_is_a_fatal_refusal_never_execution_failed() {
    // `mcp` is Circuitry-known but no electricity builder exists for
    // it -- `first_unsupported` refuses the freshly compiled plan
    // statically (Settled question 6), never recorded as
    // `execution_failed`.
    let plan_yaml = "\
effects:\n\
\x20\x20- type: tool\n\
\x20\x20\x20\x20name: a\n\
\x20\x20\x20\x20provider: mcp\n\
\x20\x20\x20\x20params: {text: \"hi\"}\n\
\x20\x20- type: tool\n\
\x20\x20\x20\x20name: merge\n\
\x20\x20\x20\x20provider: json\n\
\x20\x20\x20\x20params: {mode: parse, input: '\"merged\"'}\n\
";
    let complexity = decomposing_complexity(-1.0, 2, 5);
    let replies = vec![planner_reply(plan_yaml, &["a", "merge"])];
    let (store, _observer, result) = run_decomposing_prompt(
        template_prompt("hi"),
        OnError::Continue,
        &complexity,
        replies,
    )
    .await;
    let err = result.expect_err("an unsupported plan must be a fatal Err, never absorbed");
    match &err {
        VmError::Refused { child_label, .. } => {
            assert_eq!(child_label.as_deref(), Some("decomposition plan of 'big'"));
        }
        other => panic!("expected VmError::Refused, got {other:?}"),
    }
    // Nothing is written to `meta.decomposition` at all for a refused
    // plan -- the store isn't even touched past the pre-dispatch block.
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let big = as_dict(get(prime, "big").unwrap());
    let meta = as_dict(get(big, "meta").unwrap());
    assert_eq!(get(meta, "decomposition"), None);
}

#[tokio::test]
async fn a_dispatch_time_refusal_inside_the_plan_carries_the_decomposition_label() {
    // The plan compiles clean and passes `first_unsupported` (a
    // prompt's own `provider:` isn't a static check, and
    // `validate_plan` doesn't reject it either) -- the refusal only
    // fires once chunk `a` actually dispatches and `is_unbuildable`
    // sees `litellm`, Circuitry-known but unbuilt in this registry
    // (`registry_with_scripted` only registers `scripted`). That
    // refusal's own `child_label` starts `None` (`exec/prompt.rs`'s
    // dispatch-time check names no child document of its own); the
    // plan's run arm must claim it with the same label the static
    // check above uses.
    let plan_yaml = "\
effects:\n\
\x20\x20- type: prompt\n\
\x20\x20\x20\x20name: a\n\
\x20\x20\x20\x20provider: litellm\n\
\x20\x20\x20\x20template: \"chunk a\"\n\
\x20\x20- type: tool\n\
\x20\x20\x20\x20name: merge\n\
\x20\x20\x20\x20provider: json\n\
\x20\x20\x20\x20params: {mode: parse, input: '\"merged\"'}\n\
";
    let complexity = decomposing_complexity(-1.0, 2, 5);
    let replies = vec![planner_reply(plan_yaml, &["a", "merge"])];
    let (store, _observer, result) = run_decomposing_prompt(
        template_prompt("hi"),
        OnError::Continue,
        &complexity,
        replies,
    )
    .await;
    let err = result.expect_err("a dispatch-time refusal inside the plan must be a fatal Err");
    match &err {
        VmError::Refused { child_label, .. } => {
            assert_eq!(child_label.as_deref(), Some("decomposition plan of 'big'"));
        }
        other => panic!("expected VmError::Refused, got {other:?}"),
    }
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let big = as_dict(get(prime, "big").unwrap());
    let meta = as_dict(get(big, "meta").unwrap());
    assert_eq!(get(meta, "decomposition"), None);
}

#[tokio::test]
async fn a_planner_failure_reports_the_raw_payload_for_a_schema_rejection() {
    // A reply missing the required `yaml` key fails the planner
    // prompt's own schema -- `SchemaValidationError`'s bare re-raise
    // carries the whole rejected reply as `ErrorCause::SchemaRejected`,
    // which `maybe_decompose` recovers as `raw_payload`. The planner's
    // own retry loop (`max_iterations: 3`) means the FakeAdapter needs
    // one queued rejection per repair attempt; queue three identical
    // ones so every pass fails the same way and `invalid_plan`/
    // `planner_failed` is reached deterministically either way.
    let mut dict = Dict::new();
    dict.insert(
        Value::Str("say".to_string()),
        Value::Str("oops".to_string()),
    );
    dict.insert(Value::Str("chunks".to_string()), Value::List(vec![]));
    let text =
        electricity_json::dumps(&Value::Dict(dict), electricity_json::WriteMode::COMPACT).unwrap();
    let complexity = decomposing_complexity(-1.0, 2, 5);
    let replies = vec![
        Ok(GenerateResult {
            text: text.clone(),
            raw: Value::None,
            tokens_sent: Some(1),
            tokens_received: Some(1),
            finish_reason: None,
            warnings: Vec::new(),
            cost_usd: None,
        }),
        FakeAdapter::text("route up / run as is fallback"),
    ];
    let (store, _observer, result) = run_decomposing_prompt(
        template_prompt("hi"),
        OnError::Continue,
        &complexity,
        replies,
    )
    .await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let big = as_dict(get(prime, "big").unwrap());
    let meta = as_dict(get(big, "meta").unwrap());
    let decomposition = as_dict(get(meta, "decomposition").unwrap());
    assert_eq!(
        get(decomposition, "reason"),
        Some(&Value::Str("planner_failed".to_string()))
    );
    assert_eq!(get(decomposition, "raw_payload"), Some(&Value::Str(text)));
}

#[tokio::test]
async fn on_failure_fail_raises_decomposition_error_with_the_expected_text_and_class() {
    // An invalid plan (no `merge` effect) under `on_failure: fail`.
    // Two chunks (satisfies the planner's own internal "at least 2"
    // repair-loop gate, so it validates on the first pass) but no
    // top-level effect named `merge` at all -- fails electricity's own
    // *post-planner* merge-contract check (`_check_merge_contract`),
    // never the planner's own internal `check.ok`/chunk-count gate, so
    // exactly one planner reply is consumed and neither chunk prompt
    // ever dispatches (`validate_plan` runs before `execute`).
    let bad_plan_yaml = "\
effects:\n\
\x20\x20- type: prompt\n\
\x20\x20\x20\x20name: a\n\
\x20\x20\x20\x20template: \"a\"\n\
\x20\x20- type: prompt\n\
\x20\x20\x20\x20name: b\n\
\x20\x20\x20\x20template: \"b\"\n\
";
    let mut complexity = decomposing_complexity(-1.0, 2, 5);
    complexity.decomposition.on_failure = "fail".to_string();
    let replies = vec![planner_reply(bad_plan_yaml, &["a", "b"])];
    let (store, _observer, result) =
        run_decomposing_prompt(template_prompt("hi"), OnError::Fail, &complexity, replies).await;
    let err = result.expect_err("on_failure: fail must raise");
    // Wrapped once by the enclosing `prime` dynamic's own `on_error:
    // fail` propagation (`exec::wrap_effect_error`, the same
    // `RuntimeError(...) from e` shape any other effect failure gets)
    // -- `displayed_class`'s own one-level unwrap recovers the
    // original `PyExc::DecompositionError` the same way #442's tree
    // text would.
    assert_eq!(err.displayed_class(), Some(PyExc::DecompositionError));
    match &err {
        VmError::Raised { message, .. } => {
            assert!(
                message.contains("decomposition of 'big' failed (invalid_plan): "),
                "{message}"
            );
        }
        other => panic!("expected VmError::Raised, got {other:?}"),
    }
    // `finish_failure` still ran -- the effect's own `meta.error`
    // carries the same text, and `completed_at` is set.
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let big = as_dict(get(prime, "big").unwrap());
    let meta = as_dict(get(big, "meta").unwrap());
    let error_text = match get(meta, "error").unwrap() {
        Value::Str(s) => s.clone(),
        other => panic!("expected a string, got {other:?}"),
    };
    assert!(
        error_text.starts_with("decomposition of 'big' failed (invalid_plan): "),
        "{error_text}"
    );
    assert_eq!(get(meta, "fallback_attempts"), Some(&Value::List(vec![])));
    assert_eq!(get(meta, "retries_used"), None);
}

#[tokio::test]
async fn on_error_skip_gives_a_null_value_on_decomposition_failure() {
    // Two chunks (satisfies the planner's own internal "at least 2"
    // repair-loop gate, so it validates on the first pass) but no
    // top-level effect named `merge` at all -- fails electricity's own
    // *post-planner* merge-contract check (`_check_merge_contract`),
    // never the planner's own internal `check.ok`/chunk-count gate, so
    // exactly one planner reply is consumed and neither chunk prompt
    // ever dispatches (`validate_plan` runs before `execute`).
    let bad_plan_yaml = "\
effects:\n\
\x20\x20- type: prompt\n\
\x20\x20\x20\x20name: a\n\
\x20\x20\x20\x20template: \"a\"\n\
\x20\x20- type: prompt\n\
\x20\x20\x20\x20name: b\n\
\x20\x20\x20\x20template: \"b\"\n\
";
    let mut complexity = decomposing_complexity(-1.0, 2, 5);
    complexity.decomposition.on_failure = "fail".to_string();
    let replies = vec![planner_reply(bad_plan_yaml, &["a", "b"])];
    let (store, _observer, result) =
        run_decomposing_prompt(template_prompt("hi"), OnError::Skip, &complexity, replies).await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let big = as_dict(get(prime, "big").unwrap());
    assert_eq!(get(big, "value"), Some(&Value::None));
}

#[tokio::test]
async fn route_up_on_failure_ignores_a_run_level_model_lock() {
    // Settled questions 2/3/4: a profile's run-level `model:` locks
    // the run (`model_locked: true`), so K2's own router defers for
    // `big` itself (`model_reason` stays `"default"`, the band still
    // recorded) -- but `decompose::route_up_model` never reads that
    // lock at all, so an invalid plan's own `on_failure: route_up`
    // fallback still dispatches on the catch-all band's model, which
    // reaches `build_attempts` and `fallback_attempts[0].model`.
    // Two chunks (satisfies the planner's own internal "at least 2"
    // repair-loop gate, so it validates on the first pass) but no
    // top-level effect named `merge` at all -- fails electricity's own
    // *post-planner* merge-contract check (`_check_merge_contract`),
    // never the planner's own internal `check.ok`/chunk-count gate, so
    // exactly one planner reply is consumed and neither chunk prompt
    // ever dispatches (`validate_plan` runs before `execute`).
    let bad_plan_yaml = "\
effects:\n\
\x20\x20- type: prompt\n\
\x20\x20\x20\x20name: a\n\
\x20\x20\x20\x20template: \"a\"\n\
\x20\x20- type: prompt\n\
\x20\x20\x20\x20name: b\n\
\x20\x20\x20\x20template: \"b\"\n\
";
    let mut complexity = decomposing_complexity(-1.0, 2, 5);
    complexity.routing.enabled = true;
    complexity.routing.bands = vec![electricity_config::ComplexityBand {
        model: "catch-all-model".to_string(),
        max: None,
        name: None,
    }];
    let replies = vec![
        planner_reply(bad_plan_yaml, &["a", "b"]),
        FakeAdapter::text("routed up"),
    ];

    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    ctx.default_adapter = Rc::new(FakeAdapter::new("scripted", replies));
    let adapters = registry_with_scripted();
    ctx.adapters = &adapters;
    ctx.model = "m";
    ctx.model_locked = true;
    ctx.complexity = &complexity;
    ctx.supported = electricity_bytecode::Supported::current(&[]);
    let leaf = prompt_leaf(
        root_path().push_name("big"),
        "big",
        template_prompt("hi"),
        OnError::Fail,
    );
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![leaf]);
    let observer = RecordingObserver::new();
    electricity_vm::execute_root(
        &program(root),
        &store,
        &Value::None,
        &ctx,
        &observer,
        &token,
    )
    .await
    .unwrap();

    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let big = as_dict(get(prime, "big").unwrap());
    assert_eq!(
        get(big, "value"),
        Some(&Value::Str("routed up".to_string()))
    );
    let meta = as_dict(get(big, "meta").unwrap());
    // The lock means K2's own router deferred for `big` itself.
    assert_eq!(
        get(meta, "model_reason"),
        Some(&Value::Str("default".to_string()))
    );
    assert!(
        get(meta, "complexity")
            .and_then(Value::as_dict)
            .and_then(|c| c.get(&Value::Str("band".to_string())))
            .is_some(),
        "the band is still recorded even though the router didn't act: {meta:?}"
    );
    let decomposition = as_dict(get(meta, "decomposition").unwrap());
    assert_eq!(
        get(decomposition, "fallback_model"),
        Some(&Value::Str("catch-all-model".to_string()))
    );
    let attempts = match get(meta, "fallback_attempts").unwrap() {
        Value::List(items) => items.clone(),
        other => panic!("expected a list, got {other:?}"),
    };
    assert_eq!(attempts.len(), 1);
    let attempt = as_dict(&attempts[0]);
    assert_eq!(
        get(attempt, "model"),
        Some(&Value::Str("catch-all-model".to_string()))
    );
    // A successful dispatch overwrites `meta.model` with the winning
    // attempt's own model, exactly as it would for any other prompt.
    assert_eq!(
        get(meta, "model"),
        Some(&Value::Str("catch-all-model".to_string()))
    );
}

#[tokio::test]
async fn a_cancellation_mid_planner_propagates_and_writes_no_decomposition_record() {
    let complexity = decomposing_complexity(-1.0, 2, 5);
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    // No replies queued at all -- the planner's own `plan` prompt never
    // gets a chance to pop one: cancellation fires the instant that
    // effect's own `effect_start` is observed
    // (`support::CancelOnStart`), so the dispatch itself races
    // `token.cancelled()` and loses.
    ctx.default_adapter = Rc::new(FakeAdapter::new("scripted", Vec::new()));
    let adapters = registry_with_scripted();
    ctx.adapters = &adapters;
    ctx.model = "m";
    ctx.complexity = &complexity;
    ctx.supported = electricity_bytecode::Supported::current(&[]);
    let leaf = prompt_leaf(
        root_path().push_name("big"),
        "big",
        template_prompt("hi"),
        OnError::Fail,
    );
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![leaf]);
    let observer = support::CancelOnStart {
        inner: RecordingObserver::new(),
        token: &token,
        target: "prime.big.plan",
    };
    let result = electricity_vm::execute_root(
        &program(root),
        &store,
        &Value::None,
        &ctx,
        &observer,
        &token,
    )
    .await;
    assert!(matches!(result, Err(VmError::Cancelled)), "{result:?}");
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let big = as_dict(get(prime, "big").unwrap());
    let meta = as_dict(get(big, "meta").unwrap());
    assert_eq!(get(meta, "decomposition"), None);
}
