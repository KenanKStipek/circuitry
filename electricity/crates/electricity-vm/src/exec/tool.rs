//! `run_tool`: the VM's own tool-dispatch entry point -- looks a `tool`
//! effect's `provider:` up in an [`electricity_tools::ToolRegistry`] and
//! runs it. The dispatch seam itself is final (lane B/C never edit this
//! file to wire a new provider in; they only build the registry they
//! pass to it); *provider* normalization already matches `plugins/
//! factory.py::build_plugin`'s own `.strip().lower()` (below), but the
//! exact "unknown provider" wording still diverges -- this crate's own
//! [`crate::VmError::ToolNotFound`] text isn't `build_plugin`'s own
//! `"Unknown plugin: 'x'. Supported plugins: ..."` -- so lane C may
//! still widen that text with the supported-provider list once it has
//! the full `electricity_tools::ToolRegistry` populated to word that
//! list from.
//!
//! [`execute_tool`] is the lane C's own owning entry point for a `tool`
//! effect -- a close port of `core/tool.py::ToolRuntime.execute`. It
//! owns:
//!
//! - the `meta` reset at the start of every attempt;
//! - the `require_tool` allowlist check and the `allowed_commands`
//!   dispatch-time literal check (`_reject_templated_security_params`/
//!   `_reject_params_json_security_overrides`);
//! - rendering `params`/`params_json` ([`crate::params`]);
//! - `retries`/full-jitter backoff (capped at 60 s) between attempts, a
//!   [`crate::Limiter`] slot held per attempt and released before that
//!   attempt's own backoff sleep, and a CEL `expect:` evaluated against
//!   each successful attempt's own result;
//! - redacting and 64 KiB-capping the result before it is written at
//!   all ([`electricity_redaction`]);
//! - writing the outcome into the store and reporting it to the
//!   [`crate::RunObserver`];
//! - checking the [`CancellationToken`] between attempts and during the
//!   backoff/limiter wait -- unlike an ordinary attempt failure, a
//!   cancellation here bypasses `on_error` entirely (see
//!   [`crate::VmError::Cancelled`]'s own doc comment).
//!
//! Deliberately out of scope for M0-H, all because the only provider
//! this milestone ever dispatches through [`run_tool`] is `json`
//! (issue #431's Scope section):
//! - the HTTP-family status-code/`Retry-After` machinery
//!   (`core/tool.py`'s own `_HTTP_FAMILY_PROVIDERS`/`_is_retryable_failure`)
//!   -- no HTTP-shaped tool exists yet in this crate's own
//!   [`electricity_tools::ToolRegistry`], so every failure is simply
//!   retryable, exactly like any non-HTTP-family Python plugin already
//!   is;
//! - `meta.binary`/`meta.status_code` -- `json`'s own `ToolResult.raw`
//!   never carries either key, so the write is a no-op every time it
//!   would run; left unwired rather than wired against keys nothing
//!   produces yet;
//! - `expect: {mode: model, ...}` -- refused before a run starts
//!   ([`electricity_bytecode::refusal::first_unsupported`]), so
//!   [`ToolOp::expect`] is never anything but
//!   [`electricity_bytecode::ExpectCondition::Cel`] by the time this
//!   function sees it;
//! - the capability-ceiling check (`capability_gate.require_within_ceiling`)
//!   -- `json` needs no capability (`plugins/capabilities.py`'s own
//!   `PLUGIN_CAPABILITIES` has no entry for it), and M0-H's own run
//!   wiring always starts a document's capability ceiling empty, so this
//!   check can never fail for the one tool this milestone runs; adding
//!   it here would only check against data (a capability ceiling) this
//!   crate's own `RunContext` doesn't carry yet.

use crate::params::{
    RenderParamsError, RenderParamsJsonError, deep_merge_params, render_params, render_params_json,
};
#[cfg(test)]
use crate::store::Slot;
use crate::{CancellationToken, RunContext, RunObserver, VmError};
use electricity_bytecode::{ExpectCondition, OnError, Op, ParamNode, ToolOp};
use electricity_tools::ToolRegistry;
use electricity_value::{Dict, Value};
use indexmap::IndexMap;
use rand::Rng;

/// Runs *provider* (a tool effect's own `provider:`) against *registry*,
/// with *params* already rendered (lane B/C's own `params.rs` -- `{from:
/// ...}` resolved, templates rendered, `params_json` deep-merged; out of
/// scope for this function) and *timeout_seconds* already resolved by
/// the caller (`core/tool.py::ToolRuntime._resolve_timeout_seconds`'s
/// own `timeout_ms`-vs-`runtime.tools.timeout_seconds`-vs-default
/// policy needs the merged runtime config this function doesn't take --
/// [`crate::RunContext::runtime_config`] is where lane B's own caller
/// reads it from before calling this).
///
/// `Err` is [`VmError::ToolNotFound`] for an unregistered *provider*, or
/// the plugin's own [`electricity_tools::ToolError`] wrapped the same
/// way.
pub async fn run_tool(
    registry: &ToolRegistry,
    provider: &str,
    params: Value,
    timeout_seconds: u32,
) -> Result<electricity_tools::ToolResult, VmError> {
    let normalized = provider.trim().to_lowercase();
    let plugin = registry
        .get(&normalized)
        .ok_or_else(|| VmError::ToolNotFound(provider.to_string()))?;
    plugin
        .execute(params, timeout_seconds)
        .await
        .map_err(|err| VmError::Tool(err.to_string()))
}

/// `core/tool.py::DEFAULT_TOOL_TIMEOUT_SECONDS`.
const DEFAULT_TOOL_TIMEOUT_SECONDS: u32 = 300;

/// `adapters/_retry.py::RETRY_BACKOFF_CAP_MS`.
const RETRY_BACKOFF_CAP_MS: u32 = 60_000;

/// `core/tool.py::_SECURITY_SENSITIVE_PARAM_KEYS`.
const SECURITY_SENSITIVE_PARAM_KEYS: [&str; 1] = ["allowed_commands"];

/// `core/tool.py::ToolRuntime._resolve_timeout_seconds`. *runtime_config*
/// is the merged `runtime:` block [`RunContext::runtime_config`] carries;
/// lane D is the one that ever populates `runtime.tools.timeout_seconds`
/// in it, so this always falls through to [`DEFAULT_TOOL_TIMEOUT_SECONDS`]
/// until then.
fn resolve_timeout_seconds(tool: &ToolOp, runtime_config: &Value) -> u32 {
    // `if self.defn.timeout_ms:` -- falsy, so an explicit `timeout_ms: 0`
    // (the compiler already clamps a negative one to 0) falls through to
    // `runtime.tools.timeout_seconds` exactly like an absent one does.
    if let Some(ms) = tool.timeout_ms.filter(|&ms| ms > 0) {
        let seconds = ms.div_ceil(1000).max(1);
        return seconds.min(u64::from(u32::MAX)) as u32;
    }
    let raw = runtime_config
        .as_dict()
        .and_then(|d| d.get(&Value::Str("tools".to_string())))
        .and_then(Value::as_dict)
        .and_then(|d| d.get(&Value::Str("timeout_seconds".to_string())));
    match raw {
        None | Some(Value::None) => DEFAULT_TOOL_TIMEOUT_SECONDS,
        // `max(1, int(raw))` -- a negative/zero value floors to 1, same as
        // any other `int(raw)` result.
        Some(value) => match python_int_floor(value) {
            Some(n) => n.clamp(1, i64::from(u32::MAX)) as u32,
            // `except (TypeError, ValueError): ... return DEFAULT_TOOL_TIMEOUT_SECONDS`
            // -- Python also logs a warning here; this crate has no run
            // logger yet to port that side effect to.
            None => DEFAULT_TOOL_TIMEOUT_SECONDS,
        },
    }
}

