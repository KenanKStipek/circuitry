//! M1 lane L1 (issue #506): `reflector` effect execution -- integration
//! tests against a real, compiled-by-hand document (`execute_root`),
//! the same convention `tests/prompt_effect.rs`/`tests/use_effect.rs`
//! use. The inner planner prompt runs through a fake adapter (this
//! file's own copy of `prompt_effect.rs`'s `FakeAdapter`, a queued-
//! reply stand-in for the scripted adapter); the generated plan is an
//! ordinary `tool: json` effect, so these tests need only one queued
//! model reply per iteration.

mod support;

use async_trait::async_trait;
use electricity_bytecode::effects::{PromptContent, PromptOp, PromptType, ReflectorOp};
use electricity_bytecode::template::{Escape, TemplateText};
use electricity_bytecode::{EffectPath, LeafKind, NodeKind, OnError, Op, Region};
use electricity_value::Value;
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

struct FakeAdapter {
    name: String,
    replies: std::cell::RefCell<std::collections::VecDeque<Result<GenerateResult, AdapterError>>>,
}

impl FakeAdapter {
    fn new(name: &str, replies: Vec<&str>) -> Self {
        FakeAdapter {
            name: name.to_string(),
            replies: std::cell::RefCell::new(replies.into_iter().map(FakeAdapter::text).collect()),
        }
    }

