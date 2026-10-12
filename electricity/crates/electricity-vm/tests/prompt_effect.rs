//! M1 lane G (issue #498): `prompt` effect execution -- integration
//! tests against a real, compiled document (`execute_root`, this
//! crate's own one `pub` entry point), the same convention `tests/
//! use_effect.rs` uses.

mod support;

use async_trait::async_trait;
use electricity_bytecode::effects::{Message, PromptContent, PromptOp, PromptType, Role};
use electricity_bytecode::template::{Escape, TemplateText};
use electricity_bytecode::{EffectPath, LeafKind, NodeKind, OnError, Op, ParamNode, RetryPolicy};
use electricity_value::{Dict, Value};
use electricity_vm::adapter::{
    Adapter, AdapterError, AdapterRegistry, CallContext, CheckResult, GenerateOptions,
    GenerateResult, PyExc,
};
use electricity_vm::retry::RetryInfo;
use electricity_vm::{CancellationToken, Limiter, Store, VmError};
use indexmap::IndexMap;
use std::rc::Rc;
use support::{RecordingObserver, chain_dynamic, json_registry, program, run_ctx};

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

/// A scripted-like fake: answers every call from a fixed queue of
/// texts, in order -- enough for these tests without this crate
/// depending on `electricity-adapters` (which depends on it the other
/// way already).
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

/// A placeholder adapter with a given (Python-known) name whose own
/// `generate` is never expected to run -- this crate's own analogue of
/// `electricity` (lib)'s `preflight::PlaceholderAdapter`, built here so
/// this test doesn't depend on the top `electricity` crate (which
/// depends on `electricity-vm`, not the other way).
struct NamedUnbuiltAdapter {
    name: String,
}

#[async_trait(?Send)]
impl Adapter for NamedUnbuiltAdapter {
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
        panic!(
            "NamedUnbuiltAdapter::generate must never run -- the dispatch-time refusal must intercept it first"
        )
    }

    async fn check(&self) -> CheckResult {
        CheckResult {
            ok: true,
            ..Default::default()
        }
    }
}

/// A registry with a trivial `scripted` builder registered -- every
/// test whose own default/fallback adapter is literally named
/// `scripted` (this crate's own `is_unbuildable` check runs by name
/// against `electricity_vm::adapter_names::ADAPTER_NAMES`, which
/// includes `scripted`, regardless of which concrete `Adapter` a test
/// actually hands `run_prompt` as the default) needs this, not a bare
/// `AdapterRegistry::new()`.
fn registry_with_scripted() -> AdapterRegistry {
    let mut adapters = AdapterRegistry::new();
    adapters.register(
        "scripted",
        Box::new(|_cfg| Ok(Box::new(FakeAdapter::new("scripted", Vec::new())) as Box<dyn Adapter>)),
    );
    adapters
}

async fn run_prompt(
    op: PromptOp,
    on_error: OnError,
    default_adapter: Rc<dyn Adapter>,
    adapters: &AdapterRegistry,
) -> (Store, RecordingObserver, Result<(), VmError>) {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    ctx.default_adapter = default_adapter;
    ctx.adapters = adapters;
    ctx.model = "m";
    let leaf = prompt_leaf(root_path().push_name("ask"), "ask", op, on_error);
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
async fn a_successful_prompt_writes_value_and_meta() {
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![FakeAdapter::text("hello")],
    ));
    let adapters = registry_with_scripted();
    let (store, observer, result) = run_prompt(
        template_prompt("hi there"),
        OnError::Fail,
        default,
        &adapters,
    )
    .await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    assert_eq!(get(ask, "value"), Some(&Value::Str("hello".to_string())));
    let meta = as_dict(get(ask, "meta").unwrap());
    assert_eq!(
        get(meta, "prompt_sent"),
        Some(&Value::Str("hi there".to_string()))
    );
    assert_eq!(
        get(meta, "model_reason"),
        Some(&Value::Str("default".to_string()))
    );
    assert!(
        observer
            .events()
            .iter()
            .any(|e| matches!(e, support::Event::Complete(p, None) if p == "prime.ask"))
    );
}

