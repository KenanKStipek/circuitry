//! M1 lane G (issue #498): `prompt` effect execution -- integration
//! tests against a real, compiled document (`execute_root`, this
//! crate's own one `pub` entry point), the same convention `tests/
//! use_effect.rs` uses.

mod support;

use async_trait::async_trait;
use electricity_bytecode::effects::{Message, PromptContent, PromptOp, PromptType, Role};
use electricity_bytecode::template::{Escape, TemplateText};
use electricity_bytecode::{
    EffectPath, LeafKind, NodeKind, OnError, Op, ParamNode, RetryPolicy, RoutingOverride, UseSource,
};
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

/// A scripted-like fake: answers every call from a fixed queue of
/// texts, in order -- enough for these tests without this crate
/// depending on `electricity-adapters` (which depends on it the other
/// way already).
struct FakeAdapter {
    name: String,
    replies: std::cell::RefCell<std::collections::VecDeque<Result<GenerateResult, AdapterError>>>,
    /// Every prompt text `generate` received, in call order.
    received: std::cell::RefCell<Vec<String>>,
    /// Runs at the start of every `generate` call.
    on_generate: std::cell::RefCell<Option<Box<dyn Fn()>>>,
}

impl FakeAdapter {
    fn new(name: &str, replies: Vec<Result<GenerateResult, AdapterError>>) -> Self {
        FakeAdapter {
            name: name.to_string(),
            replies: std::cell::RefCell::new(replies.into()),
            received: std::cell::RefCell::new(Vec::new()),
            on_generate: std::cell::RefCell::new(None),
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
        prompt: &str,
        _options: &GenerateOptions,
        _call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        self.received.borrow_mut().push(prompt.to_string());
        if let Some(hook) = self.on_generate.borrow().as_ref() {
            hook();
        }
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
    run_prompt_with_runtime(op, on_error, default_adapter, adapters, &Value::None).await
}

/// [`run_prompt`] under *runtime_config* (the merged `runtime:` block).
async fn run_prompt_with_runtime(
    op: PromptOp,
    on_error: OnError,
    default_adapter: Rc<dyn Adapter>,
    adapters: &AdapterRegistry,
    runtime_config: &Value,
) -> (Store, RecordingObserver, Result<(), VmError>) {
    run_prompt_in_ctx(
        op,
        on_error,
        default_adapter,
        adapters,
        runtime_config,
        &Value::None,
    )
    .await
}

/// [`run_prompt_with_runtime`] with *ctx* as the document's rendering
/// context (what `input.<name>` reads).
async fn run_prompt_in_ctx(
    op: PromptOp,
    on_error: OnError,
    default_adapter: Rc<dyn Adapter>,
    adapters: &AdapterRegistry,
    runtime_config: &Value,
    ctx_value: &Value,
) -> (Store, RecordingObserver, Result<(), VmError>) {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    ctx.default_adapter = default_adapter;
    ctx.adapters = adapters;
    ctx.model = "m";
    ctx.runtime_config = runtime_config;
    let leaf = prompt_leaf(root_path().push_name("ask"), "ask", op, on_error);
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![leaf]);
    let observer = RecordingObserver::new();
    let result =
        electricity_vm::execute_root(&program(root), &store, ctx_value, &ctx, &observer, &token)
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
async fn an_answer_parse_error_carries_no_raw_payload_cause() {
    // Issue #522 §3: `ErrorCause::SchemaRejected` is only ever set for
    // a bare `SchemaValidationError` re-raise -- never for
    // `AnswerParseError` (`decode_output`'s own failure for a
    // `number` prompt given a non-numeric reply).
    let mut op = template_prompt("give a number");
    op.prompt_type = PromptType::Number;
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![FakeAdapter::text("not a number")],
    ));
    let adapters = registry_with_scripted();
    let (_store, _observer, result) = run_prompt(op, OnError::Fail, default, &adapters).await;
    let VmError::Raised {
        cause_class, cause, ..
    } = result.unwrap_err()
    else {
        panic!("expected VmError::Raised");
    };
    assert_eq!(cause_class, Some(PyExc::AnswerParseError));
    assert_eq!(cause, electricity_tools::ErrorCause::None);
}

#[tokio::test]
async fn an_empty_reply_carries_no_raw_payload_cause_even_under_a_schema_rejection() {
    // The same `SchemaValidationError`/"model returned None" path
    // `a_schema_decode_failure_is_retried_down_then_across_passes`
    // exercises with a non-empty reply, but with an empty one -- the
    // non-empty-reply guard in `exec/prompt.rs` must still exclude it.
    let mut schema = Dict::new();
    schema.insert(
        Value::Str("type".to_string()),
        Value::Str("object".to_string()),
    );
    let mut op = template_prompt("give json");
    op.prompt_type = PromptType::Json;
    op.schema = Some(Value::Dict(schema));
    let default = Rc::new(FakeAdapter::new("scripted", vec![FakeAdapter::text("")]));
    let adapters = registry_with_scripted();
    let (_store, _observer, result) = run_prompt(op, OnError::Fail, default, &adapters).await;
    let VmError::Raised {
        cause_class, cause, ..
    } = result.unwrap_err()
    else {
        panic!("expected VmError::Raised");
    };
    assert_eq!(cause_class, Some(PyExc::SchemaValidationError));
    assert_eq!(cause, electricity_tools::ErrorCause::None);
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
                file: None,
            },
            Message {
                role: Role::User,
                content: TemplateText::new("hi", true, Escape::None),
                file: None,
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

/// A render-failure trigger, mirroring P's own `yield-render-failure-
/// continue` case: `n` is bound to a plain int, and `{{n.0}}` tries to
/// index it like a list, which fails to render
/// (`'int' object is not subscriptable`).
fn subscripted_int_prompt() -> PromptOp {
    let mut inputs = Dict::new();
    inputs.insert(Value::Str("n".to_string()), Value::from(5i64));
    let mut op = template_prompt("{{n.0}}");
    op.inputs = Some(ParamNode::Literal(Value::Dict(inputs)));
    op
}

/// A reply carrying *finish_reason* and *warnings*, beyond
/// [`FakeAdapter::text`]'s own plain-text shape -- for the warning-
/// order and reused-node key-order tests below.
fn reply_with(
    text: &str,
    finish_reason: Option<&str>,
    warnings: Vec<&str>,
    tokens_sent: i64,
    tokens_received: i64,
) -> Result<GenerateResult, AdapterError> {
    Ok(GenerateResult {
        text: text.to_string(),
        raw: Value::None,
        tokens_sent: Some(tokens_sent),
        tokens_received: Some(tokens_received),
        finish_reason: finish_reason.map(str::to_string),
        warnings: warnings.into_iter().map(str::to_string).collect(),
        cost_usd: None,
    })
}

/// Issue #498 finding 1 (review of PR #502): a `use` child's own
/// `record_totals` calls must reach the parent run's totals too, the
/// same way `effect_start`/`effect_complete`/`write` already do
/// (`exec::use_::ChildObserver`'s own forwarding, before this fix the
/// trait's default no-op silently swallowed every call from inside a
/// child).
#[tokio::test]
async fn a_use_childs_prompt_totals_are_forwarded_to_the_parent_observer() {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![FakeAdapter::text("hello")],
    ));
    ctx.default_adapter = default;
    let adapters = registry_with_scripted();
    ctx.adapters = &adapters;
    ctx.model = "m";
    ctx.supported = electricity_bytecode::Supported::current(&[]);

    let inline_yaml = "\
effects:
  - type: prompt
    name: ask
    template: hi