    fn text(text: &str) -> Result<GenerateResult, AdapterError> {
        Ok(GenerateResult {
            text: text.to_string(),
            raw: Value::None,
            tokens_sent: None,
            tokens_received: None,
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
        Box::new(|_cfg| Ok(Box::new(FakeAdapter::new("scripted", Vec::new())) as Box<dyn Adapter>)),
    );
    adapters
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

fn plan_prompt(name: &str) -> Op {
    Op {
        path: root_path().push_name("plan_it").push_name(name),
        name: Some(name.to_string()),
        kind: NodeKind::Leaf(Box::new(LeafKind::Prompt(PromptOp {
            content: PromptContent::Template(TemplateText::new("", true, Escape::None)),
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
        }))),
        on_error: OnError::Fail,
        labels: None,
        enabled: true,
    }
}

#[allow(clippy::too_many_arguments)]
fn reflector_leaf(
    name: &str,
    plan_from_step: &str,
    max_iterations: u32,
    generated_key: &str,
    stop_on_done: bool,
    prime_template: &str,
    max_effects: u32,
) -> Op {
    let op = ReflectorOp {
        inner: Box::new(Region::Block {
            ops: vec![plan_prompt(plan_from_step)],
            overlay: false,
        }),
        plan_from_step: plan_from_step.to_string(),
        max_iterations,
        generated_key: generated_key.to_string(),
        stop_on_done,
        prime_template: TemplateText::new(prime_template, false, Escape::None),
        max_effects,
    };
    Op {
        path: root_path().push_name(name),
        name: Some(name.to_string()),
        kind: NodeKind::Leaf(Box::new(LeafKind::Reflector(op))),
        on_error: OnError::Fail,
        labels: None,
        enabled: true,
    }
}

async fn run_reflector(
    reflector_op: Op,
    replies: Vec<&str>,
) -> (Store, RecordingObserver, Result<(), VmError>) {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    let adapters = registry_with_scripted();
    ctx.default_adapter = Rc::new(FakeAdapter::new("scripted", replies));
    ctx.adapters = &adapters;
    ctx.model = "m";
    ctx.supported = electricity_bytecode::Supported::current(&[]);

    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![reflector_op]);
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
async fn a_basic_plan_runs_through_the_generated_use_child() {
    let plan_reply = "done: false\n\
         effects:\n  \
           - type: tool\n    \
             name: check\n    \
             provider: json\n    \
             params:\n      \
               mode: parse\n      \
               input: '{\"ok\": true}'\n";
    let leaf = reflector_leaf(
        "plan_it",
        "propose_steps",
        1,
        "generated",
        true,
        "plan it",
        8,
    );
    let (store, observer, result) = run_reflector(leaf, vec![plan_reply]).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let refl = as_dict(get(prime, "plan_it").unwrap());
    assert_eq!(get(refl, "value"), Some(&Value::Bool(true)));

    let meta = as_dict(get(refl, "meta").unwrap());
    let iterations = match get(meta, "iterations").unwrap() {
        Value::List(items) => items,
        other => panic!("expected a list, got {other:?}"),
    };
    assert_eq!(iterations.len(), 1);
    let rec = as_dict(&iterations[0]);
    assert_eq!(get(rec, "done"), Some(&Value::Bool(false)));
    assert_eq!(get(rec, "stop"), Some(&Value::Bool(false)));
    assert_eq!(get(rec, "error"), Some(&Value::None));

    // The inner planner prompt wrote its own node under `plan_it.inner`,
    // the `Frame::rebased` run-time path -- never `plan_it.propose_steps`.
    let inner = as_dict(get(refl, "inner").unwrap());
    assert!(get(inner, "propose_steps").is_some());

    // The generated plan actually ran, under `plan_it.generated.iter_0`.
    let generated = as_dict(get(refl, "generated").unwrap());
    let iter0 = as_dict(get(generated, "iter_0").unwrap());
    let check = as_dict(get(iter0, "check").unwrap());
    assert_eq!(get(check, "value"), Some(&Value::Dict(check_value_ok())));

    assert!(
        observer
            .events()
            .iter()
            .any(|e| matches!(e, Event::Start(p) if p == "prime.plan_it.inner.propose_steps"))
    );
    assert!(
        observer
            .events()
            .iter()
            .any(|e| matches!(e, Event::Complete(p, None) if p == "prime.plan_it"))
    );
}

fn check_value_ok() -> IndexMap<Value, Value> {
    let mut d = IndexMap::new();
    d.insert(Value::Str("ok".to_string()), Value::Bool(true));
    d
}

#[tokio::test]
async fn done_true_with_stop_on_done_never_runs_the_generated_plan() {
    let plan_reply = "done: true\neffects:\n  - type: tool\n    name: check\n    provider: json\n";
    let leaf = reflector_leaf(
        "plan_it",
        "propose_steps",
        1,
        "generated",
        true,
        "plan it",
        8,
    );
    let (store, _observer, result) = run_reflector(leaf, vec![plan_reply]).await;
    result.unwrap();

    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let refl = as_dict(get(prime, "plan_it").unwrap());
    assert_eq!(get(refl, "value"), Some(&Value::Bool(true)));

    let generated = as_dict(get(refl, "generated").unwrap());
    assert!(
        generated.is_empty(),
        "no plan should have run, got {generated:?}"
    );

    let meta = as_dict(get(refl, "meta").unwrap());
    // Success never writes created_at/completed_at.
    assert!(get(meta, "created_at").is_none());
    assert!(get(meta, "completed_at").is_none());
}

#[tokio::test]
async fn an_oversized_plan_fails_with_the_exact_invalid_plan_text() {
    let plan_reply = "done: false\neffects:\n  \
        - {type: tool, name: a, provider: json}\n  \
        - {type: tool, name: b, provider: json}\n  \
        - {type: tool, name: c, provider: json}\n";
    let leaf = reflector_leaf(
        "plan_it",
        "propose_steps",
        1,
        "generated",
        true,
        "plan it",
        2,
    );
    let (store, _observer, result) = run_reflector(leaf, vec![plan_reply]).await;
    let err = result.unwrap_err();
    assert_eq!(
        err.to_string(),
        "prime.plan_it: invalid_plan: generated plan has 3 top-level effects, exceeds max_effects (2)"
    );

    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let refl = as_dict(get(prime, "plan_it").unwrap());
    assert_eq!(get(refl, "value"), Some(&Value::Bool(false)));
    let meta = as_dict(get(refl, "meta").unwrap());
    assert_eq!(
        get(meta, "error"),
        Some(&Value::Str(
            "invalid_plan: generated plan has 3 top-level effects, exceeds max_effects (2)"
                .to_string()
        ))
    );
    assert!(get(meta, "completed_at").is_some());
}