#[tokio::test]
async fn a_dispatch_against_an_unbuildable_known_adapter_is_a_fatal_refusal() {
    // The default adapter's own name is a Circuitry-known one
    // (`electricity_vm::adapter_names::ADAPTER_NAMES` includes
    // "openai") that this test's own, deliberately empty
    // `AdapterRegistry` has no builder for -- exactly
    // `preflight::PlaceholderAdapter`'s own shape, reached only
    // through a `use` child in a real run (issue #498 ruling 4); here
    // reached directly, since this crate's own `execute_root` has no
    // concept of "inside a use child" at all.
    let default = Rc::new(NamedUnbuiltAdapter {
        name: "openai".to_string(),
    });
    let adapters = AdapterRegistry::new();
    let (store, observer, result) =
        run_prompt(template_prompt("hi"), OnError::Continue, default, &adapters).await;
    let VmError::Refused {
        refusal,
        child_label,
    } = result.unwrap_err()
    else {
        panic!("expected VmError::Refused");
    };
    assert_eq!(child_label, None);
    assert_eq!(
        refusal.reason,
        electricity_bytecode::RefusalReason::UnsupportedAdapter("openai".to_string())
    );
    // Never absorbed by `on_error` (`OnError::Continue` above would
    // otherwise swallow an ordinary failure) and never recorded as an
    // ordinary attempt: no `meta.error`/`completed_at` write at all.
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    let meta = as_dict(get(ask, "meta").unwrap());
    assert_eq!(get(meta, "completed_at"), Some(&Value::None));
    assert!(
        !observer
            .events()
            .iter()
            .any(|e| matches!(e, support::Event::Complete(p, _) if p == "prime.ask")),
        "a fatal refusal must never fire effect_complete for the prompt's own node"
    );
}

#[tokio::test]
async fn a_non_default_unbuildable_fallback_is_also_a_fatal_refusal() {
    let mut op = template_prompt("hi");
    op.provider_fallbacks = vec!["anthropic".to_string()];
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![Err(AdapterError {
            message: "scripted: failed".to_string(),
            retry: RetryInfo::retryable(),
            py_class: PyExc::AdapterCallError,
        })],
    ));
    let adapters = registry_with_scripted();
    let (_store, _observer, result) = run_prompt(op, OnError::Fail, default, &adapters).await;
    let VmError::Refused { refusal, .. } = result.unwrap_err() else {
        panic!("expected VmError::Refused");
    };
    assert_eq!(
        refusal.reason,
        electricity_bytecode::RefusalReason::UnsupportedAdapter("anthropic".to_string())
    );
}

#[tokio::test]
async fn on_error_continue_clears_value_and_keeps_running() {
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![Err(AdapterError {
            message: "boom".to_string(),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        })],
    ));
    let adapters = registry_with_scripted();
    let (store, _observer, result) =
        run_prompt(template_prompt("hi"), OnError::Continue, default, &adapters).await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    assert_eq!(get(ask, "value"), Some(&Value::None));
    let meta = as_dict(get(ask, "meta").unwrap());
    assert!(
        get(meta, "error")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("boom")
    );
}

#[tokio::test]
async fn retries_retry_the_whole_chain_and_record_retries_used() {
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![
            Err(AdapterError {
                message: "scripted: server_error".to_string(),
                retry: RetryInfo::retryable(),
                py_class: PyExc::AdapterCallError,
            }),
            FakeAdapter::text("recovered"),
        ],
    ));
    let adapters = registry_with_scripted();
    let mut op = template_prompt("hi");
    op.retries = Some(RetryPolicy {
        max_attempts: 2,
        backoff_ms: 1,
    });
    let (store, _observer, result) = run_prompt(op, OnError::Fail, default, &adapters).await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    assert_eq!(
        get(ask, "value"),
        Some(&Value::Str("recovered".to_string()))
    );
    let meta = as_dict(get(ask, "meta").unwrap());
    assert_eq!(get(meta, "retries_used"), Some(&Value::from(1i64)));
}