";
    let use_op = electricity_bytecode::UseOp {
        source: UseSource::Inline(TemplateText::new(
            inline_yaml.to_string(),
            true,
            Escape::Html,
        )),
        inputs: None,
        outputs: None,
        validate: true,
        retries: RetryPolicy::default(),
        expect: None,
        description: None,
    };
    let op = support::use_leaf(
        root_path().push_name("step"),
        "step",
        use_op.source,
        use_op.inputs,
        use_op.outputs,
        use_op.validate,
        use_op.retries,
        use_op.expect,
        OnError::Fail,
    );
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![op]);
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
    .expect("a use child's own successful prompt must not refuse");

    assert!(
        observer
            .events()
            .iter()
            .any(|e| matches!(e, Event::Totals(_))),
        "the parent observer must see a record_totals call from inside the use child"
    );
}

/// Issue #498 finding 2 (review of PR #502): a render failure must
/// also call `record_totals` before `effect_complete` -- on a reused
/// node, a render failure must not leave the run's totals double-
/// counting (or under-counting) an earlier pass's tokens (ruling 7).
#[tokio::test]
async fn a_render_failure_also_calls_record_totals_before_effect_complete() {
    let op = subscripted_int_prompt();
    let default = Rc::new(FakeAdapter::new("scripted", vec![]));
    let adapters = registry_with_scripted();
    let (_store, observer, result) = run_prompt(op, OnError::Continue, default, &adapters).await;
    result.unwrap();
    assert!(
        observer
            .events()
            .iter()
            .any(|e| matches!(e, Event::Totals(_))),
        "a render failure must still call record_totals, same as every other meta write \
         that reaches effect_complete: {:?}",
        observer.events()
    );
}

/// Issue #498 finding 3 (review of PR #502): `_record_reply` starts
/// from the reply's own warnings and appends the truncation warning
/// *after* them (`prompt.py:1225-1238`) -- not the other way around.
#[tokio::test]
async fn the_truncation_warning_is_appended_after_the_replys_own_warnings() {
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![reply_with(
            "partial",
            Some("length"),
            vec!["adapter 'scripted' ignored: max_tokens"],
            1,
            1,
        )],
    ));
    let adapters = registry_with_scripted();
    let mut op = template_prompt("hi");
    let mut params = Dict::new();
    params.insert(Value::Str("max_tokens".to_string()), Value::from(5i64));
    op.model_params = Some(ParamNode::Literal(Value::Dict(params)));
    let (store, _observer, result) = run_prompt(op, OnError::Fail, default, &adapters).await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    let meta = as_dict(get(ask, "meta").unwrap());
    let warnings = match get(meta, "warnings").unwrap() {
        Value::List(items) => items.clone(),
        other => panic!("expected a list, got {other:?}"),
    };
    assert_eq!(
        warnings,
        vec![
            Value::Str("adapter 'scripted' ignored: max_tokens".to_string()),
            Value::Str(
                "the reply was cut off by the length limit (finish_reason: length); raise \
                 params.max_tokens"
                    .to_string()
            ),
        ]
    );
}

/// `AllowlistError` propagates without being recorded as an ordinary
/// attempt, and the outer `except` records `fallback_attempts` as the
/// *previous* pass's list (`[]` on the first pass) -- this pass's own
/// partial list, with the denied entry never appended to it, is lost
/// (`prompt.py:1291-1298`; issue #498's own test plan).
#[tokio::test]
async fn an_allowlist_denial_mid_chain_records_the_previous_passs_attempt_list() {
    let mut op = template_prompt("hi");
    op.provider_fallbacks = vec!["other".to_string()];
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![Err(AdapterError {
            message: "scripted: server_error".to_string(),
            retry: RetryInfo::retryable(),
            py_class: PyExc::AdapterCallError,
        })],
    ));
    let adapters = registry_with_scripted();
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    ctx.default_adapter = default;
    ctx.adapters = &adapters;
    ctx.model = "m";
    // `other` isn't on the allowlist -- `require_adapter` denies it
    // before any build is attempted.
    let mut allowlist = Dict::new();
    let mut allowed = Dict::new();
    allowed.insert(
        Value::Str("adapters".to_string()),
        Value::List(vec![Value::Str("scripted".to_string())]),
    );
    allowlist.insert(Value::Str("_allowlists".to_string()), Value::Dict(allowed));
    let runtime_config = Value::Dict(allowlist);
    ctx.runtime_config = &runtime_config;
    let leaf = prompt_leaf(root_path().push_name("ask"), "ask", op, OnError::Continue);
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
    let ask = as_dict(get(prime, "ask").unwrap());
    let meta = as_dict(get(ask, "meta").unwrap());
    // `require_adapter` denies `other` before any build is attempted
    // -- the pass aborts on the allowlist text itself, not the default
    // attempt's own (recorded-then-discarded) failure.
    assert!(
        get(meta, "error")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("not in enabled_adapters allowlist")
    );
    // The previous pass's list -- empty, since this is the only pass:
    // the denial aborts it entirely, so `fallback_attempts` is the
    // *last completed* pass's list, and none completed.
    assert_eq!(
        get(meta, "fallback_attempts"),
        Some(&Value::List(Vec::new()))
    );
}

/// #421: `retries_used` is set on a *failed* node too, not only a
/// successful one, before `effect_complete`.
#[tokio::test]
async fn retries_used_is_set_on_a_failed_node_too() {
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![
            Err(AdapterError {
                message: "scripted: server_error".to_string(),
                retry: RetryInfo::retryable(),
                py_class: PyExc::AdapterCallError,
            }),
            Err(AdapterError {
                message: "scripted: server_error".to_string(),
                retry: RetryInfo::retryable(),
                py_class: PyExc::AdapterCallError,
            }),
        ],
    ));
    let adapters = registry_with_scripted();
    let mut op = template_prompt("hi");
    op.retries = Some(RetryPolicy {
        max_attempts: 2,
        backoff_ms: 1,
    });
    let (store, _observer, result) = run_prompt(op, OnError::Continue, default, &adapters).await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    let meta = as_dict(get(ask, "meta").unwrap());
    assert_eq!(get(meta, "retries_used"), Some(&Value::from(1i64)));
}

