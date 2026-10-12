//! M1 lane G (issue #498): `prompt` effect execution -- integration
//! tests against a real, compiled document (`execute_root`, this
//! crate's own one `pub` entry point), the same convention `tests/
//! use_effect.rs` uses.

mod support;

use async_trait::async_trait;
use electricity_bytecode::effects::{Message, PromptContent, PromptOp, PromptType, Role};
use electricity_bytecode::template::{Escape, TemplateText};
use electricity_bytecode::{
    EffectPath, LeafKind, NodeKind, OnError, Op, ParamNode, RetryPolicy, UseSource,
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
