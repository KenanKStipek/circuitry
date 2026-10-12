//! `prompt` effect execution (`core/prompt.py::PromptRuntime.execute`) --
//! issue #498 (M1-G). Covers rendering (through [`crate::compose::
//! render_with_composition`], no-escape), the pre-dispatch `meta`
//! block, the `provider:`/default/`provider_fallbacks` attempt chain,
//! per-attempt adapter resolution/build, generation options and
//! assets, decoding (including JSON-schema validation), the retry
//! loop, `on_error`, and the run's token/cost totals. Model-mode
//! `if`/`while`/`expect` (G2), complexity scoring/routing (K2) and
//! decomposition (L2) are out of scope -- the one call-site placeholder
//! each of those lanes fills is marked below.

pub(crate) mod answers;
mod decode;
mod options;

use std::rc::Rc;

use crate::exec::{CtxChain, CtxSource, live_scope};
use crate::observer::InstanceId;
use crate::{CancellationToken, Frame, NodeRef, RunContext, RunObserver, Store, VmError};
use electricity_bytecode::effects::{PromptContent, PromptOp, PromptType, Role};
use electricity_bytecode::{EffectPath, OnError, Op, ParamNode};
use electricity_value::{Dict, Value};

fn key(name: &str) -> Value {
    Value::Str(name.to_string())
}

fn set_leaf(store: &Store, node: &NodeRef, name: &str, value: Value) {
    store.set_leaf(node, key(name), value);
}

fn pop_leaf(node: &NodeRef, name: &str) {
    node.borrow_mut().shift_remove(&key(name));
}

fn now_iso() -> Value {
    use chrono::SecondsFormat;
    Value::Str(chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false))
}

/// `cli.redaction.redact(str(e))`, applied to a plain message string --
/// every caller here already has a bare `String`, never a `Value`.
fn redact_str(message: &str) -> String {
    let mut redacted = electricity_redaction::redact(Value::Str(message.to_string()));
    // `Value` has a hand-written `Drop` impl, so a plain `match
    // redacted { Value::Str(s) => s, ... }` (moving `s` out by value) is
    // `E0509` -- matching through `&mut` and swapping the field out
    // with `std::mem::take` instead (`exec::mod`'s own `live_ctx`
    // comment, same root cause) sidesteps that.
    if let Value::Str(s) = &mut redacted {
        std::mem::take(s)
    } else {
        redacted.py_str()
    }
}