/// `created_at` is written once, before the retry loop -- a second
/// (retried) pass must never overwrite it.
#[tokio::test]
async fn created_at_is_written_once_across_retry_passes() {
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![
            Err(AdapterError {
                message: "scripted: server_error".to_string(),
                retry: RetryInfo::retryable(),
                py_class: PyExc::AdapterCallError,
            }),
            FakeAdapter::text("ok"),
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
    let meta = as_dict(get(ask, "meta").unwrap());
    // `completed_at` is necessarily later than `created_at` -- both
    // real timestamps, so this just confirms there is exactly one
    // `created_at` write (if the retry pass rewrote it, this would
    // still hold trivially; the real guarantee is the key-order tests
    // below, which fail if a second `set_leaf` ever ran since
    // `created_at` would otherwise be a *reinsert*, not a position-
    // stable update).
    assert!(get(meta, "created_at").is_some());
    assert_eq!(
        meta.keys().next(),
        Some(&Value::Str("created_at".to_string())),
        "created_at must stay the first meta key after a retried pass"
    );
}

/// A cancellation requested while a retried pass is waiting out its
/// own backoff sleep must stop the run with `VmError::Cancelled` --
/// no `on_error`, no ordinary failure recording.
#[tokio::test]
async fn a_cancellation_during_backoff_stops_the_run() {
    let token = CancellationToken::new();
    let cancel_token = token.clone();
    struct CancelOnGenerate {
        name: String,
        token: CancellationToken,
        called: std::cell::Cell<bool>,
    }
    #[async_trait(?Send)]
    impl Adapter for CancelOnGenerate {
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
            if !self.called.get() {
                self.called.set(true);
                self.token.request(2);
                return Err(AdapterError {
                    message: "scripted: server_error".to_string(),
                    retry: RetryInfo::retryable(),
                    py_class: PyExc::AdapterCallError,
                });
            }
            panic!("a second attempt must never run once cancellation is requested")
        }
        async fn check(&self) -> CheckResult {
            CheckResult {
                ok: true,
                ..Default::default()
            }
        }
    }
    let default = Rc::new(CancelOnGenerate {
        name: "scripted".to_string(),
        token: cancel_token,
        called: std::cell::Cell::new(false),
    });
    let adapters = registry_with_scripted();
    let store = Store::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    ctx.default_adapter = default;
    ctx.adapters = &adapters;
    ctx.model = "m";
    let mut op = template_prompt("hi");
    op.retries = Some(RetryPolicy {
        max_attempts: 2,
        backoff_ms: 60_000,
    });
    let leaf = prompt_leaf(root_path().push_name("ask"), "ask", op, OnError::Fail);
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
    assert!(
        matches!(result, Err(VmError::Cancelled)),
        "expected VmError::Cancelled, got {result:?}"
    );
}

/// Binding note 3: the pre-dispatch/success meta key order, checked
/// against literal `cof run` output (`tests/conformance/cases/
/// c-scripted-prompt/expected.json`'s own `prime.greeting.meta`).
#[tokio::test]
async fn meta_key_order_on_success_matches_cof_run() {
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![FakeAdapter::text("hello")],
    ));
    let adapters = registry_with_scripted();
    let (store, _observer, result) = run_prompt(
        template_prompt("say hello"),
        OnError::Fail,
        default,
        &adapters,
    )
    .await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    let meta = as_dict(get(ask, "meta").unwrap());
    assert_eq!(
        meta.keys().collect::<Vec<_>>(),
        vec![
            &Value::Str("created_at".to_string()),
            &Value::Str("completed_at".to_string()),
            &Value::Str("adapter".to_string()),
            &Value::Str("model".to_string()),
            &Value::Str("model_reason".to_string()),
            &Value::Str("prompt_type".to_string()),
            &Value::Str("prompt_sent".to_string()),
            &Value::Str("prompt_sent_sha256".to_string()),
            &Value::Str("prompt_chars".to_string()),
            &Value::Str("tokens_sent".to_string()),
            &Value::Str("tokens_received".to_string()),
            &Value::Str("tokens_sent_total".to_string()),
            &Value::Str("tokens_received_total".to_string()),
            &Value::Str("error".to_string()),
            &Value::Str("dry_run".to_string()),
            &Value::Str("fallback_attempts".to_string()),
            &Value::Str("fallback_recovered".to_string()),
            &Value::Str("waiting_for".to_string()),
        ]
    );
}

/// Binding note 3: the failure meta key order, checked against literal
/// `cof run` output (`tests/conformance/cases/prompt-retries-
/// exhausted-fail/expected.out.json`'s own `prime.ask.meta`) --
/// `retries_used` appended at the tail, since it was popped in the
/// pre-dispatch block and only reset when `attempt_index > 0`.
#[tokio::test]
async fn meta_key_order_on_failure_matches_cof_run() {
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![
            Err(AdapterError {
                message: "scripted: server_error".to_string(),
                retry: RetryInfo::retryable(),
                py_class: PyExc::AdapterCallError,
            }),
            Err(AdapterError {
                message: "scripted: server_error".to_string(),
                retry: RetryInfo::retryable(),
                py_class: PyExc::AdapterCallError,
            }),
        ],
    ));
    let adapters = registry_with_scripted();
    let mut op = template_prompt("hi");
    op.retries = Some(RetryPolicy {
        max_attempts: 2,
        backoff_ms: 1,
    });
    let (store, _observer, result) = run_prompt(op, OnError::Continue, default, &adapters).await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    let meta = as_dict(get(ask, "meta").unwrap());
    assert_eq!(
        meta.keys().collect::<Vec<_>>(),
        vec![
            &Value::Str("created_at".to_string()),
            &Value::Str("completed_at".to_string()),
            &Value::Str("adapter".to_string()),
            &Value::Str("model".to_string()),
            &Value::Str("model_reason".to_string()),
            &Value::Str("prompt_type".to_string()),
            &Value::Str("prompt_sent".to_string()),
            &Value::Str("prompt_sent_sha256".to_string()),
            &Value::Str("prompt_chars".to_string()),
            &Value::Str("tokens_sent".to_string()),
            &Value::Str("tokens_received".to_string()),
            &Value::Str("tokens_sent_total".to_string()),
            &Value::Str("tokens_received_total".to_string()),
            &Value::Str("error".to_string()),
            &Value::Str("dry_run".to_string()),
            &Value::Str("fallback_attempts".to_string()),
            &Value::Str("fallback_recovered".to_string()),
            &Value::Str("waiting_for".to_string()),
            &Value::Str("retries_used".to_string()),
        ]
    );
}