/// `int(raw)`, as far as [`resolve_timeout_seconds`] needs it: `bool`/
/// `int`/`float` coerce the way CPython's `int()` does (including
/// negative values -- the caller's own `max(1, ...)` floors those, just
/// as Python's does), a numeric `str` parses the same way; anything else
/// (including a non-numeric `str`) is `None`, this function's caller's
/// cue to fall back to the default rather than Python's own
/// `ValueError`/`TypeError` text (never surfaced in state, so there is
/// nothing here to match byte for byte).
fn python_int_floor(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Int(i) => i.to_string().parse().ok(),
        Value::Float(f) if f.is_finite() => Some(f.trunc() as i64),
        Value::Str(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `core/allowlist_gate.py::allowed_tools`/`_installed` -- the run's
/// `enabled_tools`, or `None` (default-open) when
/// *runtime_config*`._allowlists.tools` isn't a list.
fn allowed_tools(runtime_config: &Value) -> Option<Vec<String>> {
    let entry = runtime_config
        .as_dict()?
        .get(&Value::Str("_allowlists".to_string()))?
        .as_dict()?
        .get(&Value::Str("tools".to_string()))?;
    match entry {
        Value::List(items) => Some(
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        ),
        _ => None,
    }
}

/// `core/allowlist_gate.py::require_tool`/`tool_denial` -- `Err` with the
/// exact `AllowlistError` text once *provider* (canonicalised the way
/// `build_plugin` canonicalises it) isn't in the run's `enabled_tools`.
fn require_tool(provider: &str, runtime_config: &Value) -> Result<(), String> {
    let normalized = provider.trim().to_lowercase();
    let Some(allowed) = allowed_tools(runtime_config) else {
        return Ok(());
    };
    if allowed.iter().any(|name| name == &normalized) {
        return Ok(());
    }
    let allowed_repr = Value::List(allowed.into_iter().map(Value::Str).collect()).py_repr();
    Err(format!(
        "tool '{normalized}' not in enabled_tools allowlist (enabled: {allowed_repr})"
    ))
}

/// A tool's own `params:`, already compiled -- always [`ParamNode::Map`]
/// (`electricity-compiler::compile::params::build_tool_params` always
/// calls [`ParamNode`]'s own dict-building arm for a `Dict` value, and a
/// document's `params:` is only ever accepted as a mapping at compile
/// time).
fn tool_params_map(params: &ParamNode) -> &IndexMap<Value, ParamNode> {
    match params {
        ParamNode::Map(map) => map,
        _ => unreachable!("electricity-compiler always compiles a tool's params as ParamNode::Map"),
    }
}

/// `core/tool.py::_reject_templated_security_params`'s own dispatch-time
/// literal check, against the *compiled* (unrendered) params tree --
/// `check_no_reference` (`electricity-compiler::compile::params`)
/// already rejects a by-reference `{from: ...}` leaf anywhere inside a
/// security-sensitive key at *compile* time, so the only shapes left for
/// this dispatch-time check to catch are a list item that isn't a plain
/// literal string (compiled to anything but [`ParamNode::Template`]) or
/// one that is a string but still carries literal `{{` template syntax
/// (compiled to [`ParamNode::Template`] either way -- every string param
/// leaf is, regardless of whether it actually uses Mustache syntax; see
/// `electricity-compiler::compile::params::build_param_node`'s own `Str`
/// arm). A value that isn't a list at all is never checked further,
/// matching Python's own `if not isinstance(value, list): continue`.
fn reject_security_sensitive_literal(params: &IndexMap<Value, ParamNode>) -> Result<(), String> {
    let by_reference_text = |key: &str| {
        format!(
            "params.{key} must be a literal list of strings; a by-reference \
             '{{from: ...}}' value is not honoured for this security-sensitive setting."
        )
    };
    for key_name in SECURITY_SENSITIVE_PARAM_KEYS {
        let Some(node) = params.get(&Value::Str(key_name.to_string())) else {
            continue;
        };
        match node {
            ParamNode::List(items) => {
                for item in items {
                    match item {
                        ParamNode::Template(text) => {
                            if text.source.contains("{{") {
                                return Err(format!(
                                    "params.{key_name} must be a literal list of strings; \
                                     a templated value is not honoured for this \
                                     security-sensitive setting."
                                ));
                            }
                        }
                        _ => return Err(by_reference_text(key_name)),
                    }
                }
            }
            // A top-level `{from: ...}` reference would already have
            // failed to compile (`check_no_reference`); kept here anyway
            // so this function stays correct even if that compile-time
            // mirror's own coverage ever narrows.
            ParamNode::From { .. } => return Err(by_reference_text(key_name)),
            _ => {}
        }
    }
    Ok(())
}

/// `core/tool.py::_reject_params_json_security_overrides` -- `Err` the
/// moment a rendered `params_json` overlay tries to set
/// [`SECURITY_SENSITIVE_PARAM_KEYS`] at its own top level.
fn reject_params_json_override(overlay: &Dict) -> Result<(), String> {
    let overridden: Vec<&str> = SECURITY_SENSITIVE_PARAM_KEYS
        .into_iter()
        .filter(|key| overlay.contains_key(&Value::Str(key.to_string())))
        .collect();
    if overridden.is_empty() {
        return Ok(());
    }
    let repr = Value::List(overridden.into_iter().map(Value::from).collect()).py_repr();
    Err(format!(
        "params_json must not set {repr}: security-sensitive settings are only \
         honoured from a document's literal params block."
    ))
}

/// `core/tool.py::_capped_raw` -- redacts *raw*, JSON-round-trips it
/// (`core/tool.py`'s own docstring: callers serialize state with plain
/// `json.dumps`, so a raw value only `default=str` can encode must
/// already be normalized here), then replaces it with the truncation
/// marker once that encoding is over [`electricity_redaction::
/// RAW_META_MAX_BYTES`].
fn capped_raw(raw: Value) -> Value {
    let redacted = electricity_redaction::redact(raw);
    let encoded = match electricity_json::dumps_default_str(
        &redacted,
        electricity_json::WriteMode {
            indent: None,
            sort_keys: false,
            ensure_ascii: false,
            separators: electricity_json::Separators::Default,
        },
    ) {
        Ok(text) => text,
        // `except (TypeError, ValueError): return redacted` -- an
        // unhashable/incomparable dict key, which can't occur for a
        // `Value` already held in a `Dict` (every key already proved
        // hashable to get there).
        Err(_) => return redacted,
    };
    match electricity_redaction::cap_raw(encoded.as_bytes()) {
        None => electricity_json::loads(&encoded).unwrap_or(redacted),
        Some(marker) => {
            let mut dict = Dict::new();
            dict.insert(Value::Str("_truncated".to_string()), Value::Bool(true));
            dict.insert(
                Value::Str("_original_bytes".to_string()),
                Value::from(marker.original_bytes as i64),
            );
            dict.insert(
                Value::Str("_preview".to_string()),
                Value::Str(marker.preview),
            );
            Value::Dict(dict)
        }
    }
}

/// `datetime.now(timezone.utc).isoformat()` -- the conformance/golden-
/// corpus normalizer only ever matches the `YYYY-MM-DDTHH:MM:SS` prefix
/// (`electricity/scripts/_run_corpus.py::_ISO_TS_RE`), so this only has
/// to produce that shape, not byte-identical microsecond/offset text.
fn now_iso() -> Value {
    use chrono::SecondsFormat;
    Value::Str(chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false))
}

/// `adapters/_retry.py::next_backoff_delay_ms`, minus the `Retry-After`
/// override: no HTTP-family tool exists in this crate's own
/// [`electricity_tools::ToolRegistry`] yet, so *retry_info.retry_after*
/// is always `None` for every provider `run_tool` can actually dispatch
/// to, same as every other non-HTTP-family Python plugin already is.
fn full_jitter_backoff_ms(attempt_index: u32, base_ms: u32) -> u32 {
    // `base_ms * 2**attempt_index`: any shift at or beyond 20 already
    // sends the product past `RETRY_BACKOFF_CAP_MS` for every `base_ms`
    // at least 1, so capping the shift there (rather than at the full
    // 63 bits a `u64` could hold) keeps the multiplication itself clear
    // of overflow without changing the clamped result for any ceiling
    // this function could ever actually return.
    let shift = attempt_index.min(20);
    let scaled = u64::from(base_ms).saturating_mul(1u64 << shift);
    let ceiling = scaled.min(u64::from(RETRY_BACKOFF_CAP_MS)) as u32;
    if ceiling == 0 {
        return 0;
    }
    rand::thread_rng().gen_range(0..=ceiling)
}

/// Test-only now: `execute_tool`'s own `setdefault`-shaped check reads
/// the node's own `IndexMap` directly (`contains_key`), since this
/// function's `None`-on-either collapsing can't tell a `Slot::Node`
/// apart from an absent key; only this module's own tests still read a
/// plain leaf value back out this way.
#[cfg(test)]
fn get_leaf(node: &crate::NodeRef, key: &str) -> Option<Value> {
    match node.borrow().get(&Value::Str(key.to_string())) {
        Some(Slot::Value(v)) => Some(v.clone()),
        _ => None,
    }
}

fn pop_leaf(node: &crate::NodeRef, key: &str) {
    node.borrow_mut().shift_remove(&Value::Str(key.to_string()));
}

fn set_leaf(store: &crate::Store, node: &crate::NodeRef, key: &str, value: Value) {
    store.set_leaf(node, Value::Str(key.to_string()), value);
}

/// Everything rendering a tool's `params`/`params_json`/top-level
/// `prompt`/`model` (`core/tool.py::ToolRuntime.execute`'s own render
/// block, up to and including `rendered = {**top_level, **params}`) can
/// fail with, already flattened to its own `Display` text -- the only
/// thing a caller here ever needs it for.
fn render_tool_call(name: &str, tool: &ToolOp, ctx: &Value) -> Result<Dict, String> {
    let mut rendered: Dict = Dict::new();
    if let Some(prompt) = &tool.prompt {
        let text = electricity_template::render_template(
            &prompt.source,
            ctx,
            &electricity_template::PlainCtx,
            "prompt",
        )
        .map_err(|err| err.to_string())?;
        rendered.insert(Value::Str("prompt".to_string()), Value::Str(text));
    }
    if let Some(model) = &tool.model {
        rendered.insert(Value::Str("model".to_string()), Value::Str(model.clone()));
    }

    reject_security_sensitive_literal(tool_params_map(&tool.params))?;
    let mut params = render_params(name, tool_params_map(&tool.params), ctx)
        .map_err(|err: RenderParamsError| err.to_string())?;
    if let Some(params_json) = &tool.params_json {
        let overlay = render_params_json(&params_json.source, ctx)
            .map_err(|err: RenderParamsJsonError| err.to_string())?;
        reject_params_json_override(&overlay)?;
        params = deep_merge_params(params, overlay);
    }
    for (key, value) in params {
        rendered.insert(key, value);
    }
    Ok(rendered)
}

/// A single `tool` effect's own owning entry point -- a close port of
/// `core/tool.py::ToolRuntime.execute`. See this module's own doc
/// comment for the full behavioural mapping and this milestone's
/// deliberately out-of-scope pieces.
#[allow(clippy::too_many_arguments)]
pub async fn execute_tool(
    op: &Op,
    tool: &ToolOp,
    store: &crate::Store,
    parent: &crate::NodeRef,
    ctx: &Value,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
) -> Result<(), VmError> {
    let name = op
        .name
        .as_deref()
        .expect("a tool effect's own Op always carries a name");
    let name_key = Value::Str(name.to_string());

    let effect_node = store
        .ensure_dict(parent, name_key)
        .map_err(|err| VmError::Tool(err.to_string()))?;
    // `node.setdefault("value", None)` -- a plain presence check, unlike
    // `get_leaf`'s own `None`-on-promoted-dict behaviour, so a `Slot::Node`
    // already there (e.g. a nested-dict value written by an earlier
    // attempt) is never clobbered back to a leaf `None`.
    if !effect_node
        .borrow()
        .contains_key(&Value::Str("value".to_string()))
    {
        set_leaf(store, &effect_node, "value", Value::None);
    }
    let meta_node = store
        .ensure_dict(&effect_node, Value::Str("meta".to_string()))
        .map_err(|err| VmError::Tool(err.to_string()))?;

    let timeout_seconds = resolve_timeout_seconds(tool, run_ctx.runtime_config);

    // `store.fire_effect_start(self.defn.name, node)` -- before
    // `require_tool`, so a denial below leaves this call's own
    // start/complete pair unbalanced, exactly as Python's own
    // `AllowlistError` (never caught, so `fire_effect_complete` is never
    // reached either) does.
    observer.effect_start(&op.path);

    let max_attempts = tool.retries.max_attempts.max(1);
    let base_backoff_ms = tool.retries.backoff_ms;

    if let Err(denial) = require_tool(&tool.provider, run_ctx.runtime_config) {
        return Err(VmError::Tool(denial));
    }

    let mut next_delay_ms = base_backoff_ms;
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

        // The per-attempt `meta` reset, in Python's own order.
        set_leaf(store, &meta_node, "created_at", now_iso());
        set_leaf(store, &meta_node, "completed_at", Value::None);
        set_leaf(
            store,
            &meta_node,
            "provider",
            Value::Str(tool.provider.clone()),
        );
        set_leaf(store, &meta_node, "error", Value::None);
        set_leaf(store, &meta_node, "stdout", Value::None);
        set_leaf(store, &meta_node, "stderr", Value::None);
        set_leaf(store, &meta_node, "exit_code", Value::None);
        pop_leaf(&meta_node, "binary");
        pop_leaf(&meta_node, "status_code");
        pop_leaf(&meta_node, "raw");
        pop_leaf(&meta_node, "expect");
        pop_leaf(&meta_node, "retries_used");
        set_leaf(store, &meta_node, "waiting_for", Value::None);
        // Published the moment a fresh attempt's own `meta` is visible,
        // the same way `--live-state` would catch up with Python's own
        // plain dict mutations here (never behind a `store.set` of their
        // own, but always live through the same shared node) -- lane
        // D2's own mirror only ever learns a run changed shape by
        // polling `RunObserver::write`'s own call count, never by
        // diffing the store itself.
        observer.write();

        let mut failure: Option<String> = None;

        match render_tool_call(name, tool, ctx) {
            Err(message) => {
                failure = Some(message);
            }
            Ok(rendered) => {
                let rendered_value = Value::Dict(rendered);
                set_leaf(
                    store,
                    &meta_node,
                    "params_rendered",
                    electricity_redaction::redact(rendered_value.clone()),
                );

                let group = tool.group.as_deref();
                let acquire_future =
                    run_ctx
                        .limiter
                        .acquire_reporting(group, |event| match event {
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
                // Biased: a free slot must be taken even if the token
                // is already cancelled, never raced against it -- Python's
                // own `_acquire_one` (`core/concurrency.py`) takes a free
                // slot with a non-blocking `sem.acquire(blocking=False)`
                // and returns before ever looking at the cancellation
                // token, which is only checked once this call actually
                // has to block. Listing the work branch first only
                // matters because of `biased;`: tokio polls branches in
                // source order and takes the first *ready* one, instead
                // of picking at random among every branch that happens to
                // be ready on the same poll.
                let acquired = tokio::select! {
                    biased;
                    result = acquire_future => Some(result),
                    () = token.cancelled() => None,
                };

                match acquired {
                    None => return Err(VmError::Cancelled),
                    Some(Err(limiter_err)) => {
                        failure = Some(limiter_err.to_string());
                    }
                    Some(Ok(guard)) => {
                        // `run_tool`'s own future is not itself
                        // cancellation-aware (it has no token to check),
                        // so this `select!` is the only thing standing
                        // between a cancelled run and a tool dispatch
                        // that blocks indefinitely -- a hit here, like
                        // every other cancellation point in this
                        // function, is `VmError::Cancelled` and bypasses
                        // `on_error` entirely, never folded into an
                        // ordinary `VmError::Tool` failure.
                        // Biased, for the same reason as the acquire
                        // `select!` above: a `run_tool` future that is
                        // already finished at the first poll (an
                        // instantly-completed json tool, say) must keep
                        // its result, never race a concurrent
                        // cancellation to a coin flip -- Python's own
                        // `ToolRuntime.execute` (`core/tool.py`) has no
                        // cancellation check once a dispatched call has
                        // actually returned, only before the next attempt.
                        let dispatch = tokio::select! {
                            biased;
                            result = run_tool(
                                run_ctx.registry,
                                &tool.provider,
                                rendered_value,
                                timeout_seconds,
                            ) => Some(result),
                            () = token.cancelled() => None,
                        };
                        // Released before this attempt's own backoff
                        // sleep (and before a model-mode `expect:` would
                        // run, if M0-H ever dispatched one) -- a
                        // retrying/`expect`-checking attempt must never
                        // hold its slot while it isn't actually using it.
                        // Also unconditionally the right thing to do on
                        // the cancelled branch above: the slot is never
                        // held past this `select!`, cancelled or not.
                        drop(guard);

                        let Some(dispatch) = dispatch else {
                            return Err(VmError::Cancelled);
                        };
                        match dispatch {
                            Err(err) => failure = Some(err.to_string()),
                            Ok(result) => {
                                let value_for_expect = result.value.clone();
                                set_leaf(store, &effect_node, "value", result.value);
                                set_leaf(
                                    store,
                                    &meta_node,
                                    "stdout",
                                    result.stdout.map(Value::Str).unwrap_or(Value::None),
                                );
                                set_leaf(
                                    store,
                                    &meta_node,
                                    "stderr",
                                    result.stderr.clone().map(Value::Str).unwrap_or(Value::None),
                                );
                                set_leaf(
                                    store,
                                    &meta_node,
                                    "exit_code",
                                    result
                                        .exit_code
                                        .map(|code| Value::from(i64::from(code)))
                                        .unwrap_or(Value::None),
                                );
                                set_leaf(store, &meta_node, "raw", capped_raw(result.raw));

                                if !result.ok {
                                    let message = result
                                        .stderr
                                        .filter(|s| !s.is_empty())
                                        .unwrap_or_else(|| {
                                            format!(
                                                "{} tool reported failure (ok=False)",
                                                tool.provider
                                            )
                                        });
                                    failure = Some(message);
                                } else if let Some(expect) = &tool.expect {
                                    let ExpectCondition::Cel { expr } = expect else {
                                        unreachable!(
                                            "a tool expect in model mode is refused before a run starts"
                                        )
                                    };
                                    let meta_snapshot = store.snapshot(&meta_node);
                                    let mut expect_meta = IndexMap::new();
                                    expect_meta.insert(Value::from("mode"), Value::from("cel"));
                                    expect_meta
                                        .insert(Value::from("expr"), Value::from(expr.as_str()));
                                    match electricity_cel::evaluate_expect(
                                        expr,
                                        &value_for_expect,
                                        &meta_snapshot,
                                        ctx,
                                    ) {
                                        Ok(passed) => {
                                            expect_meta
                                                .insert(Value::from("result"), Value::Bool(passed));
                                            set_leaf(
                                                store,
                                                &meta_node,
                                                "expect",
                                                Value::Dict(expect_meta),
                                            );
                                            if !passed {
                                                failure = Some(format!("expect failed: {expr}"));
                                            }
                                        }
                                        Err(cel_err) => {
                                            expect_meta.insert(
                                                Value::from("error"),
                                                Value::from(cel_err.to_string().as_str()),
                                            );
                                            expect_meta
                                                .insert(Value::from("result"), Value::Bool(false));
                                            set_leaf(
                                                store,
                                                &meta_node,
                                                "expect",
                                                Value::Dict(expect_meta),
                                            );
                                            failure = Some(format!("expect failed: {expr}"));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        set_leaf(store, &meta_node, "completed_at", now_iso());

        if let Some(message) = failure {
            set_leaf(store, &meta_node, "error", Value::Str(message.clone()));
            let is_last_attempt = attempt_index + 1 >= max_attempts;
            // Every failure in M0-H is retryable: the only provider
            // `run_tool` can dispatch to is `json`, never one of
            // `core/tool.py`'s own `_HTTP_FAMILY_PROVIDERS` (this
            // module's own doc comment).
            if !is_last_attempt {
                next_delay_ms = full_jitter_backoff_ms(attempt_index, base_backoff_ms);
                continue;
            }
            if matches!(op.on_error, OnError::Skip | OnError::Continue) {
                set_leaf(store, &effect_node, "value", Value::None);
            }
            observer.effect_complete(&op.path, Some(&message));
            // The final store write for this effect (whichever
            // `on_error` branch below actually returns) -- same
            // contract as the per-attempt reset's own `observer.
            // write()` above, just for the terminal state instead of a
            // fresh attempt's.
            observer.write();
            if op.on_error == OnError::Fail {
                return Err(VmError::Tool(message));
            }
            return Ok(());
        }

        if attempt_index > 0 {
            set_leaf(
                store,
                &meta_node,
                "retries_used",
                Value::from(i64::from(attempt_index)),
            );
        }
        observer.effect_complete(&op.path, None);
        observer.write();
        return Ok(());
    }
    unreachable!("the attempt loop always returns on its last iteration")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unregistered_provider_is_reported_by_name() {
        let registry = ToolRegistry::new();
        let err = run_tool(&registry, "json", Value::None, 300)
            .await
            .unwrap_err();
        assert_eq!(err, VmError::ToolNotFound("json".to_string()));
    }

    #[tokio::test]
    async fn a_provider_name_is_normalized_before_lookup() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(electricity_tools::json::JsonTool));
        let err = run_tool(&registry, "  JSON ", Value::None, 300)
            .await
            .unwrap_err();
        assert!(matches!(err, VmError::Tool(_)));
    }

    #[tokio::test]
    async fn a_registered_providers_own_error_is_wrapped() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(electricity_tools::json::JsonTool));
        let err = run_tool(&registry, "json", Value::None, 300)
            .await
            .unwrap_err();
        assert!(matches!(err, VmError::Tool(_)));
    }

    fn tool_op(params: IndexMap<Value, ParamNode>) -> ToolOp {
        ToolOp {
            provider: "json".to_string(),
            params: ParamNode::Map(params),
            params_json: None,
            prompt: None,
            model: None,
            timeout_ms: None,
            retries: electricity_bytecode::RetryPolicy::default(),
            expect: None,
            description: None,
            group: None,
        }
    }

    fn tool_node(on_error: OnError) -> Op {
        Op {
            path: electricity_bytecode::EffectPath::root().push_name("fetch"),
            name: Some("fetch".to_string()),
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool_op(IndexMap::new())),
            )),
            on_error,
            labels: None,
            enabled: true,
        }
    }

    fn literal(value: Value) -> ParamNode {
        ParamNode::Literal(value)
    }

    fn template(source: &str) -> ParamNode {
        ParamNode::Template(electricity_bytecode::TemplateText::new(
            source.to_string(),
            true,
            electricity_bytecode::Escape::Html,
        ))
    }

    fn default_run_ctx<'a>(
        registry: &'a ToolRegistry,
        limiter: &'a crate::Limiter,
        runtime_config: &'a Value,
    ) -> RunContext<'a> {
        RunContext {
            registry,
            limiter,
            model: "",
            adapter: "_noop",
            runtime_config,
            dry_run: false,
        }
    }

    fn json_registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(electricity_tools::json::JsonTool));
        registry
    }

    #[tokio::test]
    async fn a_successful_call_writes_value_and_meta_in_pythons_own_key_order() {
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("stringify"));
        let mut input = IndexMap::new();
        input.insert(Value::from("a"), literal(Value::from(1i64)));
        params.insert(Value::from("input"), ParamNode::Map(input));

        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool_op(params.clone())),
            )),
            ..tool_node(OnError::Fail)
        };
        let tool = tool_op(params);
        let store = crate::Store::new();
        let registry = json_registry();
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap();

        let snapshot = store.snapshot(&store.root);
        let root = snapshot.as_dict().unwrap();
        let node = root.get(&Value::from("fetch")).unwrap().as_dict().unwrap();
        assert_eq!(
            node.keys().collect::<Vec<_>>(),
            vec![&Value::from("value"), &Value::from("meta"),]
        );
        assert_eq!(
            node.get(&Value::from("value")),
            Some(&Value::from("{\"a\": 1}"))
        );

        let meta = node.get(&Value::from("meta")).unwrap().as_dict().unwrap();
        assert_eq!(
            meta.keys().collect::<Vec<_>>(),
            vec![
                &Value::from("created_at"),
                &Value::from("completed_at"),
                &Value::from("provider"),
                &Value::from("error"),
                &Value::from("stdout"),
                &Value::from("stderr"),
                &Value::from("exit_code"),
                &Value::from("waiting_for"),
                &Value::from("params_rendered"),
                &Value::from("raw"),
            ]
        );
        assert_eq!(meta.get(&Value::from("error")), Some(&Value::None));
        assert_eq!(
            meta.get(&Value::from("provider")),
            Some(&Value::from("json"))
        );
    }

    #[tokio::test]
    async fn an_unknown_mode_fails_and_on_error_fail_propagates() {
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("bogus"));
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool_op(params.clone())),
            )),
            ..tool_node(OnError::Fail)
        };
        let tool = tool_op(params);
        let store = crate::Store::new();
        let registry = json_registry();
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        let err = execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(err, VmError::Tool("json: unknown mode 'bogus'".to_string()));
    }

    #[tokio::test]
    async fn an_unresolved_from_reference_names_the_effect_not_the_provider() {
        // `core/tool.py::ToolRuntime.execute` calls `_render_params`
        // with `name=self.defn.name` (the effect's own name, `fetch`
        // here), never `self.defn.provider` (`json`) -- the message
        // this produces is `meta.error` and the propagated error text.
        let mut params = IndexMap::new();
        params.insert(
            Value::from("n"),
            ParamNode::From {
                path: "input.missing".to_string(),
                default: None,
            },
        );
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool_op(params.clone())),
            )),
            ..tool_node(OnError::Fail)
        };
        let tool = tool_op(params);
        let store = crate::Store::new();
        let registry = json_registry();
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        let err = execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err,
            VmError::Tool(
                "Tool effect 'fetch' param 'params.n': '{from: input.missing}' did not \
                 resolve to a value."
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn an_allowlist_denial_leaves_an_empty_meta_and_no_effect_complete() {
        let op = tool_node(OnError::Fail);
        let tool = tool_op(IndexMap::new());
        let store = crate::Store::new();
        let registry = json_registry();
        let limiter = crate::Limiter::new();
        let mut allowlists = Dict::new();
        let mut tools_entry = Dict::new();
        tools_entry.insert(
            Value::from("tools"),
            Value::List(vec![Value::from("other")]),
        );
        allowlists.insert(Value::from("_allowlists"), Value::Dict(tools_entry));
        let runtime_config = Value::Dict(allowlists);
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        struct CountingObserver(std::cell::Cell<u32>);
        impl RunObserver for CountingObserver {
            fn effect_complete(
                &self,
                _path: &electricity_bytecode::EffectPath,
                _error: Option<&str>,
            ) {
                self.0.set(self.0.get() + 1);
            }
        }
        let observer = CountingObserver(std::cell::Cell::new(0));

        let err = execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &run_ctx,
            &observer,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err,
            VmError::Tool(
                "tool 'json' not in enabled_tools allowlist (enabled: ['other'])".to_string()
            )
        );
        assert_eq!(observer.0.get(), 0);

        let snapshot = store.snapshot(&store.root);
        let node = snapshot
            .as_dict()
            .unwrap()
            .get(&Value::from("fetch"))
            .unwrap()
            .as_dict()
            .unwrap();
        assert_eq!(node.get(&Value::from("value")), Some(&Value::None));
        let meta = node.get(&Value::from("meta")).unwrap().as_dict().unwrap();
        assert!(meta.is_empty());
    }

    #[tokio::test]
    async fn on_error_skip_writes_a_null_value_and_returns_ok() {
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("bogus"));
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool_op(params.clone())),
            )),
            ..tool_node(OnError::Skip)
        };
        let tool = tool_op(params);
        let store = crate::Store::new();
        let registry = json_registry();
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap();

        let snapshot = store.snapshot(&store.root);
        let node = snapshot
            .as_dict()
            .unwrap()
            .get(&Value::from("fetch"))
            .unwrap()
            .as_dict()
            .unwrap();
        assert_eq!(node.get(&Value::from("value")), Some(&Value::None));
        let meta = node.get(&Value::from("meta")).unwrap().as_dict().unwrap();
        assert_eq!(
            meta.get(&Value::from("error")),
            Some(&Value::from("json: unknown mode 'bogus'"))
        );
    }

    #[tokio::test]
    async fn params_json_overriding_allowed_commands_is_refused() {
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("parse"));
        params.insert(Value::from("input"), template("{}"));
        let mut tool = tool_op(params);
        tool.params_json = Some(electricity_bytecode::TemplateText::new(
            "{\"allowed_commands\": [\"ls\"]}".to_string(),
            true,
            electricity_bytecode::Escape::Html,
        ));
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Fail)
        };
        let store = crate::Store::new();
        let registry = json_registry();
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        let err = execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err,
            VmError::Tool(
                "params_json must not set ['allowed_commands']: security-sensitive \
                 settings are only honoured from a document's literal params block."
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn a_templated_allowed_commands_item_is_refused() {
        let allowed = vec![template("rm {{input.target}}")];
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("parse"));
        params.insert(Value::from("input"), template("{}"));
        params.insert(Value::from("allowed_commands"), ParamNode::List(allowed));
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool_op(params.clone())),
            )),
            ..tool_node(OnError::Fail)
        };
        let tool = tool_op(params);
        let store = crate::Store::new();
        let registry = json_registry();
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        let err = execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err,
            VmError::Tool(
                "params.allowed_commands must be a literal list of strings; a templated \
                 value is not honoured for this security-sensitive setting."
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn a_cel_expect_failure_retries_then_fails_with_the_exact_text() {
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("parse"));
        params.insert(Value::from("input"), template("1"));
        let mut tool = tool_op(params);
        tool.retries = electricity_bytecode::RetryPolicy {
            max_attempts: 2,
            backoff_ms: 0,
        };
        tool.expect = Some(ExpectCondition::Cel {
            expr: "value == 2".to_string(),
        });
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Skip)
        };
        let store = crate::Store::new();
        let registry = json_registry();
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap();

        let snapshot = store.snapshot(&store.root);
        let node = snapshot
            .as_dict()
            .unwrap()
            .get(&Value::from("fetch"))
            .unwrap()
            .as_dict()
            .unwrap();
        let meta = node.get(&Value::from("meta")).unwrap().as_dict().unwrap();
        assert_eq!(
            meta.get(&Value::from("error")),
            Some(&Value::from("expect failed: value == 2"))
        );
        let expect_meta = meta.get(&Value::from("expect")).unwrap().as_dict().unwrap();
        assert_eq!(
            expect_meta.get(&Value::from("mode")),
            Some(&Value::from("cel"))
        );
        assert_eq!(
            expect_meta.get(&Value::from("result")),
            Some(&Value::Bool(false))
        );
        assert!(!meta.contains_key(&Value::from("retries_used")));
    }

    #[tokio::test]
    async fn a_raising_cel_expect_fails_with_expect_failed_text_not_the_cel_error() {
        // `core/expect.py::evaluate_expect` catches `CelEvaluationError`
        // itself and returns `passed=False` with the error text folded
        // into `meta.expect.error` -- `tool.py` never sees a raised
        // exception here, so the failure text it writes is the same
        // `expect failed: {expr}` an ordinary failing (non-raising)
        // `expect:` gets, not the CEL evaluator's own error text.
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("parse"));
        params.insert(Value::from("input"), template("{}"));
        let mut tool = tool_op(params);
        tool.retries = electricity_bytecode::RetryPolicy {
            max_attempts: 1,
            backoff_ms: 0,
        };
        tool.expect = Some(ExpectCondition::Cel {
            expr: "value.missing == 1".to_string(),
        });
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Skip)
        };
        let store = crate::Store::new();
        let registry = json_registry();
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap();

        let snapshot = store.snapshot(&store.root);
        let node = snapshot
            .as_dict()
            .unwrap()
            .get(&Value::from("fetch"))
            .unwrap()
            .as_dict()
            .unwrap();
        let meta = node.get(&Value::from("meta")).unwrap().as_dict().unwrap();
        assert_eq!(
            meta.get(&Value::from("error")),
            Some(&Value::from("expect failed: value.missing == 1"))
        );
        let expect_meta = meta.get(&Value::from("expect")).unwrap().as_dict().unwrap();
        assert_eq!(
            expect_meta.get(&Value::from("result")),
            Some(&Value::Bool(false))
        );
        assert!(expect_meta.contains_key(&Value::from("error")));
    }

    #[tokio::test]
    async fn a_passing_cel_expect_succeeds_on_the_first_attempt() {
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("parse"));
        params.insert(Value::from("input"), template("2"));
        let mut tool = tool_op(params);
        tool.expect = Some(ExpectCondition::Cel {
            expr: "value == 2".to_string(),
        });
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Fail)
        };
        let store = crate::Store::new();
        let registry = json_registry();
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap();

        let snapshot = store.snapshot(&store.root);
        let node = snapshot
            .as_dict()
            .unwrap()
            .get(&Value::from("fetch"))
            .unwrap()
            .as_dict()
            .unwrap();
        let meta = node.get(&Value::from("meta")).unwrap().as_dict().unwrap();
        assert_eq!(meta.get(&Value::from("error")), Some(&Value::None));
        let expect_meta = meta.get(&Value::from("expect")).unwrap().as_dict().unwrap();
        assert_eq!(
            expect_meta.get(&Value::from("result")),
            Some(&Value::Bool(true))
        );
    }

    #[tokio::test]
    async fn a_non_string_param_key_survives_to_the_json_tools_own_stringify_boundary() {
        let mut inner = IndexMap::new();
        inner.insert(Value::Bool(true), template("yes-value"));
        inner.insert(Value::from(1i64), template("one-value"));
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("stringify"));
        params.insert(Value::from("input"), ParamNode::Map(inner));
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool_op(params.clone())),
            )),
            ..tool_node(OnError::Fail)
        };
        let tool = tool_op(params);
        let store = crate::Store::new();
        let registry = json_registry();
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap();

        let snapshot = store.snapshot(&store.root);
        let node = snapshot
            .as_dict()
            .unwrap()
            .get(&Value::from("fetch"))
            .unwrap()
            .as_dict()
            .unwrap();
        // `True` and `1` collide as dict keys under Python's own numeric-
        // tower equality, so only the last-written entry survives.
        assert_eq!(
            node.get(&Value::from("value")),
            Some(&Value::from("{\"true\": \"one-value\"}"))
        );
    }

    #[tokio::test]
    async fn a_contended_group_slot_reports_waiting_for_then_clears_it() {
        let op = tool_node(OnError::Fail);
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("parse"));
        params.insert(Value::from("input"), template("1"));
        let mut tool = tool_op(params);
        tool.group = Some("io".to_string());
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..op
        };
        let store = crate::Store::new();
        let registry = json_registry();
        let limiter = crate::Limiter::with_limits(None, [("io".to_string(), 1)]);
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        let holder = limiter.acquire(Some("io")).await.unwrap();

        let effect_node = store
            .ensure_dict(&store.root, Value::from("fetch"))
            .unwrap();
        let meta_node = store
            .ensure_dict(&effect_node, Value::from("meta"))
            .unwrap();

        // `Store.set`'s own `on_write` is what drives `cof run
        // --live-state` (`tool.py`'s `_on_wait`/`_on_acquired`); this
        // crate's own equivalent is `RunObserver::write`, which must
        // fire from inside the `acquire_reporting` callback too, not
        // just from the per-attempt `meta` writes `execute_tool` makes
        // on its own.
        struct CountingObserver(std::cell::Cell<u32>);
        impl RunObserver for CountingObserver {
            fn write(&self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let observer = CountingObserver(std::cell::Cell::new(0));

        let run = async {
            execute_tool(
                &op,
                &tool,
                &store,
                &store.root,
                &Value::None,
                &run_ctx,
                &observer,
                &token,
            )
            .await
            .unwrap();
        };
        tokio::pin!(run);

        for _ in 0..4 {
            tokio::select! {
                _ = &mut run => unreachable!("must not finish while the group slot is held"),
                _ = tokio::task::yield_now() => {}
            }
        }
        assert_eq!(get_leaf(&meta_node, "waiting_for"), Some(Value::from("io")));
        // 2 by now: the per-attempt `meta` reset's own write, then the
        // `Waiting` event's -- not just the latter, since the reset
        // fires its own `observer.write()` too (this same contract,
        // just for the start of a fresh attempt rather than a limiter
        // event).
        assert_eq!(
            observer.0.get(),
            2,
            "the per-attempt reset's own write, then one for the Waiting event"
        );
        drop(holder);
        run.await;
        assert_eq!(get_leaf(&meta_node, "waiting_for"), Some(Value::None));
        // 2 more: `Acquired`, then the final store write once the
        // (successful) attempt completes.
        assert_eq!(
            observer.0.get(),
            4,
            "a write for Acquired, then one more for the final store write"
        );
    }

    /// A test-only [`electricity_tools::ToolPlugin`] that fails its
    /// first *fail_times* calls (an `ok: false` [`electricity_tools::
    /// ToolResult`], never a raised error, so it exercises the `not
    /// result.ok` branch `JsonPlugin` never takes), then succeeds --
    /// covering `meta.retries_used` and the `not ok` failure text, which
    /// the real `json` tool's own deterministic output can never reach
    /// (`ToolRuntime.execute`'s own `retries_used`/`not result.ok`
    /// branches need a provider whose result can differ attempt to
    /// attempt; `json`'s never does).
    struct FlakyTool {
        fail_times: u32,
        calls: std::cell::Cell<u32>,
    }

    #[async_trait::async_trait(?Send)]
    impl electricity_tools::ToolPlugin for FlakyTool {
        fn name(&self) -> &str {
            "flaky"
        }

        async fn execute(
            &self,
            _params: Value,
            _timeout_seconds: u32,
        ) -> Result<electricity_tools::ToolResult, electricity_tools::ToolError> {
            let call = self.calls.get();
            self.calls.set(call + 1);
            if call < self.fail_times {
                let mut result =
                    electricity_tools::ToolResult::new(Value::None, Value::Dict(Dict::new()));
                result.ok = false;
                result.stderr = Some(format!("flaky: attempt {call} failed"));
                return Ok(result);
            }
            Ok(electricity_tools::ToolResult::new(
                Value::from("ok"),
                Value::Dict(Dict::new()),
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

    #[tokio::test]
    async fn a_not_ok_result_fails_without_an_exception_and_retries_record_retries_used() {
        let mut tool = tool_op(IndexMap::new());
        tool.provider = "flaky".to_string();
        tool.retries = electricity_bytecode::RetryPolicy {
            max_attempts: 3,
            backoff_ms: 0,
        };
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Fail)
        };
        let store = crate::Store::new();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FlakyTool {
            fail_times: 1,
            calls: std::cell::Cell::new(0),
        }));
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap();

        let snapshot = store.snapshot(&store.root);
        let node = snapshot
            .as_dict()
            .unwrap()
            .get(&Value::from("fetch"))
            .unwrap()
            .as_dict()
            .unwrap();
        assert_eq!(node.get(&Value::from("value")), Some(&Value::from("ok")));
        let meta = node.get(&Value::from("meta")).unwrap().as_dict().unwrap();
        assert_eq!(meta.get(&Value::from("error")), Some(&Value::None));
        assert_eq!(
            meta.get(&Value::from("retries_used")),
            Some(&Value::from(1i64))
        );
    }

    #[tokio::test]
    async fn a_not_ok_result_with_no_stderr_uses_the_default_failure_text() {
        struct AlwaysNotOk;
        #[async_trait::async_trait(?Send)]
        impl electricity_tools::ToolPlugin for AlwaysNotOk {
            fn name(&self) -> &str {
                "flaky"
            }
            async fn execute(
                &self,
                _params: Value,
                _timeout_seconds: u32,
            ) -> Result<electricity_tools::ToolResult, electricity_tools::ToolError> {
                let mut result =
                    electricity_tools::ToolResult::new(Value::None, Value::Dict(Dict::new()));
                result.ok = false;
                Ok(result)
            }
            fn check(&self) -> electricity_tools::CheckResult {
                electricity_tools::CheckResult {
                    ok: true,
                    missing: Vec::new(),
                    message: None,
                }
            }
        }

        let mut tool = tool_op(IndexMap::new());
        tool.provider = "flaky".to_string();
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Fail)
        };
        let store = crate::Store::new();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(AlwaysNotOk));
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        let err = execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err,
            VmError::Tool("flaky tool reported failure (ok=False)".to_string())
        );
    }

    #[tokio::test]
    async fn a_huge_raw_value_is_capped_through_execute_tools_own_wiring() {
        // The real `json` tool's own `ToolResult.raw` is always a tiny
        // `{"mode": "..."}` -- the 64 KiB cap can never fire through it.
        // This mock plugin returns a `raw` well past the cap so this
        // test exercises `execute_tool`'s own `capped_raw` wiring end to
        // end, not just `electricity_redaction::cap_raw`'s own (already
        // golden-tested) unit behaviour.
        struct HugeRawTool;
        #[async_trait::async_trait(?Send)]
        impl electricity_tools::ToolPlugin for HugeRawTool {
            fn name(&self) -> &str {
                "huge_raw"
            }
            async fn execute(
                &self,
                _params: Value,
                _timeout_seconds: u32,
            ) -> Result<electricity_tools::ToolResult, electricity_tools::ToolError> {
                let mut raw = Dict::new();
                let body = "a".repeat(electricity_redaction::RAW_META_MAX_BYTES + 1);
                raw.insert(Value::from("body"), Value::from(body.as_str()));
                Ok(electricity_tools::ToolResult::new(
                    Value::from("ok"),
                    Value::Dict(raw),
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

        let mut tool = tool_op(IndexMap::new());
        tool.provider = "huge_raw".to_string();
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Fail)
        };
        let store = crate::Store::new();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(HugeRawTool));
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap();

        let snapshot = store.snapshot(&store.root);
        let node = snapshot
            .as_dict()
            .unwrap()
            .get(&Value::from("fetch"))
            .unwrap()
            .as_dict()
            .unwrap();
        let meta = node.get(&Value::from("meta")).unwrap().as_dict().unwrap();
        let raw = meta.get(&Value::from("raw")).unwrap().as_dict().unwrap();
        // Exact values, not just key presence: `{"body": "<N+1 a's>"}`
        // is plain ASCII with no redaction-sensitive shape, so its own
        // `json.dumps` encoding (`core/tool.py::_capped_raw`'s own
        // separators, matched byte for byte against
        // `electricity-json/src/writer.rs`) is computable here directly,
        // the same way `generate_tool_runtime_corpus.py`'s own
        // `cap_raw_cases` compute `_capped_raw`'s own
        // `_original_bytes`/`_preview` fields from a real encoded byte
        // string.
        let encoded = format!(
            "{{\"body\": \"{}\"}}",
            "a".repeat(electricity_redaction::RAW_META_MAX_BYTES + 1)
        );
        assert_eq!(
            raw.get(&Value::from("_truncated")),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            raw.get(&Value::from("_original_bytes")),
            Some(&Value::from(encoded.len() as i64))
        );
        assert_eq!(
            raw.get(&Value::from("_preview")),
            Some(&Value::from(
                &encoded[..electricity_redaction::RAW_META_MAX_BYTES]
            ))
        );
    }

    #[tokio::test]
    // Cancellation point 1 of 3 this module tests directly (the fourth,
    // the synchronous `token.is_set()` fast path in front of a retry --
    // the same guard clause as the `tokio::select!` below, just for a
    // token that was already cancelled before this attempt was even due
    // -- is not independently observable by a black-box test: a token
    // cancelled before `execute_tool` is even called also races this
    // same attempt's own limiter-wait `select!` below, so a test built
    // that way is flaky rather than exercising the fast path in
    // isolation. This test instead cancels *while genuinely suspended*
    // in the backoff sleep -- no race, since `token.cancelled()` is the
    // only ready branch at that point -- which is the deterministic,
    // reachable half of the same guard clause.
    async fn a_cancelled_token_stops_a_queued_retry() {
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("bogus"));
        let mut tool = tool_op(params.clone());
        tool.retries = electricity_bytecode::RetryPolicy {
            max_attempts: 5,
            backoff_ms: 60_000,
        };
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Fail)
        };
        let store = crate::Store::new();
        let registry = json_registry();
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        let run = async {
            execute_tool(
                &op,
                &tool,
                &store,
                &store.root,
                &Value::None,
                &run_ctx,
                &crate::NullObserver,
                &token,
            )
            .await
        };
        tokio::pin!(run);

        for _ in 0..4 {
            tokio::select! {
                _ = &mut run => unreachable!("must still be sleeping its backoff"),
                _ = tokio::task::yield_now() => {}
            }
        }
        token.request(2);
        let err = run.await.unwrap_err();
        assert_eq!(err, VmError::Cancelled);
    }

    #[tokio::test]
    // Cancellation point 2 of 3: the limiter wait (already exercised
    // indirectly by `a_contended_group_slot_reports_waiting_for_then_
    // clears_it`'s own `Waiting` assertions, but this test is the one
    // that actually cancels the token while blocked there and checks
    // the function returns `Err(VmError::Cancelled)`, not an ordinary
    // `VmError::Tool` failure or a successful completion once the slot
    // frees up later).
    async fn a_cancelled_token_stops_a_queued_limiter_wait() {
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("parse"));
        params.insert(Value::from("input"), template("1"));
        let mut tool = tool_op(params);
        tool.group = Some("io".to_string());
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Fail)
        };
        let store = crate::Store::new();
        let registry = json_registry();
        let limiter = crate::Limiter::with_limits(None, [("io".to_string(), 1)]);
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        // Hold the only `io` slot for the whole test -- never dropped,
        // so `execute_tool`'s own wait never resolves on its own.
        let _holder = limiter.acquire(Some("io")).await.unwrap();

        let run = async {
            execute_tool(
                &op,
                &tool,
                &store,
                &store.root,
                &Value::None,
                &run_ctx,
                &crate::NullObserver,
                &token,
            )
            .await
        };
        tokio::pin!(run);

        for _ in 0..4 {
            tokio::select! {
                _ = &mut run => unreachable!("must not finish while the group slot is held"),
                _ = tokio::task::yield_now() => {}
            }
        }
        token.request(2);
        let err = run.await.unwrap_err();
        assert_eq!(err, VmError::Cancelled);
    }

    #[tokio::test]
    // Cancellation point 3 of 3: inside `run_tool`'s own dispatch --
    // `run_tool`'s future carries no token of its own, so this is the
    // one point where a cancellation hit depends entirely on
    // `execute_tool`'s own `select!` racing it against the dispatch,
    // not on anything the dispatched plugin itself checks.
    async fn a_cancelled_token_stops_a_tool_dispatch_in_flight() {
        struct PendingForeverTool;
        #[async_trait::async_trait(?Send)]
        impl electricity_tools::ToolPlugin for PendingForeverTool {
            fn name(&self) -> &str {
                "pending_forever"
            }
            async fn execute(
                &self,
                _params: Value,
                _timeout_seconds: u32,
            ) -> Result<electricity_tools::ToolResult, electricity_tools::ToolError> {
                std::future::pending::<()>().await;
                unreachable!("never resolves")
            }
            fn check(&self) -> electricity_tools::CheckResult {
                electricity_tools::CheckResult {
                    ok: true,
                    missing: Vec::new(),
                    message: None,
                }
            }
        }

        let mut tool = tool_op(IndexMap::new());
        tool.provider = "pending_forever".to_string();
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Fail)
        };
        let store = crate::Store::new();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(PendingForeverTool));
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        let run = async {
            execute_tool(
                &op,
                &tool,
                &store,
                &store.root,
                &Value::None,
                &run_ctx,
                &crate::NullObserver,
                &token,
            )
            .await
        };
        tokio::pin!(run);

        for _ in 0..4 {
            tokio::select! {
                _ = &mut run => unreachable!("must still be waiting on the pending dispatch"),
                _ = tokio::task::yield_now() => {}
            }
        }
        token.request(2);
        let err = run.await.unwrap_err();
        assert_eq!(err, VmError::Cancelled);
    }

    #[tokio::test]
    // The race `biased;` fixes: with a token already cancelled *before*
    // `execute_tool` is even called, a free global slot and an
    // instantly-finished json tool call are both ready works the very
    // first time either `select!` above is polled, same as the
    // concurrent `token.cancelled()` branch -- without `biased;`, tokio
    // picks one of two ready branches at random, so the very same call
    // would sometimes run the tool (matching Python's own
    // `_acquire_one`: a free slot is just taken, never checked against
    // the token) and sometimes return `VmError::Cancelled` instead,
    // depending on nothing but scheduler luck. Run 20 times: every one
    // of them must run the tool, never flip to `Cancelled`.
    async fn a_cancelled_token_before_the_call_still_runs_a_ready_slot_and_tool() {
        for _ in 0..20 {
            let mut params = IndexMap::new();
            params.insert(Value::from("mode"), template("parse"));
            params.insert(Value::from("input"), template("1"));
            let tool = tool_op(params);
            let op = Op {
                kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                    electricity_bytecode::LeafKind::Tool(tool.clone()),
                )),
                ..tool_node(OnError::Fail)
            };
            let store = crate::Store::new();
            let registry = json_registry();
            let limiter = crate::Limiter::new();
            let runtime_config = Value::None;
            let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
            let token = CancellationToken::new();
            token.request(2);

            execute_tool(
                &op,
                &tool,
                &store,
                &store.root,
                &Value::None,
                &run_ctx,
                &crate::NullObserver,
                &token,
            )
            .await
            .unwrap();

            let snapshot = store.snapshot(&store.root);
            let node = snapshot
                .as_dict()
                .unwrap()
                .get(&Value::from("fetch"))
                .unwrap()
                .as_dict()
                .unwrap();
            assert_eq!(node.get(&Value::from("value")), Some(&Value::from(1i64)));
            let meta = node.get(&Value::from("meta")).unwrap().as_dict().unwrap();
            assert_eq!(meta.get(&Value::from("error")), Some(&Value::None));
        }
    }

    #[tokio::test]
    // The same race's other ready branch: with the slot held elsewhere,
    // `acquire_reporting`'s own future is genuinely pending (not ready)
    // at the select's first poll, so `token.cancelled()` is the only
    // ready branch here -- no race, `biased;` only decides *which* ready
    // branch wins when more than one is. This is the deterministic half
    // of the pair, run 20 times for the same reason as its sibling above:
    // a regression that made the acquire select non-biased again would
    // not show up here, only there.
    async fn a_cancelled_token_before_the_call_with_the_slot_held_cancels_after_waiting_for() {
        for _ in 0..20 {
            let mut params = IndexMap::new();
            params.insert(Value::from("mode"), template("parse"));
            params.insert(Value::from("input"), template("1"));
            let mut tool = tool_op(params);
            tool.group = Some("io".to_string());
            let op = Op {
                kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                    electricity_bytecode::LeafKind::Tool(tool.clone()),
                )),
                ..tool_node(OnError::Fail)
            };
            let store = crate::Store::new();
            let registry = json_registry();
            let limiter = crate::Limiter::with_limits(None, [("io".to_string(), 1)]);
            let runtime_config = Value::None;
            let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
            let token = CancellationToken::new();

            // Held for the whole attempt -- never dropped before the
            // call below returns, so the group slot is never actually
            // free for it to take.
            let _holder = limiter.acquire(Some("io")).await.unwrap();
            token.request(2);

            let err = execute_tool(
                &op,
                &tool,
                &store,
                &store.root,
                &Value::None,
                &run_ctx,
                &crate::NullObserver,
                &token,
            )
            .await
            .unwrap_err();
            assert_eq!(err, VmError::Cancelled);

            let snapshot = store.snapshot(&store.root);
            let node = snapshot
                .as_dict()
                .unwrap()
                .get(&Value::from("fetch"))
                .unwrap()
                .as_dict()
                .unwrap();
            let meta = node.get(&Value::from("meta")).unwrap().as_dict().unwrap();
            assert_eq!(
                meta.get(&Value::from("waiting_for")),
                Some(&Value::from("io"))
            );
        }
    }

    #[tokio::test]
    // The fast path a prior fix pass declined to test in isolation (see
    // this module's own cancellation-points doc comment above the first
    // of the three `a_cancelled_token_stops_a_queued_*` tests): the
    // synchronous `token.is_set()` guard in front of a *due* retry's own
    // backoff sleep. Before `biased;`, a token cancelled ahead of the
    // whole call also raced the first attempt's own acquire/dispatch
    // selects, so a test built that way was flaky rather than reaching
    // this guard deterministically. With both of those biased, a
    // pre-cancelled token's first attempt still runs to its own
    // ordinary (non-cancellation) failure, so the second attempt's
    // `is_set()` check is reached every time, before that attempt's own
    // meta reset ever runs -- the stored `meta.error` stays the first
    // attempt's own text, and `retries_used` (only ever set by a
    // *successful* later attempt) is never set at all.
    async fn a_token_cancelled_before_the_call_stops_a_due_retry_before_its_own_backoff() {
        for _ in 0..20 {
            let mut params = IndexMap::new();
            params.insert(Value::from("mode"), template("bogus"));
            let mut tool = tool_op(params);
            tool.retries = electricity_bytecode::RetryPolicy {
                max_attempts: 2,
                backoff_ms: 60_000,
            };
            let op = Op {
                kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                    electricity_bytecode::LeafKind::Tool(tool.clone()),
                )),
                ..tool_node(OnError::Fail)
            };
            let store = crate::Store::new();
            let registry = json_registry();
            let limiter = crate::Limiter::new();
            let runtime_config = Value::None;
            let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
            let token = CancellationToken::new();
            token.request(2);

            let err = execute_tool(
                &op,
                &tool,
                &store,
                &store.root,
                &Value::None,
                &run_ctx,
                &crate::NullObserver,
                &token,
            )
            .await
            .unwrap_err();
            assert_eq!(err, VmError::Cancelled);

            let snapshot = store.snapshot(&store.root);
            let node = snapshot
                .as_dict()
                .unwrap()
                .get(&Value::from("fetch"))
                .unwrap()
                .as_dict()
                .unwrap();
            let meta = node.get(&Value::from("meta")).unwrap().as_dict().unwrap();
            assert_eq!(
                meta.get(&Value::from("error")),
                Some(&Value::from("json: unknown mode 'bogus'"))
            );
            assert!(!meta.contains_key(&Value::from("retries_used")));
        }
    }
}