/// `meta.warnings` (a plain list, or absent) -- read out by value
/// (never cloned) through the same `&mut`/`mem::take` pattern
/// [`redact_str`] uses, for the same reason.
fn warnings_list(store: &Store, node: &NodeRef) -> Vec<Value> {
    match store.peek(node, &key("warnings")) {
        Some(crate::Slot::Value(mut value)) => {
            if let Value::List(items) = &mut value {
                std::mem::take(items)
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn push_warning(
    store: &Store,
    meta_node: &NodeRef,
    name: &str,
    warning: String,
    observer: &dyn RunObserver,
) {
    log::warn!("prompt {name:?}: {warning}");
    observer.warning(&format!("prompt {name:?}: {warning}"));
    let mut current = warnings_list(store, meta_node);
    current.push(Value::Str(warning));
    set_leaf(store, meta_node, "warnings", Value::List(current));
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn prompt_type_str(prompt_type: PromptType) -> &'static str {
    match prompt_type {
        PromptType::Text => "text",
        PromptType::Json => "json",
        PromptType::Boolean => "boolean",
        PromptType::Tool => "tool",
        PromptType::Number => "number",
        PromptType::Array => "array",
        PromptType::Object => "object",
    }
}

/// A prompt's own `inputs:`/`params:` -- always a literal dict or
/// absent (`effects::PromptOp::inputs`/`model_params`'s own doc
/// comment: the compiler only ever emits `ParamNode::Literal` for
/// either, never a `{from:}`/Mustache leaf).
fn raw_dict(node: &Option<ParamNode>) -> Option<&Dict> {
    match node {
        Some(ParamNode::Literal(Value::Dict(dict))) => Some(dict),
        _ => None,
    }
}

/// `_cap_reply_text`: 2000 *code points*, not bytes.
fn cap_reply_text(text: &str) -> String {
    const CAP: usize = 2000;
    let char_count = text.chars().count();
    if char_count <= CAP {
        return text.to_string();
    }
    let truncated: String = text.chars().take(CAP).collect();
    format!("{truncated}... [truncated, {char_count} chars total]")
}

/// `_build_attempts`/`_parse_provider_token`.
fn parse_provider_token(
    token: &str,
    default_adapter: &str,
    default_model: &str,
) -> (String, String) {
    let parsed = electricity_value::pycompat::py_strip(token);
    if parsed.is_empty() {
        return (default_adapter.to_string(), default_model.to_string());
    }
    match parsed.find(':') {
        None => (parsed.to_string(), default_model.to_string()),
        Some(idx) => {
            let adapter_name = electricity_value::pycompat::py_strip(&parsed[..idx]).to_string();
            let model_name = electricity_value::pycompat::py_strip(&parsed[idx + 1..]);
            let model_name = if model_name.is_empty() {
                default_model.to_string()
            } else {
                model_name.to_string()
            };
            (adapter_name, model_name)
        }
    }
}

fn build_attempts(
    prompt: &PromptOp,
    default_adapter: &str,
    default_model: &str,
) -> Vec<(String, String)> {
    let mut attempts = vec![(default_adapter.to_string(), default_model.to_string())];
    if let Some(provider) = &prompt.provider {
        attempts.insert(
            0,
            parse_provider_token(provider, default_adapter, default_model),
        );
    }
    for fallback in &prompt.provider_fallbacks {
        attempts.push(parse_provider_token(
            fallback,
            default_adapter,
            default_model,
        ));
    }
    let mut seen = std::collections::HashSet::new();
    attempts.retain(|attempt| seen.insert(attempt.clone()));
    attempts
}

/// Issue #498 ruling 4: *name* is Circuitry-known (`electricity_vm::
/// adapter_names::ADAPTER_NAMES`) but this build has no builder for it
/// at all -- never an ordinary "unknown adapter" failure, a fatal
/// refusal instead.
fn is_unbuildable(name: &str, run_ctx: &RunContext<'_>) -> bool {
    let normalized = electricity_value::pycompat::py_strip(name).to_lowercase();
    crate::adapter_names::ADAPTER_NAMES.contains(&normalized.as_str())
        && !run_ctx.adapters.supported_names().contains(&normalized)
}

/// `allowlist_gate.py::allowed_adapters` -- the run's `enabled_adapters`,
/// or `None` (default-open) when `_allowlists.adapters` isn't a list.
fn allowed_adapters(runtime_config: &Value) -> Option<Vec<String>> {
    let entry = runtime_config
        .as_dict()?
        .get(&key("_allowlists"))?
        .as_dict()?
        .get(&key("adapters"))?;
    match entry {
        Value::List(items) => Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        ),
        _ => None,
    }
}

/// `_attempt_timeout_seconds`.
fn attempt_timeout_seconds(
    adapter_name: &str,
    timeout_ms: Option<u64>,
    run_ctx: &RunContext<'_>,
) -> u32 {
    let adapter_budget =
        electricity_config::configured_timeout_seconds(adapter_name, Some(run_ctx.runtime_config))
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(run_ctx.adapter_timeout_seconds);
    match timeout_ms {
        Some(ms) if ms > 0 => {
            let effect_seconds = ((ms as f64) / 1000.0).ceil().max(1.0) as u32;
            effect_seconds.min(adapter_budget)
        }
        _ => adapter_budget,
    }
}

enum AttemptAdapter {
    Default(Rc<dyn crate::adapter::Adapter>),
    Built(Box<dyn crate::adapter::Adapter>),
}

impl AttemptAdapter {
    fn as_ref(&self) -> &dyn crate::adapter::Adapter {
        match self {
            AttemptAdapter::Default(rc) => rc.as_ref(),
            AttemptAdapter::Built(boxed) => boxed.as_ref(),
        }
    }
}

struct AttemptEntry {
    adapter: String,
    model: String,
    status: &'static str,
    error: Option<String>,
    tokens_sent: Option<i64>,
    tokens_received: Option<i64>,
    raw_reply: Option<String>,
}

impl AttemptEntry {
    fn into_value(self) -> Value {
        let mut dict = Dict::new();
        dict.insert(key("adapter"), Value::Str(self.adapter));
        dict.insert(key("model"), Value::Str(self.model));
        dict.insert(key("status"), Value::Str(self.status.to_string()));
        dict.insert(
            key("error"),
            self.error.map(Value::Str).unwrap_or(Value::None),
        );
        dict.insert(
            key("tokens_sent"),
            self.tokens_sent.map(Value::from).unwrap_or(Value::None),
        );
        dict.insert(
            key("tokens_received"),
            self.tokens_received.map(Value::from).unwrap_or(Value::None),
        );
        if let Some(raw_reply) = self.raw_reply {
            dict.insert(key("raw_reply"), Value::Str(raw_reply));
        }
        Value::Dict(dict)
    }
}

/// The one failure a pass's own fallback chain ended on -- enough to
/// decide whether this pass retries, and what the final error looks
/// like if it doesn't.
struct LastFailure {
    retry: crate::retry::RetryInfo,
    /// `Some` only for a decode/schema failure (`decode_failed`/
    /// `schema_invalid`) -- the bare `(class, message)` `_retry_info_for`'s
    /// own exhaustion path re-raises as-is, rather than wrapping in
    /// `"All adapter attempts failed: ..."`.
    bare: Option<(electricity_tools::PyExc, String)>,
}

enum PassOutcome {
    Success {
        value: Value,
        tokens_sent: Option<i64>,
        tokens_received: Option<i64>,
        cost_usd: Option<f64>,
        attempts_meta: Vec<Value>,
        winner_adapter: String,
        winner_model: String,
    },
    Failed {
        attempts_meta: Vec<Value>,
        failure: LastFailure,
    },
}

/// A failure this pass's own chain never gets to classify at all --
/// propagates past the whole retry loop (issue #498).
enum HardStop {
    /// `AllowlistError` -- a denied `provider:`/fallback; still recorded
    /// like an ordinary failure by the *caller* (the previous pass's
    /// own `attempts_meta`, per the ruling below), just never retried.
    Allowlist(String),
    /// Ruling 4: a `prompt` inside a `use` child named an adapter this
    /// build has no builder for at all -- fatal, never recorded as an
    /// attempt, never absorbed by `on_error`.
    Refused(VmError),
}

/// `_decode_and_validate`. `Ok` the decoded value; `Err` the message,
/// `_decode_failure_status`'s own status string, and the Python
/// exception class the failure text would carry.
fn decode_and_validate(
    prompt: &PromptOp,
    meta_node: &NodeRef,
    store: &Store,
    text: &str,
) -> Result<Value, (String, &'static str, electricity_tools::PyExc)> {
    if matches!(prompt.prompt_type, PromptType::Boolean | PromptType::Number) {
        set_leaf(store, meta_node, "answer", Value::Str(text.to_string()));
    }
    let decoded = decode::decode_output(prompt.prompt_type, text).map_err(|message| {
        (
            message,
            "decode_failed",
            electricity_tools::PyExc::AnswerParseError,
        )
    })?;
    if let Some(schema) = &prompt.schema {
        if matches!(
            prompt.prompt_type,
            PromptType::Json | PromptType::Object | PromptType::Array
        ) {
            validate_schema(schema, &decoded)?;
        }
    }
    Ok(decoded)
}

fn validate_schema(
    schema: &Value,
    decoded: &Value,
) -> Result<(), (String, &'static str, electricity_tools::PyExc)> {
    if matches!(decoded, Value::None) {
        return Err((
            "JSON schema validation failed: model returned None (JSON could not be parsed). \
             Check the model's response format."
                .to_string(),
            "schema_invalid",
            electricity_tools::PyExc::SchemaValidationError,
        ));
    }
    let json_schema = electricity_compiler::schema_instance::to_schema_instance(schema);
    let json_instance = electricity_compiler::schema_instance::to_schema_instance(decoded);
    match electricity_schema::validate_prompt_schema(&json_schema, &json_instance) {
        electricity_schema::PromptSchemaCheck::Valid => Ok(()),
        electricity_schema::PromptSchemaCheck::Invalid(message) => Err((
            format!("Schema validation failed: {message}"),
            "schema_invalid",
            electricity_tools::PyExc::SchemaValidationError,
        )),
        // A malformed schema itself (`jsonschema.SchemaError`) is
        // never `_decode_failure_status`'s `schema_invalid` -- its
        // default `"failed"`, and never forced-retryable either
        // (issue #498 ruling 6).
        electricity_schema::PromptSchemaCheck::SchemaError(message) => {
            Err((message, "failed", electricity_tools::PyExc::RuntimeError))
        }
    }
}

/// `_record_reply`: `finish_reason`/warnings, replacing (not
/// accumulating onto) whatever an earlier attempt/pass left.
fn record_reply(
    store: &Store,
    meta_node: &NodeRef,
    name: &str,
    result: &crate::adapter::GenerateResult,
    option_warnings: &[String],
    observer: &dyn RunObserver,
) {
    pop_leaf(meta_node, "finish_reason");
    let mut warnings: Vec<String> = Vec::new();
    if let Some(finish_reason) = &result.finish_reason {
        set_leaf(
            store,
            meta_node,
            "finish_reason",
            Value::Str(finish_reason.clone()),
        );
        if matches!(finish_reason.as_str(), "length" | "max_tokens") {
            warnings.push(format!(
                "the reply was cut off by the length limit (finish_reason: {finish_reason}); \
                 raise params.max_tokens"
            ));
        }
    }
    if !option_warnings.is_empty() {
        set_leaf(
            store,
            meta_node,
            "warnings",
            Value::List(option_warnings.iter().cloned().map(Value::Str).collect()),
        );
    } else {
        pop_leaf(meta_node, "warnings");
    }
    warnings.extend(result.warnings.iter().cloned());
    for warning in warnings {
        push_warning(store, meta_node, name, warning, observer);
    }
}

#[allow(clippy::too_many_arguments)]
async fn generate_with_fallbacks(
    name: &str,
    op_path: &EffectPath,
    prompt: &PromptOp,
    attempts: &[(String, String)],
    prompt_sent: &str,
    built: &options::BuiltOptions,
    messages: &[crate::adapter::ChatMessage],
    images: &[crate::adapter::ImageInput],
    json_schema: Option<&Value>,
    meta_node: &NodeRef,
    store: &Store,
    option_warnings: &[String],
    run_ctx: &RunContext<'_>,
    call_path: &EffectPath,
    token: &CancellationToken,
    observer: &dyn RunObserver,
) -> Result<PassOutcome, HardStop> {
    let mut attempts_meta: Vec<Value> = Vec::new();
    let mut last_failure: Option<LastFailure> = None;

    for (adapter_name, model_name) in attempts {
        if is_unbuildable(adapter_name, run_ctx) {
            return Err(HardStop::Refused(VmError::Refused {
                refusal: electricity_bytecode::Refusal {
                    path: op_path.clone(),
                    reason: electricity_bytecode::RefusalReason::UnsupportedAdapter(
                        adapter_name.clone(),
                    ),
                },
                child_label: None,
            }));
        }

        let is_default = adapter_name == run_ctx.default_adapter.name();
        let adapter_holder = if is_default {
            AttemptAdapter::Default(Rc::clone(&run_ctx.default_adapter))
        } else {
            if let Err(denial) = electricity_config::require_adapter(
                adapter_name,
                allowed_adapters(run_ctx.runtime_config).as_deref(),
            ) {
                return Err(HardStop::Allowlist(denial));
            }
            match run_ctx.adapters.build(adapter_name, run_ctx.runtime_config) {
                Ok(adapter) => AttemptAdapter::Built(adapter),
                Err(err) => {
                    attempts_meta.push(
                        AttemptEntry {
                            adapter: adapter_name.clone(),
                            model: model_name.clone(),
                            status: "failed",
                            error: Some(redact_str(&err.message)),
                            tokens_sent: None,
                            tokens_received: None,
                            raw_reply: None,
                        }
                        .into_value(),
                    );
                    last_failure = Some(LastFailure {
                        retry: err.retry,
                        bare: None,
                    });
                    continue;
                }
            }
        };

        let timeout_seconds = attempt_timeout_seconds(adapter_name, prompt.timeout_ms, run_ctx);
        let call = crate::adapter::CallContext {
            path: call_path,
            timeout_seconds,
            token,
        };
        let generate_options = crate::adapter::GenerateOptions {
            temperature: built.temperature,
            max_tokens: built.max_tokens,
            stop: built.stop.clone(),
            params: built.params.clone(),
            deterministic: prompt.deterministic,
            messages: messages.to_vec(),
            images: images.to_vec(),
            json_schema: json_schema.cloned(),
        };

        match adapter_holder
            .as_ref()
            .generate(model_name, prompt_sent, &generate_options, &call)
            .await
        {
            Err(err) => {
                attempts_meta.push(
                    AttemptEntry {
                        adapter: adapter_name.clone(),
                        model: model_name.clone(),
                        status: "failed",
                        error: Some(redact_str(&err.message)),
                        tokens_sent: None,
                        tokens_received: None,
                        raw_reply: None,
                    }
                    .into_value(),
                );
                last_failure = Some(LastFailure {
                    retry: err.retry,
                    bare: None,
                });
            }
            Ok(result) => {
                record_reply(store, meta_node, name, &result, option_warnings, observer);
                match decode_and_validate(prompt, meta_node, store, &result.text) {
                    Ok(decoded_value) => {
                        attempts_meta.push(
                            AttemptEntry {
                                adapter: adapter_name.clone(),
                                model: model_name.clone(),
                                status: "succeeded",
                                error: None,
                                tokens_sent: result.tokens_sent,
                                tokens_received: result.tokens_received,
                                raw_reply: None,
                            }
                            .into_value(),
                        );
                        return Ok(PassOutcome::Success {
                            value: decoded_value,
                            tokens_sent: result.tokens_sent,
                            tokens_received: result.tokens_received,
                            cost_usd: result.cost_usd,
                            attempts_meta,
                            winner_adapter: adapter_name.clone(),
                            winner_model: model_name.clone(),
                        });
                    }
                    Err((message, status, py_class)) => {
                        attempts_meta.push(
                            AttemptEntry {
                                adapter: adapter_name.clone(),
                                model: model_name.clone(),
                                status,
                                error: Some(redact_str(&message)),
                                tokens_sent: result.tokens_sent,
                                tokens_received: result.tokens_received,
                                raw_reply: Some(cap_reply_text(&result.text)),
                            }
                            .into_value(),
                        );
                        let retry = match status {
                            "decode_failed" | "schema_invalid" => {
                                crate::retry::RetryInfo::retryable()
                            }
                            _ => crate::retry::RetryInfo::not_retryable(),
                        };
                        let bare = match status {
                            "decode_failed" | "schema_invalid" => Some((py_class, message)),
                            _ => None,
                        };
                        last_failure = Some(LastFailure { retry, bare });
                    }
                }
            }
        }
    }

    Ok(PassOutcome::Failed {
        attempts_meta,
        failure: last_failure.unwrap_or(LastFailure {
            retry: crate::retry::RetryInfo::not_retryable(),
            bare: None,
        }),
    })
}

/// Accumulates every attempt entry's non-`None` `tokens_sent`/
/// `tokens_received` into *totals* -- `None` until the first real
/// value, matching Python's own `(total_tokens_sent or 0) + sent`.
fn accumulate_tokens(totals: &mut (Option<i64>, Option<i64>), attempts_meta: &[Value]) {
    for entry in attempts_meta {
        let Some(dict) = entry.as_dict() else {
            continue;
        };
        if let Some(Value::Int(electricity_value::IntValue::Small(n))) =
            dict.get(&key("tokens_sent"))
        {
            totals.0 = Some(totals.0.unwrap_or(0) + n);
        }
        if let Some(Value::Int(electricity_value::IntValue::Small(n))) =
            dict.get(&key("tokens_received"))
        {
            totals.1 = Some(totals.1.unwrap_or(0) + n);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_failure(
    store: &Store,
    meta_node: &NodeRef,
    effect_node: &NodeRef,
    op: &Op,
    effect_path: &EffectPath,
    instance: InstanceId,
    observer: &dyn RunObserver,
    attempts_meta: Vec<Value>,
    attempt_index: u32,
    message: String,
    py_class: electricity_tools::PyExc,
) -> Result<(), VmError> {
    set_leaf(
        store,
        meta_node,
        "fallback_attempts",
        Value::List(attempts_meta),
    );
    set_leaf(store, meta_node, "fallback_recovered", Value::Bool(false));
    let redacted = redact_str(&message);
    set_leaf(store, meta_node, "error", Value::Str(redacted.clone()));
    set_leaf(store, meta_node, "completed_at", now_iso());
    if attempt_index > 0 {
        set_leaf(
            store,
            meta_node,
            "retries_used",
            Value::from(i64::from(attempt_index)),
        );
    }
    if matches!(op.on_error, OnError::Skip | OnError::Continue) {
        set_leaf(store, effect_node, "value", Value::None);
    }
    observer.record_totals(&store.snapshot(meta_node));
    observer.effect_complete(effect_path, instance, Some(&redacted));
    observer.write();
    if op.on_error == OnError::Fail {
        return Err(VmError::Raised {
            py_class,
            message,
            cause: electricity_tools::ErrorCause::None,
            cause_class: None,
        });
    }
    Ok(())
}

/// Runs *prompt* (a `prompt` effect's own compiled fields) against
/// *store*, under *parent*, with *ctx_chain* the not-yet-materialized
/// rendering context. See this module's own doc comment.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_prompt(
    op: &Op,
    prompt: &PromptOp,
    store: &Store,
    parent: &NodeRef,
    ctx_chain: &CtxChain,
    frame: &Frame,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
) -> Result<(), VmError> {
    let name = op
        .name
        .as_deref()
        .expect("a prompt effect's own Op always carries a name");
    let effect_node = store
        .ensure_dict(parent, key(name))
        .map_err(super::store_err)?;
    if !effect_node.borrow().contains_key(&key("value")) {
        set_leaf(store, &effect_node, "value", Value::None);
    }
    let meta_node = store
        .ensure_dict(&effect_node, key("meta"))
        .map_err(super::store_err)?;

    // `effective_ctx = dict(ctx); effective_ctx.update(defn.inputs)` --
    // a raw, top-level-only overlay: appended as one more `Frozen`
    // entry at the end of the chain, so it wins every key it sets
    // without materializing anything `ctx_chain` doesn't already carry.
    let mut render_chain = ctx_chain.clone();
    if let Some(inputs) = raw_dict(&prompt.inputs) {
        render_chain.push(CtxSource::Frozen(Value::Dict(inputs.clone())));
    }
    let render_root = live_scope::LiveRoot::new(&render_chain, store);

    let render_result = render_prompt(prompt, &render_root, run_ctx);
    let (messages, prompt_sent) = match render_result {
        Ok(pair) => pair,
        Err(message) => {
            set_leaf(store, &meta_node, "created_at", now_iso());
            set_leaf(store, &meta_node, "error", Value::Str(redact_str(&message)));
            set_leaf(store, &meta_node, "completed_at", now_iso());
            set_leaf(store, &effect_node, "value", Value::None);
            let effect_path = frame.effect_path(&op.path);
            let instance = observer.effect_start(&effect_path);
            observer.effect_complete(&effect_path, instance, Some(&redact_str(&message)));
            observer.write();
            if op.on_error == OnError::Fail {
                return Err(VmError::Raised {
                    py_class: electricity_tools::PyExc::TemplateError,
                    message,
                    cause: electricity_tools::ErrorCause::None,
                    cause_class: None,
                });
            }
            return Ok(());
        }
    };

    let resolved_model = prompt
        .model
        .clone()
        .unwrap_or_else(|| run_ctx.model.to_string());
    let model_reason = if prompt.model.is_some() {
        "explicit"
    } else {
        "default"
    };

    set_leaf(store, &meta_node, "created_at", now_iso());
    set_leaf(store, &meta_node, "completed_at", Value::None);
    set_leaf(
        store,
        &meta_node,
        "adapter",
        Value::Str(run_ctx.default_adapter.name().to_string()),
    );
    set_leaf(
        store,
        &meta_node,
        "model",
        Value::Str(resolved_model.clone()),
    );
    set_leaf(
        store,
        &meta_node,
        "model_reason",
        Value::Str(model_reason.to_string()),
    );
    set_leaf(
        store,
        &meta_node,
        "prompt_type",
        Value::Str(prompt_type_str(prompt.prompt_type).to_string()),
    );
    set_leaf(
        store,
        &meta_node,
        "prompt_sent",
        Value::Str(prompt_sent.clone()),
    );
    set_leaf(store, &meta_node, "tokens_sent", Value::None);
    set_leaf(store, &meta_node, "tokens_received", Value::None);
    set_leaf(store, &meta_node, "tokens_sent_total", Value::None);
    set_leaf(store, &meta_node, "tokens_received_total", Value::None);
    set_leaf(store, &meta_node, "error", Value::None);
    set_leaf(store, &meta_node, "dry_run", Value::Bool(run_ctx.dry_run));
    set_leaf(
        store,
        &meta_node,
        "fallback_attempts",
        Value::List(Vec::new()),
    );
    set_leaf(store, &meta_node, "fallback_recovered", Value::Bool(false));
    pop_leaf(&meta_node, "retries_used");
    // K2/L2's own model-mode routing/decomposition call-site placeholder:
    // `_score_and_route`/`_maybe_decompose` are out of scope for this
    // lane (issue #498 rulings 1-2) -- `meta.complexity`/`meta.decomposition`
    // are simply never written.
    pop_leaf(&meta_node, "decomposition");
    pop_leaf(&meta_node, "answer");
    pop_leaf(&meta_node, "finish_reason");
    pop_leaf(&meta_node, "warnings");
    pop_leaf(&meta_node, "assets");
    pop_leaf(&meta_node, "cost_usd");
    set_leaf(store, &meta_node, "waiting_for", Value::None);
    observer.write();

    let effect_path = frame.effect_path(&op.path);
    let instance = observer.effect_start(&effect_path);

    let (max_attempts, backoff_ms): (u32, u32) = match prompt.retries {
        Some(policy) => (policy.max_attempts, policy.backoff_ms),
        None => {
            let raw = run_ctx
                .runtime_config
                .as_dict()
                .and_then(|dict| dict.get(&key("default_prompt_retries")))
                .cloned()
                .unwrap_or_else(|| Value::from(1i64));
            let parsed =
                electricity_value::pycompat::py_int(&raw).map_err(|err| VmError::Raised {
                    py_class: err.py_class,
                    message: err.message,
                    cause: electricity_tools::ErrorCause::None,
                    cause_class: None,
                })?;
            (parsed.clamp(0, i64::from(u32::MAX)) as u32, 1000)
        }
    };

    let built_options = match options::build_options(
        name,
        raw_dict(&prompt.model_params)
            .map(|d| Value::Dict(d.clone()))
            .as_ref(),
        prompt.deterministic,
    ) {
        Ok(built) => built,
        Err(message) => {
            return finish_failure(
                store,
                &meta_node,
                &effect_node,
                op,
                &effect_path,
                instance,
                observer,
                Vec::new(),
                0,
                message,
                electricity_tools::PyExc::ValueError,
            );
        }
    };

    let mut images = Vec::new();
    let mut asset_meta = Vec::new();
    for (index, asset) in prompt.assets.iter().enumerate() {
        let label = format!("assets[{index}].ref");
        let rendered_ref = match electricity_template::render_template_with_root(
            &asset.reference,
            &render_root,
            &electricity_template::PlainCtx,
            &label,
        ) {
            Ok(rendered) => electricity_value::pycompat::py_strip(&rendered).to_string(),
            Err(err) => {
                return finish_failure(
                    store,
                    &meta_node,
                    &effect_node,
                    op,
                    &effect_path,
                    instance,
                    observer,
                    Vec::new(),
                    0,
                    err.to_string(),
                    electricity_tools::PyExc::TemplateError,
                );
            }
        };
        match options::load_asset(&asset.kind, &rendered_ref) {
            Ok(options::LoadedAsset::Image { image, record }) => {
                images.push(image);
                asset_meta.push(record);
            }
            Ok(options::LoadedAsset::NonImageWarning(warning)) => {
                push_warning(store, &meta_node, name, warning, observer);
            }
            Err(message) => {
                return finish_failure(
                    store,
                    &meta_node,
                    &effect_node,
                    op,
                    &effect_path,
                    instance,
                    observer,
                    Vec::new(),
                    0,
                    message,
                    electricity_tools::PyExc::ValueError,
                );
            }
        }
    }
    if !asset_meta.is_empty() {
        set_leaf(store, &meta_node, "assets", Value::List(asset_meta));
    }

    let json_schema = if matches!(
        prompt.prompt_type,
        PromptType::Json | PromptType::Object | PromptType::Array
    ) {
        prompt.schema.clone()
    } else {
        None
    };

    let option_warnings: Vec<String> = warnings_list(store, &meta_node)
        .into_iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();

    let attempts = build_attempts(prompt, run_ctx.default_adapter.name(), &resolved_model);
    let call_path = effect_path.clone();

    let mut attempts_meta_outer: Vec<Value> = Vec::new();
    let mut totals: (Option<i64>, Option<i64>) = (None, None);
    let mut next_delay_ms = backoff_ms;

    for attempt_index in 0..max_attempts {
        if attempt_index > 0 {
            if token.is_set() {
                return Err(VmError::Cancelled);
            }
            tokio::select! {
                () = tokio::time::sleep(std::time::Duration::from_millis(u64::from(next_delay_ms))) => {}
                () = token.cancelled() => return Err(VmError::Cancelled),
            }
        }

        let acquire_future = run_ctx
            .limiter
            .acquire_reporting(prompt.group.as_deref(), |event| match event {
                crate::limiter::SlotEvent::Waiting(label) => {
                    set_leaf(
                        store,
                        &meta_node,
                        "waiting_for",
                        Value::Str(label.to_string()),
                    );
                    observer.write();
                }
                crate::limiter::SlotEvent::Acquired => {
                    set_leaf(store, &meta_node, "waiting_for", Value::None);
                    observer.write();
                }
            });
        let acquired = tokio::select! {
            biased;
            result = acquire_future => Some(result),
            () = token.cancelled() => None,
        };
        let guard = match acquired {
            None => return Err(VmError::Cancelled),
            Some(Err(limiter_err)) => {
                return finish_failure(
                    store,
                    &meta_node,
                    &effect_node,
                    op,
                    &effect_path,
                    instance,
                    observer,
                    attempts_meta_outer,
                    attempt_index,
                    limiter_err.to_string(),
                    electricity_tools::PyExc::RuntimeError,
                );
            }
            Some(Ok(guard)) => guard,
        };

        let outcome = generate_with_fallbacks(
            name,
            &op.path,
            prompt,
            &attempts,
            &prompt_sent,
            &built_options,
            &chat_messages(&messages),
            &images,
            json_schema.as_ref(),
            &meta_node,
            store,
            &option_warnings,
            run_ctx,
            &call_path,
            token,
            observer,
        )
        .await;
        drop(guard);

        match outcome {
            Err(HardStop::Refused(err)) => return Err(err),
            Err(HardStop::Allowlist(message)) => {
                return finish_failure(
                    store,
                    &meta_node,
                    &effect_node,
                    op,
                    &effect_path,
                    instance,
                    observer,
                    attempts_meta_outer,
                    attempt_index,
                    message,
                    electricity_tools::PyExc::AllowlistError,
                );
            }
            Ok(PassOutcome::Success {
                value,
                tokens_sent,
                tokens_received,
                cost_usd,
                attempts_meta,
                winner_adapter,
                winner_model,
            }) => {
                accumulate_tokens(&mut totals, &attempts_meta);
                set_leaf(
                    store,
                    &meta_node,
                    "tokens_sent_total",
                    totals.0.map(Value::from).unwrap_or(Value::None),
                );
                set_leaf(
                    store,
                    &meta_node,
                    "tokens_received_total",
                    totals.1.map(Value::from).unwrap_or(Value::None),
                );
                set_leaf(store, &effect_node, "value", value);
                set_leaf(
                    store,
                    &meta_node,
                    "tokens_sent",
                    tokens_sent.map(Value::from).unwrap_or(Value::None),
                );
                set_leaf(
                    store,
                    &meta_node,
                    "tokens_received",
                    tokens_received.map(Value::from).unwrap_or(Value::None),
                );
                if let Some(cost_usd) = cost_usd {
                    set_leaf(store, &meta_node, "cost_usd", Value::from(cost_usd));
                }
                let recovered = attempts_meta.len() > 1;
                set_leaf(
                    store,
                    &meta_node,
                    "fallback_attempts",
                    Value::List(attempts_meta),
                );
                set_leaf(
                    store,
                    &meta_node,
                    "fallback_recovered",
                    Value::Bool(recovered),
                );
                set_leaf(store, &meta_node, "completed_at", now_iso());
                set_leaf(store, &meta_node, "adapter", Value::Str(winner_adapter));
                set_leaf(store, &meta_node, "model", Value::Str(winner_model));
                if attempt_index > 0 {
                    set_leaf(
                        store,
                        &meta_node,
                        "retries_used",
                        Value::from(i64::from(attempt_index)),
                    );
                }
                observer.record_totals(&store.snapshot(&meta_node));
                observer.effect_complete(&effect_path, instance, None);
                observer.write();
                return Ok(());
            }
            Ok(PassOutcome::Failed {
                attempts_meta,
                failure,
            }) => {
                accumulate_tokens(&mut totals, &attempts_meta);
                set_leaf(
                    store,
                    &meta_node,
                    "tokens_sent_total",
                    totals.0.map(Value::from).unwrap_or(Value::None),
                );
                set_leaf(
                    store,
                    &meta_node,
                    "tokens_received_total",
                    totals.1.map(Value::from).unwrap_or(Value::None),
                );
                attempts_meta_outer = attempts_meta;

                let is_last_attempt = attempt_index + 1 >= max_attempts;
                if !failure.retry.retryable || is_last_attempt {
                    let (message, py_class) = match failure.bare {
                        Some((py_class, message)) => (message, py_class),
                        None => {
                            let repr = Value::List(attempts_meta_outer.clone()).py_repr();
                            (
                                format!("All adapter attempts failed: {repr}"),
                                electricity_tools::PyExc::RuntimeError,
                            )
                        }
                    };
                    return finish_failure(
                        store,
                        &meta_node,
                        &effect_node,
                        op,
                        &effect_path,
                        instance,
                        observer,
                        attempts_meta_outer,
                        attempt_index,
                        message,
                        py_class,
                    );
                }
                next_delay_ms =
                    crate::retry::next_backoff_delay_ms(&failure.retry, attempt_index, backoff_ms);
                observer.write();
            }
        }
    }

    // `range(max_attempts)` with `max_attempts <= 0` runs zero passes
    // (issue #498: no `max(1, ...)` for a prompt's own retry policy) --
    // `execute()` then falls out of the `try` with `completed_at: None`
    // and no `effect_complete` at all, exactly like this.
    Ok(())
}

fn chat_messages(messages: &[(Role, String)]) -> Vec<crate::adapter::ChatMessage> {
    messages
        .iter()
        .map(|(role, content)| crate::adapter::ChatMessage {
            role: role_str(*role).to_string(),
            content: content.clone(),
        })
        .collect()
}

/// `_render_messages` + `_materialize_input`: `template` wins whenever
/// the IR carries it (the compiler already resolved that truthiness
/// check); otherwise the rendered messages, flattened as `meta.
/// prompt_sent`. Returns the *typed* messages (for [`chat_messages`])
/// alongside the flattened text.
fn render_prompt<R: electricity_template::ScopeRoot>(
    prompt: &PromptOp,
    root: &R,
    run_ctx: &RunContext<'_>,
) -> Result<(Vec<(Role, String)>, String), String> {
    match &prompt.content {
        PromptContent::Template(text) => {
            let rendered = crate::compose::render_with_composition(
                &text.source,
                root,
                run_ctx.declared_prompts,
                run_ctx.effect_names,
                "template",
                &electricity_template::PlainCtx,
                false,
            )?;
            Ok((Vec::new(), rendered))
        }
        PromptContent::Messages(messages) => {
            let mut rendered = Vec::with_capacity(messages.len());
            let mut flattened = Vec::with_capacity(messages.len());
            for (index, message) in messages.iter().enumerate() {
                let label = format!("messages[{index}].content");
                let content = crate::compose::render_with_composition(
                    &message.content.source,
                    root,
                    run_ctx.declared_prompts,
                    run_ctx.effect_names,
                    &label,
                    &electricity_template::PlainCtx,
                    false,
                )?;
                flattened.push(format!("{}: {content}", role_str(message.role)));
                rendered.push((message.role, content));
            }
            Ok((rendered, flattened.join("\n\n")))
        }
    }
}