/// Binding note 3: the render-failure meta key order, checked against
/// literal `cof run` output (an equivalent real orchestration's own
/// `meta` for a render failure: `created_at`, `error`, `completed_at`
/// -- no `dry_run`, unlike `yield`'s own render-failure shape).
#[tokio::test]
async fn meta_key_order_on_a_render_failure_matches_cof_run() {
    let op = subscripted_int_prompt();
    let default = Rc::new(FakeAdapter::new("scripted", vec![]));
    let adapters = registry_with_scripted();
    let (store, _observer, result) = run_prompt(op, OnError::Continue, default, &adapters).await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    let meta = as_dict(get(ask, "meta").unwrap());
    assert_eq!(
        meta.keys().collect::<Vec<_>>(),
        vec![
            &Value::Str("created_at".to_string()),
            &Value::Str("error".to_string()),
            &Value::Str("completed_at".to_string()),
        ]
    );
}

/// Binding note 3: a reused node's meta key order (a second pass
/// against the same store node, each with a truncated reply) --
/// checked against a literal `cof run`/`electricity` binary diff of
/// an equivalent real orchestration (an unnamed `each` loop sharing
/// one prompt node across two iterations): `finish_reason`/`warnings`
/// are popped by the pre-dispatch block, then re-appended at the tail
/// by `record_reply`, same as a fresh node -- the pop-then-reinsert
/// never leaves them at their *first*-pass tail position once a
/// second pass has run.
#[tokio::test]
async fn meta_key_order_on_a_reused_node_matches_cof_run() {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![
            reply_with("first reply", Some("length"), vec![], 1, 2),
            reply_with("second reply", Some("length"), vec![], 3, 4),
        ],
    ));
    ctx.default_adapter = default;
    let adapters = registry_with_scripted();
    ctx.adapters = &adapters;
    ctx.model = "m";
    let leaf = prompt_leaf(
        root_path().push_name("ask"),
        "ask",
        template_prompt("item"),
        OnError::Fail,
    );
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![leaf.clone()]);

    // First pass.
    electricity_vm::execute_root(
        &program(root.clone()),
        &store,
        &Value::None,
        &ctx,
        &RecordingObserver::new(),
        &token,
    )
    .await
    .unwrap();

    // Second pass, same store node.
    electricity_vm::execute_root(
        &program(root),
        &store,
        &Value::None,
        &ctx,
        &RecordingObserver::new(),
        &token,
    )
    .await
    .unwrap();

    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    let meta = as_dict(get(ask, "meta").unwrap());
    assert_eq!(
        meta.keys().collect::<Vec<_>>(),
        vec![
            &Value::Str("created_at".to_string()),
            &Value::Str("completed_at".to_string()),
            &Value::Str("adapter".to_string()),
            &Value::Str("model".to_string()),
            &Value::Str("model_reason".to_string()),
            &Value::Str("prompt_type".to_string()),
            &Value::Str("prompt_sent".to_string()),
            &Value::Str("prompt_sent_sha256".to_string()),
            &Value::Str("prompt_chars".to_string()),
            &Value::Str("tokens_sent".to_string()),
            &Value::Str("tokens_received".to_string()),
            &Value::Str("tokens_sent_total".to_string()),
            &Value::Str("tokens_received_total".to_string()),
            &Value::Str("error".to_string()),
            &Value::Str("dry_run".to_string()),
            &Value::Str("fallback_attempts".to_string()),
            &Value::Str("fallback_recovered".to_string()),
            &Value::Str("waiting_for".to_string()),
            &Value::Str("finish_reason".to_string()),
            &Value::Str("warnings".to_string()),
        ]
    );
}

/// Issue #498 ruling 4: a `use` child's own unbuildable fallback is
/// claimed by the nearest enclosing `use` node's own label, the same
/// as any other refused child.
#[tokio::test]
async fn a_use_childs_unbuildable_fallback_claims_the_use_child_label() {
    let store = Store::new();
    let token = CancellationToken::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![Err(AdapterError {
            message: "scripted: failed".to_string(),
            retry: RetryInfo::retryable(),
            py_class: PyExc::AdapterCallError,
        })],
    ));
    ctx.default_adapter = default;
    let adapters = registry_with_scripted();
    ctx.adapters = &adapters;
    ctx.model = "m";
    ctx.supported = electricity_bytecode::Supported::current(&[]);

    let inline_yaml = "\
effects:
  - type: prompt
    name: ask
    template: hi
    provider_fallbacks: [openai]
    on_error: continue
";
    let use_op = electricity_bytecode::UseOp {
        source: UseSource::Inline(TemplateText::new(
            inline_yaml.to_string(),
            true,
            Escape::Html,
        )),
        inputs: None,
        outputs: None,
        validate: true,
        retries: RetryPolicy::default(),
        expect: None,
        description: None,
    };
    let op = support::use_leaf(
        root_path().push_name("step"),
        "step",
        use_op.source,
        use_op.inputs,
        use_op.outputs,
        use_op.validate,
        use_op.retries,
        use_op.expect,
        OnError::Fail,
    );
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![op]);
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
    let VmError::Refused {
        refusal,
        child_label,
    } = result.unwrap_err()
    else {
        panic!("expected VmError::Refused");
    };
    assert_eq!(
        refusal.reason,
        electricity_bytecode::RefusalReason::UnsupportedAdapter("openai".to_string())
    );
    assert!(
        child_label.is_some(),
        "the nearest enclosing use node must claim the refusal's own child_label"
    );
}

/// `model: ""` is falsy, same as `None` (Python's own `defn.model or
/// self.model`) -- the run's own default model wins, `model_reason:
/// "default"`.
#[tokio::test]
async fn an_empty_model_string_falls_back_to_the_run_default() {
    let mut op = template_prompt("hi");
    op.model = Some(String::new());
    let default = Rc::new(FakeAdapter::new("scripted", vec![FakeAdapter::text("ok")]));
    let adapters = registry_with_scripted();
    let (store, _observer, result) = run_prompt(op, OnError::Fail, default, &adapters).await;
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    let meta = as_dict(get(ask, "meta").unwrap());
    assert_eq!(get(meta, "model"), Some(&Value::Str("m".to_string())));
    assert_eq!(
        get(meta, "model_reason"),
        Some(&Value::Str("default".to_string()))
    );
}