#[tokio::test]
async fn a_schema_decode_failure_is_retried_down_then_across_passes() {
    let mut schema = Dict::new();
    schema.insert(
        Value::Str("type".to_string()),
        Value::Str("object".to_string()),
    );
    schema.insert(
        Value::Str("required".to_string()),
        Value::List(vec![Value::Str("a".to_string())]),
    );
    let mut op = template_prompt("give json");
    op.prompt_type = PromptType::Json;
    op.schema = Some(Value::Dict(schema));
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![FakeAdapter::text("not json")],
    ));
    let adapters = registry_with_scripted();
    let (store, _observer, result) = run_prompt(op, OnError::Fail, default, &adapters).await;
    let err = result.unwrap_err();
    // The bare schema text, not the "All adapter attempts failed"
    // wrapper -- issue #498's own C14.
    assert!(
        err.to_string()
            .contains("JSON schema validation failed: model returned None")
    );
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    let meta = as_dict(get(ask, "meta").unwrap());
    let attempts = match get(meta, "fallback_attempts").unwrap() {
        Value::List(items) => items,
        other => panic!("expected a list, got {other:?}"),
    };
    assert_eq!(attempts.len(), 1);
    let attempt = as_dict(&attempts[0]);
    assert_eq!(
        get(attempt, "status"),
        Some(&Value::Str("schema_invalid".to_string()))
    );
}

#[tokio::test]
async fn a_bad_params_option_fails_before_any_attempt() {
    let mut op = template_prompt("hi");
    let mut params = Dict::new();
    params.insert(Value::Str("max_tokens".to_string()), Value::from(0i64));
    op.model_params = Some(ParamNode::Literal(Value::Dict(params)));
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![FakeAdapter::text("unused")],
    ));
    let adapters = registry_with_scripted();
    let (store, _observer, result) = run_prompt(op, OnError::Continue, default, &adapters).await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    let meta = as_dict(get(ask, "meta").unwrap());
    assert_eq!(
        get(meta, "error"),
        Some(&Value::Str(
            "Prompt 'ask': params.max_tokens must be a positive integer, got 0".to_string()
        ))
    );
    assert_eq!(
        get(meta, "fallback_attempts"),
        Some(&Value::List(Vec::new()))
    );
}

#[tokio::test]
async fn messages_flatten_into_the_prompt_sent_role_content_form() {
    let op = PromptOp {
        content: PromptContent::Messages(vec![
            Message {
                role: Role::System,
                content: TemplateText::new("be nice", true, Escape::None),
            },
            Message {
                role: Role::User,
                content: TemplateText::new("hi", true, Escape::None),
            },
        ]),
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
    };
    let default = Rc::new(FakeAdapter::new("scripted", vec![FakeAdapter::text("ok")]));
    let adapters = registry_with_scripted();
    let (store, _observer, result) = run_prompt(op, OnError::Fail, default, &adapters).await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    let meta = as_dict(get(ask, "meta").unwrap());
    assert_eq!(
        get(meta, "prompt_sent"),
        Some(&Value::Str("system: be nice\n\nuser: hi".to_string()))
    );
}

#[tokio::test]
async fn inputs_overlay_the_render_context_raw() {
    let mut inputs = Dict::new();
    inputs.insert(
        Value::Str("name".to_string()),
        Value::Str("Ada".to_string()),
    );
    let mut op = template_prompt("hi {{name}}");
    op.inputs = Some(ParamNode::Literal(Value::Dict(inputs)));
    let default = Rc::new(FakeAdapter::new("scripted", vec![FakeAdapter::text("ok")]));
    let adapters = registry_with_scripted();
    let (store, _observer, result) = run_prompt(op, OnError::Fail, default, &adapters).await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    let meta = as_dict(get(ask, "meta").unwrap());
    assert_eq!(
        get(meta, "prompt_sent"),
        Some(&Value::Str("hi Ada".to_string()))
    );
}