/// `py_repr` of the "All adapter attempts failed" wrapper message
/// carries a `raw_reply` key for an entry that failed to decode, in
/// Python list-of-dicts repr form (single quotes, `None`) -- only
/// reached when the chain's own *last* entry is an ordinary adapter
/// failure, not a decode/schema one (otherwise that error re-raises
/// bare instead, issue #498's own C14).
#[tokio::test]
async fn py_repr_of_the_wrapper_message_includes_a_raw_reply() {
    let mut op = template_prompt("give json");
    op.prompt_type = PromptType::Json;
    let mut schema = Dict::new();
    schema.insert(
        Value::Str("type".to_string()),
        Value::Str("object".to_string()),
    );
    op.schema = Some(Value::Dict(schema));
    op.provider_fallbacks = vec!["scripted:m2".to_string()];
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![
            FakeAdapter::text("not json at all"),
            Err(AdapterError {
                message: "scripted: server_error".to_string(),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::AdapterCallError,
            }),
        ],
    ));
    let adapters = registry_with_scripted();
    let (_store, _observer, result) = run_prompt(op, OnError::Fail, default, &adapters).await;
    let err = result.unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("All adapter attempts failed: [{"),
        "expected the python-repr wrapper, got {text}"
    );
    assert!(
        text.contains("'raw_reply': 'not json at all'"),
        "the decode-failed entry's own raw_reply must survive into the wrapper's py_repr: {text}"
    );
    assert_eq!(
        text.matches("'raw_reply'").count(),
        1,
        "the plain adapter-call failure (the chain's last entry) must never carry a raw_reply \
         key of its own: {text}"
    );
}

/// Follow-up, finding N12: a real HTTP adapter's own
/// `generate()` call must race cancellation too, not just the backoff
/// sleep/limiter wait around it -- otherwise Ctrl-C/SIGTERM during a
/// prompt would wait out the whole call (up to its own timeout) and
/// then get recorded as a failed attempt, instead of stopping the run
/// at once with no attempt entry at all (the same contract `exec/
/// tool.rs`'s own cancellation check already has).
#[tokio::test]
async fn cancelling_mid_generate_stops_the_run_without_recording_an_attempt() {
    struct NeverAnswers {
        token: CancellationToken,
        called: std::cell::Cell<bool>,
    }
    #[async_trait(?Send)]
    impl Adapter for NeverAnswers {
        fn name(&self) -> &str {
            "scripted"
        }
        async fn generate(
            &self,
            _model: &str,
            _prompt: &str,
            _options: &GenerateOptions,
            _call: &CallContext<'_>,
        ) -> Result<GenerateResult, AdapterError> {
            self.called.set(true);
            // Requests cancellation itself, then never resolves --
            // this is what a real HTTP adapter's own in-flight call
            // looks like from `generate_with_fallbacks`'s own
            // `tokio::select!`'s point of view: it must never
            // actually be awaited to completion.
            self.token.request(2);
            std::future::pending::<()>().await;
            unreachable!("must be cancelled before this future ever resolves")
        }
        async fn check(&self) -> CheckResult {
            CheckResult {
                ok: true,
                ..Default::default()
            }
        }
    }
    let token = CancellationToken::new();
    let default = Rc::new(NeverAnswers {
        token: token.clone(),
        called: std::cell::Cell::new(false),
    });
    let adapters = registry_with_scripted();
    let store = Store::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    ctx.default_adapter = default;
    ctx.adapters = &adapters;
    ctx.model = "m";
    let leaf = prompt_leaf(
        root_path().push_name("ask"),
        "ask",
        template_prompt("hi"),
        OnError::Fail,
    );
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
    assert!(
        matches!(result, Err(VmError::Cancelled)),
        "expected VmError::Cancelled, got {result:?}"
    );
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    let meta = as_dict(get(ask, "meta").unwrap());
    assert_eq!(
        get(meta, "fallback_attempts"),
        Some(&Value::List(Vec::new())),
        "a cancelled-mid-call attempt must never be recorded at all: {meta:?}"
    );
    assert_eq!(
        get(meta, "completed_at"),
        Some(&Value::None),
        "a cancellation must never reach the ordinary completed_at write either: {meta:?}"
    );
}

/// A token cancelled *between* two fallback attempts (after the first
/// one's own ordinary failure, before the second one ever dispatches)
/// must stop the chain there -- checked at the top of every attempt,
/// not just the first (follow-up, finding N12).
#[tokio::test]
async fn cancelling_between_fallback_attempts_never_dispatches_the_next_one() {
    struct CancelsThenPanics {
        token: CancellationToken,
        calls: std::cell::Cell<u32>,
    }
    #[async_trait(?Send)]
    impl Adapter for CancelsThenPanics {
        fn name(&self) -> &str {
            "scripted"
        }
        async fn generate(
            &self,
            _model: &str,
            _prompt: &str,
            _options: &GenerateOptions,
            _call: &CallContext<'_>,
        ) -> Result<GenerateResult, AdapterError> {
            self.calls.set(self.calls.get() + 1);
            // The *first* (and only) attempt this adapter must ever be
            // handed: fails ordinarily, then requests cancellation --
            // by the time the chain would move on to the next fallback
            // entry, the token is already set.
            self.token.request(2);
            Err(AdapterError {
                message: "scripted: server_error".to_string(),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::AdapterCallError,
            })
        }
        async fn check(&self) -> CheckResult {
            CheckResult {
                ok: true,
                ..Default::default()
            }
        }
    }
    let token = CancellationToken::new();
    let default = Rc::new(CancelsThenPanics {
        token: token.clone(),
        calls: std::cell::Cell::new(0),
    });
    let calls = Rc::clone(&default);
    let adapters = registry_with_scripted();
    let store = Store::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    ctx.default_adapter = default;
    ctx.adapters = &adapters;
    ctx.model = "m";
    let mut op = template_prompt("hi");
    op.provider_fallbacks = vec!["scripted:other".to_string()];
    let leaf = prompt_leaf(root_path().push_name("ask"), "ask", op, OnError::Fail);
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
    assert!(
        matches!(result, Err(VmError::Cancelled)),
        "expected VmError::Cancelled, got {result:?}"
    );
    assert_eq!(
        calls.calls.get(),
        1,
        "the second fallback entry must never dispatch once the token is set"
    );
}

/// Issue #516 §6: a pinned/routed model replaces `resolved_model`
/// before `build_attempts` ever runs, so a `provider_fallbacks` token
/// with no model of its own (`parse_provider_token`'s own default,
/// `exec/prompt.rs`) inherits the *routed* model, not the run's own
/// ambient `ctx.model`.
#[tokio::test]
async fn a_routed_model_becomes_the_fallback_chains_default_model() {
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![Err(AdapterError {
            message: "scripted: server_error".to_string(),
            retry: RetryInfo::retryable(),
            py_class: PyExc::AdapterCallError,
        })],
    ));
    let mut adapters = registry_with_scripted();
    adapters.register(
        "ollama",
        Box::new(|_cfg| {
            Ok(
                Box::new(FakeAdapter::new("ollama", vec![FakeAdapter::text("done")]))
                    as Box<dyn Adapter>,
            )
        }),
    );
    let band = electricity_config::ComplexityBand {
        model: "routed-model".to_string(),
        max: None,
        name: Some("big".to_string()),
    };
    let complexity = electricity_config::ComplexitySettings {
        routing: electricity_config::RoutingSettings {
            enabled: true,
            bands: vec![band],
            ..electricity_config::RoutingSettings::default()
        },
        ..electricity_config::ComplexitySettings::default()
    };
    let store = Store::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    ctx.default_adapter = default;
    ctx.adapters = &adapters;
    ctx.model = "m";
    ctx.complexity = &complexity;
    let mut op = template_prompt("hi");
    op.routing_override = Some(RoutingOverride::Named("big".to_string()));
    op.provider_fallbacks = vec!["ollama".to_string()];
    let leaf = prompt_leaf(root_path().push_name("ask"), "ask", op, OnError::Fail);
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![leaf]);
    let token = CancellationToken::new();
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
    result.unwrap();
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    let meta = as_dict(get(ask, "meta").unwrap());
    assert_eq!(
        get(meta, "model"),
        Some(&Value::Str("routed-model".to_string()))
    );
    let Some(Value::List(fallback_attempts)) = get(meta, "fallback_attempts") else {
        panic!("expected a fallback_attempts list");
    };
    assert!(
        fallback_attempts.iter().any(|entry| {
            let entry = as_dict(entry);
            get(entry, "adapter") == Some(&Value::Str("ollama".to_string()))
                && get(entry, "model") == Some(&Value::Str("routed-model".to_string()))
        }),
        "expected the fallback token (no model of its own) to inherit the routed model: {fallback_attempts:?}"
    );
}

/// Issue #516 §3 finding 7: an unknown pinned band raises before
/// `effect_start`, so no start/complete pair fires -- and `on_error:
/// skip` never gets a chance to absorb it, the same way it never
/// absorbs the fatal refusals above (the `?` in `exec/prompt.rs`
/// returns straight out of `execute_prompt`, never through
/// `finish_failure`'s own `op.on_error` check at all).
#[tokio::test]
async fn an_unknown_pinned_band_raises_before_effect_start_and_on_error_skip_does_not_absorb_it() {
    let band = electricity_config::ComplexityBand {
        model: "model-big".to_string(),
        max: None,
        name: Some("big".to_string()),
    };
    let complexity = electricity_config::ComplexitySettings {
        routing: electricity_config::RoutingSettings {
            enabled: true,
            bands: vec![band],
            ..electricity_config::RoutingSettings::default()
        },
        ..electricity_config::ComplexitySettings::default()
    };
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![FakeAdapter::text("hello")],
    ));
    let adapters = registry_with_scripted();
    let store = Store::new();
    let registry = json_registry();
    let limiter = Limiter::new();
    let mut ctx = run_ctx(&registry, &limiter);
    ctx.default_adapter = default;
    ctx.adapters = &adapters;
    ctx.model = "m";
    ctx.complexity = &complexity;
    let mut op = template_prompt("hi");
    op.routing_override = Some(RoutingOverride::Named("nope".to_string()));
    let leaf = prompt_leaf(root_path().push_name("ask"), "ask", op, OnError::Skip);
    let root = chain_dynamic(root_path(), "prime", OnError::Fail, vec![leaf]);
    let token = CancellationToken::new();
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
    let VmError::Raised {
        message,
        cause_class,
        ..
    } = result.unwrap_err()
    else {
        panic!("expected VmError::Raised, on_error: skip must not absorb the unknown-pin error");
    };
    // Wrapped like any other leaf error once it crosses the containing
    // chain (issue #516 §7 ruling 4) -- `cause_class` is the
    // unwrapped `ValueError` [`VmError::displayed_class`] reads back
    // off, the exact text still inside *message* after the chain's
    // own label prefix.
    assert_eq!(cause_class, Some(PyExc::ValueError));
    assert!(
        message.ends_with(
            "Prompt 'ask' is pinned to routing band 'nope', which is not in \
             runtime.complexity.routing.bands. Valid band names: big."
        ),
        "unexpected message: {message}"
    );
    assert!(
        !observer.events().iter().any(|e| matches!(
            e,
            support::Event::Start(p) | support::Event::Complete(p, _) if p == "prime.ask"
        )),
        "no start/complete pair must ever fire for the prompt's own node: {:?}",
        observer.events()
    );
}

fn state_prompt_sent(setting: &str) -> Value {
    let mut state = Dict::new();
    state.insert(
        Value::Str("prompt_sent".to_string()),
        Value::Str(setting.to_string()),
    );
    let mut runtime = Dict::new();
    runtime.insert(Value::Str("state".to_string()), Value::Dict(state));
    Value::Dict(runtime)
}

/// The `prime.ask` meta of a finished run, as a snapshot.
fn ask_meta(store: &Store) -> IndexMap<Value, Value> {
    let snapshot = store.snapshot(&store.root);
    let prime = as_dict(get(as_dict(&snapshot), "prime").unwrap());
    let ask = as_dict(get(prime, "ask").unwrap());
    as_dict(get(ask, "meta").unwrap()).clone()
}

const HELLO_SHA256: &str = "185f8db32271fe25f561a6fc938b2e264306ec304eda518007d1764826381969";

#[tokio::test]
async fn the_default_records_the_whole_text_and_the_two_new_fields() {
    let default = Rc::new(FakeAdapter::new("scripted", vec![FakeAdapter::text("ok")]));
    let adapters = registry_with_scripted();
    let (store, _observer, result) =
        run_prompt(template_prompt("Hello"), OnError::Fail, default, &adapters).await;
    result.unwrap();
    let meta = ask_meta(&store);
    assert_eq!(
        get(&meta, "prompt_sent"),
        Some(&Value::Str("Hello".to_string()))
    );
    assert_eq!(
        get(&meta, "prompt_sent_sha256"),
        Some(&Value::Str(HELLO_SHA256.to_string()))
    );
    assert_eq!(get(&meta, "prompt_chars"), Some(&Value::from(5i64)));
}

#[tokio::test]
async fn sha256_leaves_null_and_the_adapter_still_gets_the_whole_text() {
    let default = Rc::new(FakeAdapter::new("scripted", vec![FakeAdapter::text("ok")]));
    let adapters = registry_with_scripted();
    let runtime = state_prompt_sent("sha256");
    let (store, _observer, result) = run_prompt_with_runtime(
        template_prompt("Hello"),
        OnError::Fail,
        default.clone(),
        &adapters,
        &runtime,
    )
    .await;
    result.unwrap();
    let meta = ask_meta(&store);
    assert_eq!(get(&meta, "prompt_sent"), Some(&Value::None));
    assert_eq!(
        get(&meta, "prompt_sent_sha256"),
        Some(&Value::Str(HELLO_SHA256.to_string()))
    );
    assert_eq!(*default.received.borrow(), vec!["Hello".to_string()]);
}

#[tokio::test]
async fn head_keeps_n_code_points_then_a_marker_and_counts_code_points() {
    let default = Rc::new(FakeAdapter::new("scripted", vec![FakeAdapter::text("ok")]));
    let adapters = registry_with_scripted();
    let runtime = state_prompt_sent("head:3");
    let (store, _observer, result) = run_prompt_with_runtime(
        template_prompt("a\u{1F642}bcdef"),
        OnError::Fail,
        default.clone(),
        &adapters,
        &runtime,
    )
    .await;
    result.unwrap();
    let meta = ask_meta(&store);
    assert_eq!(
        get(&meta, "prompt_sent"),
        Some(&Value::Str(
            "a\u{1F642}b\n[\u{2026} 4 more characters not recorded]".to_string()
        ))
    );
    assert_eq!(get(&meta, "prompt_chars"), Some(&Value::from(7i64)));
    assert_eq!(
        *default.received.borrow(),
        vec!["a\u{1F642}bcdef".to_string()]
    );
}

#[tokio::test]
async fn a_failed_dispatch_under_on_error_continue_records_it_too() {
    let default = Rc::new(FakeAdapter::new("scripted", Vec::new()));
    let adapters = registry_with_scripted();
    let runtime = state_prompt_sent("sha256");
    let (store, _observer, result) = run_prompt_with_runtime(
        template_prompt("Hello"),
        OnError::Continue,
        default,
        &adapters,
        &runtime,
    )
    .await;
    result.unwrap();
    let meta = ask_meta(&store);
    assert_eq!(get(&meta, "prompt_sent"), Some(&Value::None));
    assert_eq!(get(&meta, "prompt_chars"), Some(&Value::from(5i64)));
    assert_ne!(get(&meta, "error"), Some(&Value::None));
}

// ---------- issue #578: a prompt's text read at dispatch from a file named in state ----------

fn dispatch_file(
    within_from: Option<&str>,
    within: Option<&str>,
) -> electricity_bytecode::DispatchFile {
    electricity_bytecode::DispatchFile {
        path_from: "input.prompt_file".to_string(),
        within: within.map(str::to_string),
        within_from: within_from.map(str::to_string),
        confine_to: None,
    }
}

fn input_ctx(pairs: &[(&str, Value)]) -> Value {
    let mut input = Dict::new();
    for (name, value) in pairs {
        input.insert(Value::Str((*name).to_string()), value.clone());
    }
    let mut root = Dict::new();
    root.insert(Value::Str("input".to_string()), Value::Dict(input));
    Value::Dict(root)
}

/// A fresh scratch directory under the system temp dir, removed on drop.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("electricity-578-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(std::fs::canonicalize(&dir).unwrap())
    }

    fn path(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn file_prompt(source: electricity_bytecode::DispatchFile) -> PromptOp {
    let mut op = template_prompt("");
    op.content = PromptContent::TemplateFile(Box::new(source));
    op
}

#[tokio::test]
async fn a_template_file_is_read_at_dispatch_verbatim_and_recorded() {
    let scratch = Scratch::new("verbatim");
    let text = "line {{not rendered}}\r\nh\u{e9}llo \u{1F642}\n";
    std::fs::write(scratch.0.join("p.txt"), text).unwrap();
    let ctx = input_ctx(&[
        ("prompt_file", Value::Str("p.txt".to_string())),
        ("work", Value::Str(scratch.path().to_string())),
    ]);
    let default = Rc::new(FakeAdapter::new("scripted", vec![FakeAdapter::text("ok")]));
    let adapters = registry_with_scripted();
    let (store, _observer, result) = run_prompt_in_ctx(
        file_prompt(dispatch_file(Some("input.work"), None)),
        OnError::Fail,
        default.clone(),
        &adapters,
        &Value::None,
        &ctx,
    )
    .await;
    result.unwrap();
    assert_eq!(*default.received.borrow(), vec![text.to_string()]);
    let meta = ask_meta(&store);
    assert_eq!(
        get(&meta, "prompt_sent"),
        Some(&Value::Str(text.to_string()))
    );
    let Some(Value::List(files)) = get(&meta, "template_files") else {
        panic!("template_files must be a list");
    };
    assert_eq!(files.len(), 1);
    let record = as_dict(&files[0]);
    assert_eq!(
        get(record, "field"),
        Some(&Value::Str("template".to_string()))
    );
    assert_eq!(
        get(record, "path"),
        Some(&Value::Str(format!("{}/p.txt", scratch.path())))
    );
    assert_eq!(
        get(record, "chars"),
        Some(&Value::from(text.chars().count() as i64))
    );
    let sha256 = electricity_vm::exec::use_::sha256_hex(text.as_bytes());
    assert_eq!(get(record, "sha256"), Some(&Value::Str(sha256)));
}

#[tokio::test]
async fn a_message_file_is_read_beside_a_rendered_message() {
    let scratch = Scratch::new("messages");
    std::fs::write(scratch.0.join("p.txt"), "raw {{x}} body").unwrap();
    let ctx = input_ctx(&[
        ("prompt_file", Value::Str("p.txt".to_string())),
        ("tone", Value::Str("brief".to_string())),
    ]);
    let mut op = template_prompt("");
    op.content = PromptContent::Messages(vec![
        Message {
            role: Role::System,
            content: TemplateText::new("be {{input.tone}}", true, Escape::None),
            file: None,
        },
        Message {
            role: Role::User,
            content: TemplateText::new("", true, Escape::None),
            file: Some(dispatch_file(None, Some(scratch.path()))),
        },
    ]);
    let default = Rc::new(FakeAdapter::new("scripted", vec![FakeAdapter::text("ok")]));
    let adapters = registry_with_scripted();
    let (store, _observer, result) = run_prompt_in_ctx(
        op,
        OnError::Fail,
        default.clone(),
        &adapters,
        &Value::None,
        &ctx,
    )
    .await;
    result.unwrap();
    assert_eq!(
        *default.received.borrow(),
        vec!["system: be brief\n\nuser: raw {{x}} body".to_string()]
    );
    let meta = ask_meta(&store);
    let Some(Value::List(files)) = get(&meta, "template_files") else {
        panic!("template_files must be a list");
    };
    assert_eq!(
        get(as_dict(&files[0]), "field"),
        Some(&Value::Str("messages[1].content".to_string()))
    );
}

#[tokio::test]
async fn a_file_error_fails_the_effect_naming_effect_field_and_path() {
    let scratch = Scratch::new("errors");
    std::fs::write(scratch.0.join("big.txt"), vec![b'a'; 1024 * 1024 + 1]).unwrap();
    std::fs::write(scratch.0.join("bin.txt"), [0xffu8, 0x00]).unwrap();
    std::fs::create_dir(scratch.0.join("d")).unwrap();
    let cases: Vec<(Value, &str)> = vec![
        (
            Value::Str("nope.txt".to_string()),
            "Prompt effect 'ask': template: file 'nope.txt' does not exist.",
        ),
        (
            Value::Str("d".to_string()),
            "Prompt effect 'ask': template: file 'd' is not a regular file.",
        ),
        (
            Value::Str("../escape.txt".to_string()),
            "Prompt effect 'ask': template: file '../escape.txt' resolves outside 'within' (",
        ),
        (
            Value::Str("big.txt".to_string()),
            "Prompt effect 'ask': template: file 'big.txt' is over the 1048576-byte limit for a prompt file.",
        ),
        (
            Value::Str("bin.txt".to_string()),
            "Prompt effect 'ask': template: file 'bin.txt' is not valid UTF-8: 'utf-8' codec can't decode byte 0xff in position 0: invalid start byte",
        ),
        (
            Value::from(5i64),
            "Prompt effect 'ask': template: 'file: {from: input.prompt_file}' resolved to 5; expected a file path string.",
        ),
        (
            Value::None,
            "Prompt effect 'ask': template: 'file: {from: input.prompt_file}' resolved to None; expected a file path string.",
        ),
    ];
    for (value, expected) in cases {
        let ctx = input_ctx(&[
            ("prompt_file", value),
            ("work", Value::Str(scratch.path().to_string())),
        ]);
        let default = Rc::new(FakeAdapter::new("scripted", vec![FakeAdapter::text("ok")]));
        let adapters = registry_with_scripted();
        let (store, _observer, result) = run_prompt_in_ctx(
            file_prompt(dispatch_file(Some("input.work"), None)),
            OnError::Continue,
            default.clone(),
            &adapters,
            &Value::None,
            &ctx,
        )
        .await;
        result.unwrap();
        assert!(default.received.borrow().is_empty(), "{expected}");
        let meta = ask_meta(&store);
        let Some(Value::Str(error)) = get(&meta, "error") else {
            panic!("{expected}: no error recorded");
        };
        assert!(error.starts_with(expected), "{error}");
        assert!(get(&meta, "template_files").is_none());
    }
}

#[tokio::test]
async fn a_relative_or_missing_within_fails_the_effect() {
    let ctx = input_ctx(&[
        ("prompt_file", Value::Str("p.txt".to_string())),
        ("work", Value::Str("relative/dir".to_string())),
    ]);
    let default = Rc::new(FakeAdapter::new("scripted", Vec::new()));
    let adapters = registry_with_scripted();
    let (store, _observer, result) = run_prompt_in_ctx(
        file_prompt(dispatch_file(Some("input.work"), None)),
        OnError::Continue,
        default,
        &adapters,
        &Value::None,
        &ctx,
    )
    .await;
    result.unwrap();
    let meta = ask_meta(&store);
    assert_eq!(
        get(&meta, "error"),
        Some(&Value::Str(
            "Prompt effect 'ask': template: 'within' {from: input.work} resolved to 'relative/dir'; expected an absolute directory path.".to_string()
        ))
    );
}

#[tokio::test]
async fn a_limited_documents_within_must_stay_inside_its_project() {
    let project = Scratch::new("project");
    let outside = Scratch::new("outside");
    std::fs::write(outside.0.join("p.txt"), "outside").unwrap();
    let ctx = input_ctx(&[
        ("prompt_file", Value::Str("p.txt".to_string())),
        ("work", Value::Str(outside.path().to_string())),
    ]);
    let mut source = dispatch_file(Some("input.work"), None);
    source.confine_to = Some(project.path().to_string());
    let default = Rc::new(FakeAdapter::new("scripted", vec![FakeAdapter::text("ok")]));
    let adapters = registry_with_scripted();
    let (store, _observer, result) = run_prompt_in_ctx(
        file_prompt(source),
        OnError::Continue,
        default.clone(),
        &adapters,
        &Value::None,
        &ctx,
    )
    .await;
    result.unwrap();
    assert!(default.received.borrow().is_empty());
    let meta = ask_meta(&store);
    let Some(Value::Str(error)) = get(&meta, "error") else {
        panic!("no error recorded");
    };
    assert!(error.contains("resolves outside the project"), "{error}");
}

#[tokio::test]
async fn retries_reuse_the_text_read_before_the_first_attempt() {
    let scratch = Scratch::new("retries");
    std::fs::write(scratch.0.join("p.txt"), "first read").unwrap();
    let ctx = input_ctx(&[
        ("prompt_file", Value::Str("p.txt".to_string())),
        ("work", Value::Str(scratch.path().to_string())),
    ]);
    let default = Rc::new(FakeAdapter::new(
        "scripted",
        vec![
            Err(AdapterError {
                message: "scripted: server_error".to_string(),
                retry: RetryInfo::retryable(),
                py_class: PyExc::AdapterCallError,
            }),
            FakeAdapter::text("ok"),
        ],
    ));
    let mut op = file_prompt(dispatch_file(Some("input.work"), None));
    op.retries = Some(RetryPolicy {
        max_attempts: 2,
        backoff_ms: 1,
    });
    let adapters = registry_with_scripted();
    // Rewriting the file during the first attempt must change nothing: the
    // adapter only ever sees the text read before it.
    let target = scratch.0.join("p.txt");
    *default.on_generate.borrow_mut() = Some(Box::new(move || {
        std::fs::write(&target, "changed mid-run").unwrap();
    }));
    let (_store, _observer, result) = run_prompt_in_ctx(
        op,
        OnError::Fail,
        default.clone(),
        &adapters,
        &Value::None,
        &ctx,
    )
    .await;
    result.unwrap();
    assert_eq!(
        *default.received.borrow(),
        vec!["first read".to_string(), "first read".to_string()]
    );
}
