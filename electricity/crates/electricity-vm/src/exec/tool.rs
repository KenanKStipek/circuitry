//! `run_tool`: the VM's own tool-dispatch entry point -- looks a `tool`
//! effect's `provider:` up in an [`electricity_tools::ToolRegistry`] and
//! runs it. The dispatch seam itself is final (lane B/C never edit this
//! file to wire a new provider in; they only build the registry they
//! pass to it); *provider* normalization already matches `plugins/
//! factory.py::build_plugin`'s own `.strip().lower()` (below), and the
//! "unknown provider" text is already exact -- [`electricity_tools::
//! build_plugin`]'s own `"Unknown plugin: 'x'. Supported plugins:
//! ..."`, naming Circuitry's full registry (`electricity_tools::
//! plugin_names::PLUGIN_NAMES`), not just this crate's own
//! [`electricity_tools::ToolRegistry`].
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
//! The HTTP-family status-code/`Retry-After` machinery (`core/tool.py`'s
//! own `_HTTP_FAMILY_PROVIDERS`/`_is_retryable_failure`) is lane E's own
//! fill (issue #485): [`http_family_provider`] below, `meta.status_code`
//! and `Retry-After`-aware backoff in [`execute_tool`]'s own result
//! branch. Still deliberately out of scope for M0-H, all because the
//! only provider this milestone ever dispatches through [`run_tool`] is
//! `json` (issue #431's Scope section):
//! - `meta.binary` (`core/tool.py:822-823`) is generic across every
//!   provider instead, so this gate wires it here (M1 gate seam S2):
//!   `json`'s own `ToolResult.raw` never carries a `binary` key, so the
//!   write is still a no-op for every M0 case;
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
    ParamsCtx, RenderParamsError, RenderParamsJsonError, deep_merge_params, render_params,
    render_params_json,
};
#[cfg(test)]
use crate::store::Slot;
use crate::{CancellationToken, Frame, RunContext, RunObserver, VmError};
use electricity_bytecode::{ExpectCondition, OnError, Op, ParamNode, ToolOp};
use electricity_tools::{ToolCall, ToolRegistry};
use electricity_value::{Dict, Value};
use indexmap::IndexMap;

/// Either half of the plugin a `tool` effect ends up dispatching
/// through -- [`resolve_plugin`]'s own result. An embedder's own
/// long-lived [`electricity_tools::ToolPlugin`] instance borrows
/// straight through; one [`electricity_tools::build_plugin`] just
/// built owns its own box (review finding 6 on PR #454: resolving
/// *before* the limiter slot is acquired, as `core/tool.py::
/// ToolRuntime.execute` does, needs a value this enum's own caller can
/// hold across that wait -- a bare `&dyn ToolPlugin` can't borrow from
/// a `Box` [`resolve_plugin`] itself only just created and is about to
/// drop).
pub enum ResolvedPlugin<'a> {
    Registered(&'a dyn electricity_tools::ToolPlugin),
    Built(Box<dyn electricity_tools::ToolPlugin>),
}

impl ResolvedPlugin<'_> {
    fn as_dyn(&self) -> &dyn electricity_tools::ToolPlugin {
        match self {
            ResolvedPlugin::Registered(plugin) => *plugin,
            ResolvedPlugin::Built(plugin) => plugin.as_ref(),
        }
    }
}

/// Looks *provider* (a tool effect's own `provider:`) up in *registry*,
/// falling through to [`electricity_tools::build_plugin`] -- `core/
/// tool.py::ToolRuntime.execute`'s own `build_plugin(...)` call,
/// `tool.py:702`, which runs *before* that same attempt's limiter wait
/// (`tool.py:724`) -- [`execute_tool`] calls this first, outside its
/// own acquire `select!`, so a per-plugin config error fails the
/// attempt immediately, never waiting for a concurrency slot it will
/// only turn around and release unused (review finding 6 on PR #454).
///
/// *registry* (still [`crate::RunContext::registry`]) is tried first --
/// an embedder's own long-lived [`electricity_tools::ToolPlugin`]
/// instance (or a test double, as this module's own unit tests below
/// still register) always wins when one is registered under
/// *provider*'s own normalized name. Only a *provider* with no such
/// entry falls through to [`electricity_tools::build_plugin`] (issue
/// #449's gate lane item 2) -- called fresh on *this* call, i.e. once
/// per attempt, exactly where `core/tool.py::ToolRuntime.execute` calls
/// Python's own `build_plugin` every attempt -- so a per-plugin config
/// error on that path fails just the one attempt. `json` is never
/// registered as a singleton on the production path
/// (`electricity::tool_registry`) for exactly this reason: every
/// attempt builds its own fresh [`electricity_tools::json::JsonTool`]
/// through this same fallback, the way any other `build_plugin`-only
/// provider would.
///
/// `Err` is a typed [`electricity_tools::ToolError`] (review finding 1
/// on PR #454 -- a previous revision wrapped it into a bare
/// `VmError::Tool(String)` right here, throwing its own *py_class*/
/// *cause* away before [`execute_tool`] could ever classify a failure
/// for retry or report its real exception class) -- `build_plugin`'s
/// own exact unknown-plugin text (naming Circuitry's **full**
/// registry, [`electricity_tools::PyExc::ValueError`]) for a *provider*
/// neither *registry* nor `build_plugin` knows.
pub fn resolve_plugin<'a>(
    registry: &'a ToolRegistry,
    provider: &str,
    runtime_config: &Value,
) -> Result<ResolvedPlugin<'a>, electricity_tools::ToolError> {
    let normalized = electricity_value::pycompat::py_strip(provider).to_lowercase();
    if let Some(plugin) = registry.get(&normalized) {
        return Ok(ResolvedPlugin::Registered(plugin));
    }
    let plugin = electricity_tools::build_plugin(&normalized, runtime_config)?;
    Ok(ResolvedPlugin::Built(plugin))
}

/// Dispatches *params* (already rendered -- lane B/C's own `params.rs`:
/// `{from: ...}` resolved, templates rendered, `params_json`
/// deep-merged; out of scope for this function) through *plugin*
/// ([`resolve_plugin`]'s own result, resolved by the caller *before*
/// this call). *runtime_config* is the merged `runtime:` block
/// [`crate::RunContext::runtime_config`] carries, read again here only
/// for the [`electricity_tools::ToolCall::config`] slice *plugin*'s own
/// `execute` receives (`resolve_plugin`'s own build already read it
/// once, for the build itself; a plugin's own `execute`, e.g. `shell`'s
/// host pin, can still need the same slice again at dispatch time).
///
/// `Err` is the resolved plugin's own raised
/// [`electricity_tools::ToolError`], unchanged. [`execute_tool`] is the
/// one place this crate ever turns it into a [`VmError::Raised`].
pub async fn run_tool(
    plugin: &ResolvedPlugin<'_>,
    provider: &str,
    params: Value,
    runtime_config: &Value,
    timeout_seconds: u32,
    token: &CancellationToken,
    armed: bool,
) -> Result<electricity_tools::ToolResult, electricity_tools::ToolError> {
    let normalized = electricity_value::pycompat::py_strip(provider).to_lowercase();
    let config = electricity_tools::registry::plugin_config_slice(runtime_config, &normalized);
    let call = ToolCall {
        timeout_seconds,
        config: &config,
        token,
        armed,
    };
    plugin.as_dyn().execute(params, &call).await
}

/// `core/tool.py::DEFAULT_TOOL_TIMEOUT_SECONDS`.
const DEFAULT_TOOL_TIMEOUT_SECONDS: u32 = 300;

/// `core/tool.py::_SECURITY_SENSITIVE_PARAM_KEYS`.
const SECURITY_SENSITIVE_PARAM_KEYS: [&str; 1] = ["allowed_commands"];

/// `core/tool.py::_HTTP_FAMILY_PROVIDERS` -- a local copy, not a call
/// into `crate::retry`'s own private `is_http_family_provider` (issue
/// #485's own scope note: `retry.rs` is final except for
/// `parse_retry_after_seconds`'s own widening). *provider* is the
/// tool's own `provider:` as written, never normalized -- a ` HTTP `
/// provider still dispatches the `http` plugin (`resolve_plugin`'s own
/// `.strip().lower()`) but is never classified HTTP family, so
/// `meta.status_code` is never written for it and every one of its
/// failures stays unconditionally retryable, exactly as
/// `crate::retry::is_tool_failure_retryable` already keeps for any
/// non-HTTP-family provider.
fn http_family_provider(provider: &str) -> bool {
    matches!(provider, "http" | "web_fetch" | "webhook" | "linear")
}

/// `core/tool.py::ToolRuntime._resolve_timeout_seconds`. *runtime_config*
/// is the merged `runtime:` block [`RunContext::runtime_config`] carries;
/// lane D is the one that ever populates `runtime.tools.timeout_seconds`
/// in it, so this always falls through to [`DEFAULT_TOOL_TIMEOUT_SECONDS`]
/// until then. A `timeout_ms: {from: ...}` is read from *ctx* here, for
/// each attempt (issue #564), and fails with the text `tool`'s own
/// `meta.error` records.
fn resolve_timeout_seconds<R: ParamsCtx>(
    tool: &ToolOp,
    name: &str,
    ctx: &R,
    runtime_config: &Value,
) -> Result<u32, String> {
    let timeout_ms = crate::exec::resolve_timeout_ms(
        tool.timeout_ms.as_ref(),
        ctx,
        &format!("Tool effect '{name}'"),
    )?;
    Ok(literal_timeout_seconds(timeout_ms, runtime_config))
}

/// The rest of `_resolve_timeout_seconds`, once `timeout_ms` is a number.
fn literal_timeout_seconds(timeout_ms: Option<i64>, runtime_config: &Value) -> u32 {
    // `if self.defn.timeout_ms:` -- falsy, so an explicit `timeout_ms: 0`
    // falls through to `runtime.tools.timeout_seconds` exactly like an
    // absent one does. A negative `timeout_ms` is still truthy (issue
    // #536): `crate::exec::effect_timeout_seconds` ports the same
    // `max(1, ceil(ms / 1000))` rule cof applies to it.
    if let Some(ms) = timeout_ms.filter(|&ms| ms != 0) {
        return crate::exec::effect_timeout_seconds(ms);
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
            // `except (TypeError, ValueError): logger.warning("Invalid
            // runtime.tools.timeout_seconds=%r; using %ds", raw,
            // DEFAULT_TOOL_TIMEOUT_SECONDS); return
            // DEFAULT_TOOL_TIMEOUT_SECONDS` (issue #442).
            None => {
                log::warn!(
                    "Invalid runtime.tools.timeout_seconds={}; using {}s",
                    value.py_repr(),
                    DEFAULT_TOOL_TIMEOUT_SECONDS
                );
                DEFAULT_TOOL_TIMEOUT_SECONDS
            }
        },
    }
}

/// `int(raw)`, as far as [`resolve_timeout_seconds`] needs it: `bool`/
/// `int`/`float` coerce the way CPython's `int()` does (including
/// negative values -- the caller's own `max(1, ...)` floors those, just
/// as Python's does), a numeric `str` parses the same way (PEP 515
/// underscores -- `"1_000"` -- included, via
/// [`strip_python_int_underscores`]); anything else (including a
/// non-numeric `str`, a hex literal like `"0x10"`, or a float literal
/// like `"1.5"` -- `int()` takes a base-10 digit string only) is
/// `None`, this function's caller's cue to fall back to the default
/// rather than Python's own `ValueError`/`TypeError` text (never
/// surfaced in state, so there is nothing here to match byte for
/// byte).
fn python_int_floor(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Int(i) => i.to_string().parse().ok(),
        Value::Float(f) if f.is_finite() => Some(f.trunc() as i64),
        Value::Str(s) => strip_python_int_underscores(s.trim())?.parse().ok(),
        _ => None,
    }
}

/// Python's PEP 515 digit-separator rule, as far as
/// [`python_int_floor`] needs it: an `_` is valid only directly between
/// two ASCII digits -- never leading, trailing, doubled, or next to a
/// sign. Returns `None` (an invalid literal, same as CPython's `int()`
/// would raise on it) on any other placement; otherwise every `_` is
/// stripped and the remaining text returned. A value with no `_` at all
/// is returned unchanged.
fn strip_python_int_underscores(s: &str) -> Option<String> {
    if !s.contains('_') {
        return Some(s.to_string());
    }
    let chars: Vec<char> = s.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c != '_' {
            continue;
        }
        let prev_digit = i > 0 && chars[i - 1].is_ascii_digit();
        let next_digit = i + 1 < chars.len() && chars[i + 1].is_ascii_digit();
        if !prev_digit || !next_digit {
            return None;
        }
    }
    Some(chars.into_iter().filter(|c| *c != '_').collect())
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
    let normalized = electricity_value::pycompat::py_strip(provider).to_lowercase();
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
fn render_tool_call<R: ParamsCtx>(
    name: &str,
    tool: &ToolOp,
    ctx: &R,
    declared_prompts: &indexmap::IndexMap<String, String>,
    effect_names: &std::collections::BTreeSet<String>,
) -> Result<Dict, String> {
    let mut rendered: Dict = Dict::new();
    if let Some(prompt) = &tool.prompt {
        let text = crate::compose::render_with_composition(
            &prompt.source,
            ctx,
            declared_prompts,
            effect_names,
            "prompt",
            &electricity_template::PlainCtx,
            matches!(prompt.escape, electricity_bytecode::Escape::Html),
        )?;
        rendered.insert(Value::Str("prompt".to_string()), Value::Str(text));
    }
    if let Some(model) = &tool.model {
        rendered.insert(Value::Str("model".to_string()), Value::Str(model.clone()));
    }

    reject_security_sensitive_literal(tool_params_map(&tool.params))?;
    let mut params = render_params(
        name,
        tool_params_map(&tool.params),
        ctx,
        declared_prompts,
        effect_names,
    )
    .map_err(|err: RenderParamsError| err.to_string())?;
    if let Some(params_json) = &tool.params_json {
        let overlay = render_params_json(&params_json.source, ctx, declared_prompts, effect_names)
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
pub async fn execute_tool<R: ParamsCtx>(
    op: &Op,
    tool: &ToolOp,
    store: &crate::Store,
    parent: &crate::NodeRef,
    ctx: &R,
    frame: &Frame,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
) -> Result<(), VmError> {
    // `ToolRuntime.__init__`'s own `drops_parsed_stdout` read (#592).
    // Validated before the run starts (`pre_input_checks`); a bad value that
    // got here anyway fails the run rather than recording the text in full.
    let drop_parsed_stdout = electricity_config::drops_parsed_stdout(Some(run_ctx.runtime_config))
        .map_err(|message| VmError::Raised {
            py_class: electricity_tools::PyExc::ValueError,
            message,
            cause: electricity_tools::ErrorCause::None,
            cause_class: None,
        })?;
    let name = op
        .name
        .as_deref()
        .expect("a tool effect's own Op always carries a name");
    let name_key = Value::Str(name.to_string());

    let effect_node = store
        .ensure_dict(parent, name_key)
        .map_err(|err| VmError::tool(err.to_string()))?;
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
        .map_err(|err| VmError::tool(err.to_string()))?;

    // `store.fire_effect_start(self.defn.name, node)` -- before
    // `require_tool`, so a denial below leaves this call's own
    // start/complete pair unbalanced, exactly as Python's own
    // `AllowlistError` (never caught, so `fire_effect_complete` is never
    // reached either) does.
    let effect_path = frame.effect_path(&op.path);
    let instance = observer.effect_start(&effect_path);

    let max_attempts = tool.retries.max_attempts.max(1);
    let base_backoff_ms = tool.retries.backoff_ms;

    if let Err(denial) = require_tool(&tool.provider, run_ctx.runtime_config) {
        return Err(VmError::tool(denial));
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

        // The per-attempt `meta` reset, in Python's own order. `created_at`
        // is set only on this pass's first attempt (#421): the start of a
        // retried tool's final attempt is not when it started.
        if attempt_index == 0 {
            set_leaf(store, &meta_node, "created_at", now_iso());
        }
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
        pop_leaf(&meta_node, "stdout_chars");
        pop_leaf(&meta_node, "stdout_sha256");
        set_leaf(store, &meta_node, "waiting_for", Value::None);
        // Published the moment a fresh attempt's own `meta` is visible,
        // the same way `--live-state` would catch up with Python's own
        // plain dict mutations here (never behind a `store.set` of their
        // own, but always live through the same shared node) -- lane
        // D2's own mirror only ever learns a run changed shape by
        // polling `RunObserver::write`'s own call count, never by
        // diffing the store itself.
        observer.write();

        let mut failure: Option<electricity_tools::ToolError> = None;
        // Lane E (issue #485): `meta.status_code`'s own value, when a
        // returned result's `raw.status` was an int *and* this
        // provider is HTTP family (`is_http_family_provider`, below).
        // Threaded past the big `match` below so the retry
        // classification after it can pass the real status instead of
        // M0-H's own hard-coded `None` (`core/tool.py::ToolRuntime.
        // _is_retryable_failure`'s own *status_code* argument).
        let mut http_status_for_retry: Option<u16> = None;
        // The plugin's own *original* `result.raw.headers`'s own
        // `retry-after`/`Retry-After` value, captured before
        // `capped_raw` redacts/consumes `result.raw` below. Only ever
        // set on an `ok: false` returned result -- never a raised
        // failure or a failed `expect:` (issue #485's own ruling).
        let mut retry_after_for_backoff: Option<String> = None;
        // `core/tool.py:838-840`'s own `retryable = True` on *any* failed
        // `expect:` check, raised or not, overriding whatever this
        // provider's own HTTP-family status/cause classification would
        // otherwise say -- unlike an HTTP status, a failed `expect:`
        // carries no classification of its own, and the motivating case
        // (a flaky image-generation result) is exactly "try again"
        // (review P2-8 on PR #454's second review). Set only in the two
        // `expect` branches below, never the dispatch-failure branch
        // above them.
        let mut expect_failed = false;
        // `core/tool.py`'s own `timeout_unresolved` (issue #564): a
        // `timeout_ms: {from: ...}` with no usable value is never
        // retried -- a retry reads the same state.
        let mut timeout_unresolved = false;

        match render_tool_call(
            name,
            tool,
            ctx,
            run_ctx.declared_prompts,
            run_ctx.effect_names,
        ) {
            Err(message) => {
                failure = Some(electricity_tools::ToolError::message(message));
            }
            Ok(rendered) => {
                let rendered_value = Value::Dict(rendered);
                set_leaf(
                    store,
                    &meta_node,
                    "params_rendered",
                    electricity_redaction::redact(rendered_value.clone()),
                );

                // This attempt's own timeout budget, read after the params
                // render and before the plugin is built, as in Python
                // (issue #564).
                let timeout_result =
                    resolve_timeout_seconds(tool, name, ctx, run_ctx.runtime_config);
                timeout_unresolved = timeout_result.is_err();
                let timeout_seconds = timeout_result.clone().unwrap_or_default();

                // `core/tool.py::ToolRuntime.execute`'s own
                // `build_plugin(...)` call (`tool.py:702`) runs *before*
                // that same attempt's limiter wait (`tool.py:724`) --
                // resolved here, outside the acquire `select!` below, so
                // a per-plugin config error fails this attempt at once,
                // never waiting for (and then releasing unused) a
                // concurrency slot (review finding 6 on PR #454).
                let plugin_result = match timeout_result {
                    Err(message) => Err(electricity_tools::ToolError {
                        py_class: electricity_tools::PyExc::ValueError,
                        message,
                        cause: electricity_tools::ErrorCause::None,
                    }),
                    Ok(_) => {
                        resolve_plugin(run_ctx.registry, &tool.provider, run_ctx.runtime_config)
                    }
                };
                match plugin_result {
                    Err(err) => {
                        failure = Some(err);
                    }
                    Ok(resolved) => {
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
                                failure = Some(electricity_tools::ToolError::message(
                                    limiter_err.to_string(),
                                ));
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
                                // ordinary `VmError::Raised` failure.
                                // Biased, for the same reason as the acquire
                                // `select!` above: a `run_tool` future that is
                                // already finished at the first poll (an
                                // instantly-completed json tool, say) must keep
                                // its result, never race a concurrent
                                // cancellation to a coin flip -- Python's own
                                // `ToolRuntime.execute` (`core/tool.py`) has no
                                // cancellation check once a dispatched call has
                                // actually returned, only before the next attempt.
                                //
                                let dispatch = tokio::select! {
                                    biased;
                                    result = run_tool(
                                        &resolved,
                                        &tool.provider,
                                        rendered_value,
                                        run_ctx.runtime_config,
                                        timeout_seconds,
                                        token,
                                        run_ctx.armed,
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
                                // Review finding 6 on PR #467: a process-
                                // backed tool's own `run_tracked` returns
                                // `Err(ProcError::Cancelled)` *synchronously*
                                // -- before its first `.await` -- the moment
                                // cancellation was already set when dispatch
                                // started, which `subprocess_tool.rs`/
                                // `shell.rs`/`ffmpeg.rs` all turn into the
                                // exact same `ToolError::message("cancelled")`.
                                // Against `biased;` above, that makes the
                                // `run_tool(...)` branch already `Ready` on
                                // the select!'s very first poll, so it
                                // always wins over `token.cancelled()` in
                                // exactly that case -- turning a real
                                // cancellation into an ordinary, `on_error`-
                                // recordable tool failure instead of
                                // `VmError::Cancelled`. Caught here by its
                                // exact, narrow text rather than by skipping
                                // dispatch outright: the `a_cancelled_token_
                                // before_the_call_still_runs_a_ready_slot_
                                // and_tool` test below is deliberate, tested
                                // Python parity (`core/tool.py`'s own
                                // `_acquire_one` just takes a free slot and
                                // dispatches, with no cancellation check of
                                // its own at all) that an ordinary tool
                                // dispatched with the token already
                                // cancelled must still run to completion --
                                // this only ever intercepts the one shape a
                                // process-backed tool's own cancellation path
                                // produces, never an ordinary tool's
                                // unrelated success or failure.
                                if token.is_set()
                                    && matches!(&dispatch, Err(err) if err.message == "cancelled")
                                {
                                    return Err(VmError::Cancelled);
                                }
                                match dispatch {
                                    Err(err) => failure = Some(err),
                                    Ok(result) => {
                                        let value_for_expect = result.value.clone();
                                        set_leaf(store, &effect_node, "value", result.value);
                                        match result.stdout {
                                            // `runtime.state.parsed_stdout: drop`
                                            // (#592): the parsed text's length
                                            // and digest, never the text.
                                            Some(stdout)
                                                if drop_parsed_stdout && result.stdout_parsed =>
                                            {
                                                set_leaf(store, &meta_node, "stdout", Value::None);
                                                set_leaf(
                                                    store,
                                                    &meta_node,
                                                    "stdout_chars",
                                                    Value::from(stdout.chars().count() as i64),
                                                );
                                                set_leaf(
                                                    store,
                                                    &meta_node,
                                                    "stdout_sha256",
                                                    Value::Str(super::use_::sha256_hex(
                                                        stdout.as_bytes(),
                                                    )),
                                                );
                                            }
                                            stdout => set_leaf(
                                                store,
                                                &meta_node,
                                                "stdout",
                                                stdout.map(Value::Str).unwrap_or(Value::None),
                                            ),
                                        }
                                        set_leaf(
                                            store,
                                            &meta_node,
                                            "stderr",
                                            result
                                                .stderr
                                                .clone()
                                                .map(Value::Str)
                                                .unwrap_or(Value::None),
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
                                        // Lane E (issue #485): `meta.status_code`,
                                        // HTTP-family providers only (`core/
                                        // tool.py`'s own `_HTTP_FAMILY_PROVIDERS`
                                        // gate), the provider's own *as-written*
                                        // spelling, and only when `raw.status` is
                                        // an int -- written for a 2xx result too,
                                        // not just a failing one. `retry_after` is
                                        // read from the plugin's own *unredacted*
                                        // `raw.headers` here, before `capped_raw`
                                        // below redacts/consumes `result.raw`.
                                        if http_family_provider(&tool.provider) {
                                            if let Some(Value::Int(
                                                electricity_value::IntValue::Small(n),
                                            )) = result.raw.as_dict().and_then(|d| {
                                                d.get(&Value::Str("status".to_string()))
                                            }) {
                                                if let Ok(status_u16) = u16::try_from(*n) {
                                                    http_status_for_retry = Some(status_u16);
                                                    set_leaf(
                                                        store,
                                                        &meta_node,
                                                        "status_code",
                                                        Value::Int(
                                                            electricity_value::IntValue::Small(*n),
                                                        ),
                                                    );
                                                }
                                            }
                                            if !result.ok {
                                                retry_after_for_backoff = result
                                                    .raw
                                                    .as_dict()
                                                    .and_then(|d| {
                                                        d.get(&Value::Str("headers".to_string()))
                                                    })
                                                    .and_then(|h| h.as_dict())
                                                    .and_then(|headers| {
                                                        headers
                                                            .get(&Value::Str(
                                                                "retry-after".to_string(),
                                                            ))
                                                            .or_else(|| {
                                                                headers.get(&Value::Str(
                                                                    "Retry-After".to_string(),
                                                                ))
                                                            })
                                                    })
                                                    .and_then(|v| match v {
                                                        Value::Str(s) => Some(s.clone()),
                                                        _ => None,
                                                    });
                                            }
                                        }
                                        // Generic across every provider (`core/
                                        // tool.py:822-823`): a `meta.binary` write
                                        // this gate owns, not lane C/E's own HTTP-
                                        // family status-code machinery -- `json`
                                        // never sets `raw.binary`, so this is a
                                        // no-op for M0's own output; a later
                                        // provider (`shell`, `http`, ...) whose
                                        // `raw` carries one needs no change here.
                                        if let Some(binary) = result
                                            .raw
                                            .as_dict()
                                            .and_then(|d| d.get(&Value::Str("binary".to_string())))
                                            .cloned()
                                        {
                                            set_leaf(store, &meta_node, "binary", binary);
                                        }
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
                                            failure = Some(electricity_tools::ToolError::message(
                                                message,
                                            ));
                                        } else if let Some(expect) = &tool.expect {
                                            match expect {
                                                ExpectCondition::Cel { expr } => {
                                                    let meta_snapshot = store.snapshot(&meta_node);
                                                    let mut expect_meta = IndexMap::new();
                                                    expect_meta.insert(
                                                        Value::from("mode"),
                                                        Value::from("cel"),
                                                    );
                                                    expect_meta.insert(
                                                        Value::from("expr"),
                                                        Value::from(expr.as_str()),
                                                    );
                                                    // Deliberately *not* narrowed through
                                                    // `exec::live_scope::build_cel_state`: unlike
                                                    // `evaluate_condition`, `evaluate_expect`
                                                    // converts its whole `state` with no
                                                    // internal `project` of its own, so an unread
                                                    // big int elsewhere in `state` already makes
                                                    // it raise today (its own doc comment) --
                                                    // narrowing here would silently drop that and
                                                    // change behaviour, which issue #463 is
                                                    // explicitly not about.
                                                    let state = ctx.whole();
                                                    match electricity_cel::evaluate_expect(
                                                        expr,
                                                        &value_for_expect,
                                                        &meta_snapshot,
                                                        &state,
                                                    ) {
                                                        Ok(passed) => {
                                                            expect_meta.insert(
                                                                Value::from("result"),
                                                                Value::Bool(passed),
                                                            );
                                                            set_leaf(
                                                                store,
                                                                &meta_node,
                                                                "expect",
                                                                Value::Dict(expect_meta),
                                                            );
                                                            if !passed {
                                                                expect_failed = true;
                                                                failure = Some(
                                                                    electricity_tools::ToolError::message(
                                                                        format!("expect failed: {expr}"),
                                                                    ),
                                                                );
                                                            }
                                                        }
                                                        // `core/cel_eval.py::evaluate_cel_expect`
                                                        // converts `value`/`meta`/`state` before
                                                        // its own `try`/`except`, so the matching
                                                        // `ValueError("overflow")` reaches
                                                        // `core/tool.py` uncaught -- never a
                                                        // `CelEvaluationError`, never a failed
                                                        // expectation. `core/tool.py`'s own
                                                        // `except Exception as expect_exc:
                                                        // failure = expect_exc` catches it exactly
                                                        // like any other attempt failure (retried
                                                        // up to this step's own `retries:`
                                                        // policy), but `meta.expect` is never set
                                                        // (the escape happens before
                                                        // `core/expect.py` would set it) and
                                                        // `meta.error`/the bare, uncaught
                                                        // `ValueError` an exhausted `on_error:
                                                        // fail` re-raises both read "overflow",
                                                        // not "expect failed: ...".
                                                        Err(cel_err) if cel_err.is_overflow() => {
                                                            expect_failed = true;
                                                            failure = Some(electricity_tools::ToolError {
                                                                py_class: electricity_tools::PyExc::ValueError,
                                                                message: "overflow".to_string(),
                                                                cause: electricity_tools::ErrorCause::None,
                                                            });
                                                        }
                                                        Err(cel_err) => {
                                                            expect_meta.insert(
                                                                Value::from("error"),
                                                                Value::from(
                                                                    cel_err.to_string().as_str(),
                                                                ),
                                                            );
                                                            expect_meta.insert(
                                                                Value::from("result"),
                                                                Value::Bool(false),
                                                            );
                                                            set_leaf(
                                                                store,
                                                                &meta_node,
                                                                "expect",
                                                                Value::Dict(expect_meta),
                                                            );
                                                            expect_failed = true;
                                                            failure = Some(
                                                                electricity_tools::ToolError::message(
                                                                    format!("expect failed: {expr}"),
                                                                ),
                                                            );
                                                        }
                                                    }
                                                }
                                                // `condition_model::evaluate_model_expect` --
                                                // issue #513. *timeout_seconds* is this tool's
                                                // own resolved timeout (`resolve_timeout_seconds`
                                                // above), never the run adapter budget (that's
                                                // only for `if`/`while`/a `use` expect). A
                                                // `Raised` error (a render or adapter failure)
                                                // is always retried here, same as every other
                                                // dispatch failure this provider's own retry
                                                // loop already handles -- only `Fatal` (a
                                                // cancellation) returns early.
                                                ExpectCondition::Model { template } => {
                                                    let meta_snapshot = store.snapshot(&meta_node);
                                                    let state = ctx.whole();
                                                    match super::condition_model::evaluate_model_expect(
                                                        template,
                                                        &value_for_expect,
                                                        &meta_snapshot,
                                                        &state,
                                                        &effect_path,
                                                        timeout_seconds,
                                                        run_ctx,
                                                        token,
                                                    )
                                                    .await
                                                    {
                                                        Ok(outcome) => {
                                                            let summary =
                                                                super::condition_model::expect_model_summary(
                                                                    &template.source,
                                                                );
                                                            set_leaf(
                                                                store,
                                                                &meta_node,
                                                                "expect",
                                                                outcome.meta,
                                                            );
                                                            if !outcome.passed {
                                                                expect_failed = true;
                                                                failure = Some(
                                                                    electricity_tools::ToolError::message(
                                                                        format!("expect failed: {summary}"),
                                                                    ),
                                                                );
                                                            }
                                                        }
                                                        Err(super::condition_model::ExpectCallError::Fatal(e)) => {
                                                            return Err(e);
                                                        }
                                                        Err(super::condition_model::ExpectCallError::Raised {
                                                            message,
                                                            py_class,
                                                        }) => {
                                                            expect_failed = true;
                                                            failure = Some(electricity_tools::ToolError {
                                                                py_class,
                                                                message,
                                                                cause: electricity_tools::ErrorCause::None,
                                                            });
                                                        }
                                                    }
                                                }
                                            }
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

        if let Some(failure) = failure {
            set_leaf(
                store,
                &meta_node,
                "error",
                Value::Str(failure.message.clone()),
            );
            let is_last_attempt = attempt_index + 1 >= max_attempts;
            // `core/tool.py::ToolRuntime._is_retryable_failure` -- every
            // non-HTTP-family provider (every provider this crate can
            // build today: `json`, plus `sleep`/`fail` under
            // `test-tools`) retries on any failure, unchanged from
            // M0-H; lane E's own HTTP-family tool is what first makes
            // *retryable* ever come back `false` here for a dispatch
            // failure -- never for a failed `expect:`, which is always
            // retryable regardless of provider (review P2-8 on PR
            // #454's second review, `expect_failed` above).
            let retryable = !timeout_unresolved
                && crate::retry::is_tool_failure_retryable(
                    &tool.provider,
                    http_status_for_retry,
                    Some(&failure.cause),
                    expect_failed,
                );
            if !is_last_attempt && retryable {
                // Lane E (issue #485): a provider's own `Retry-After`
                // overrides the computed backoff -- only ever present
                // for an `ok: false` returned result
                // (`retry_after_for_backoff`'s own doc comment above:
                // never set for a raised failure or a failed
                // `expect:`).
                next_delay_ms = crate::retry::next_backoff_delay_ms(
                    &crate::retry::RetryInfo {
                        retryable: true,
                        status: http_status_for_retry,
                        retry_after: retry_after_for_backoff.clone(),
                    },
                    attempt_index,
                    base_backoff_ms,
                );
                continue;
            }
            // Set on failure too (#421), same as success below -- absent,
            // not 0, when the first attempt is also the last.
            if attempt_index > 0 {
                set_leaf(
                    store,
                    &meta_node,
                    "retries_used",
                    Value::from(i64::from(attempt_index)),
                );
            }
            if matches!(op.on_error, OnError::Skip | OnError::Continue) {
                set_leaf(store, &effect_node, "value", Value::None);
            }
            observer.effect_complete(
                &effect_path,
                instance,
                Some(&failure.message),
                super::completed_at_is_truthy(store, &meta_node),
            );
            // The final store write for this effect (whichever
            // `on_error` branch below actually returns) -- same
            // contract as the per-attempt reset's own `observer.
            // write()` above, just for the terminal state instead of a
            // fresh attempt's.
            observer.write();
            if op.on_error == OnError::Fail {
                return Err(failure.into());
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
        observer.effect_complete(
            &effect_path,
            instance,
            None,
            super::completed_at_is_truthy(store, &meta_node),
        );
        observer.write();
        return Ok(());
    }
    unreachable!("the attempt loop always returns on its last iteration")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_negative_timeout_ms_still_resolves_to_one_second() {
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("stringify"));
        params.insert(Value::from("input"), template("1"));
        let mut tool = tool_op(params);
        tool.timeout_ms = Some(electricity_bytecode::effects::TimeoutMs::Literal(-5000));
        assert_eq!(
            resolve_timeout_seconds(&tool, "t", &Value::None, &Value::None),
            Ok(1)
        );
    }

    #[test]
    fn an_unknown_provider_lists_pythons_full_registry() {
        let registry = ToolRegistry::new();
        let err = match resolve_plugin(&registry, "bogus", &Value::None) {
            Err(err) => err,
            Ok(_) => panic!("expected an unknown-plugin error"),
        };
        assert!(err.message.starts_with("Unknown plugin: 'bogus'."));
        assert_eq!(err.py_class, electricity_tools::PyExc::ValueError);
    }

    #[tokio::test]
    async fn a_provider_name_is_normalized_before_dispatch() {
        let registry = ToolRegistry::new();
        let resolved = resolve_plugin(&registry, "  JSON ", &Value::None).unwrap();
        let token = CancellationToken::new();
        let err = run_tool(
            &resolved,
            "  JSON ",
            Value::None,
            &Value::None,
            300,
            &token,
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.message,
            "JsonPlugin: parse mode requires params['input'] as a string."
        );
    }

    #[tokio::test]
    async fn a_built_providers_own_error_is_wrapped() {
        let registry = ToolRegistry::new();
        let resolved = resolve_plugin(&registry, "json", &Value::None).unwrap();
        let token = CancellationToken::new();
        let err = run_tool(
            &resolved,
            "json",
            Value::None,
            &Value::None,
            300,
            &token,
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.message,
            "JsonPlugin: parse mode requires params['input'] as a string."
        );
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
            prompt_file: None,
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
        // Leaked (test-only helper, called many times across this
        // module's own tests): every M1-A field below has no test here
        // that reads it, so there is nothing to thread a real lifetime
        // through -- a `'static` default is simpler than adding four
        // more parameters to every one of this function's own callers.
        let adapters: &'a crate::adapter::AdapterRegistry =
            Box::leak(Box::new(crate::adapter::AdapterRegistry::new()));
        let complexity: &'a electricity_config::ComplexitySettings =
            Box::leak(Box::new(electricity_config::ComplexitySettings::default()));
        let dir: &'static std::path::PathBuf = Box::leak(Box::new(std::path::PathBuf::from(".")));
        let declared_prompts: &'a indexmap::IndexMap<String, String> =
            Box::leak(Box::new(indexmap::IndexMap::new()));
        let effect_names: &'a std::collections::BTreeSet<String> =
            Box::leak(Box::new(std::collections::BTreeSet::new()));
        RunContext {
            registry,
            limiter,
            model: "",
            adapter: "_noop",
            runtime_config,
            dry_run: false,
            armed: false,
            adapters,
            default_adapter: std::rc::Rc::new(crate::adapter::NoopAdapter),
            model_locked: false,
            adapter_timeout_seconds: 120,
            complexity,
            decomposition_depth: 0,
            orchestration_dir: dir.as_path(),
            declared_prompts,
            effect_names,
            supported: electricity_bytecode::Supported::m0(&[]),
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
            &Frame::root(),
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
            &Frame::root(),
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "json: unknown mode 'bogus'");
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
                as_json: false,
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
            &Frame::root(),
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Tool effect 'fetch' param 'params.n': '{from: input.missing}' did not \
             resolve to a value."
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
                _instance: crate::observer::InstanceId,
                _error: Option<&str>,
                _completed_at_is_truthy: bool,
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
            &Frame::root(),
            &run_ctx,
            &observer,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "tool 'json' not in enabled_tools allowlist (enabled: ['other'])"
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
            &Frame::root(),
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
            &Frame::root(),
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "params_json must not set ['allowed_commands']: security-sensitive \
             settings are only honoured from a document's literal params block."
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
            &Frame::root(),
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "params.allowed_commands must be a literal list of strings; a templated \
             value is not honoured for this security-sensitive setting."
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
            &Frame::root(),
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
        assert_eq!(
            meta.get(&Value::from("retries_used")),
            Some(&Value::from(1i64))
        );
    }

    #[tokio::test]
    async fn an_overflowing_cel_expect_retries_then_fails_with_overflow_text() {
        // Issue #400: `value` itself is too large for CEL's 64-bit
        // `int` -- `evaluate_cel_expect`'s own `_to_cel` conversion
        // raises a bare `ValueError("overflow")` before Circuitry's own
        // `evaluate_expect` ever gets a `CelEvaluationError` to catch,
        // so `core/tool.py`'s attempt loop sees it exactly like any
        // other attempt exception: retried like normal, `meta.error`
        // reads "overflow" (not "expect failed: ..."), and `meta.expect`
        // is never set at all (a failed, non-raising expectation sets
        // it; this never reaches that code).
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("parse"));
        params.insert(Value::from("input"), template("1180591620717411303424"));
        let mut tool = tool_op(params);
        tool.retries = electricity_bytecode::RetryPolicy {
            max_attempts: 2,
            backoff_ms: 0,
        };
        tool.expect = Some(ExpectCondition::Cel {
            expr: "value == value".to_string(),
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
            &Frame::root(),
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
            Some(&Value::from("overflow"))
        );
        assert!(!meta.contains_key(&Value::from("expect")));
        assert_eq!(
            meta.get(&Value::from("retries_used")),
            Some(&Value::from(1i64))
        );
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
            &Frame::root(),
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
            &Frame::root(),
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

    /// `core/tool.py::ToolRuntime.execute`'s own `with concurrency_cm:`
    /// ends before a `mode: model` `expect:` -- issue #513's own test
    /// list, item "the tool limiter slot is free during the expect
    /// call". Blocks inside `generate()` until released, so the probe
    /// below can observe the slot while the call is still pending.
    struct BlockingAdapter {
        started: std::rc::Rc<tokio::sync::Notify>,
        release: std::rc::Rc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait(?Send)]
    impl crate::adapter::Adapter for BlockingAdapter {
        fn name(&self) -> &str {
            "fake"
        }

        async fn generate(
            &self,
            _model: &str,
            _prompt: &str,
            _options: &crate::adapter::GenerateOptions,
            _call: &crate::adapter::CallContext<'_>,
        ) -> Result<crate::adapter::GenerateResult, crate::adapter::AdapterError> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(crate::adapter::GenerateResult {
                text: "yes".to_string(),
                raw: Value::None,
                tokens_sent: None,
                tokens_received: None,
                finish_reason: None,
                warnings: Vec::new(),
                cost_usd: None,
            })
        }

        async fn check(&self) -> crate::adapter::CheckResult {
            crate::adapter::CheckResult {
                ok: true,
                ..Default::default()
            }
        }
    }

    #[tokio::test]
    async fn the_tool_limiter_slot_is_free_during_a_model_expects_dispatch() {
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("parse"));
        params.insert(Value::from("input"), template("{\"ok\": true}"));
        let mut tool = tool_op(params);
        tool.expect = Some(ExpectCondition::Model {
            template: electricity_bytecode::TemplateText::new(
                "ok?",
                false,
                electricity_bytecode::Escape::Html,
            ),
        });
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Fail)
        };
        let store = crate::Store::new();
        let registry = json_registry();
        let limiter = crate::Limiter::with_limits(Some(1), []);
        let runtime_config = Value::None;
        let adapters = crate::adapter::AdapterRegistry::new();
        let complexity = electricity_config::ComplexitySettings::default();
        let dir = std::path::PathBuf::from(".");
        let declared_prompts: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
        let effect_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let started = std::rc::Rc::new(tokio::sync::Notify::new());
        let release = std::rc::Rc::new(tokio::sync::Notify::new());
        let adapter = std::rc::Rc::new(BlockingAdapter {
            started: started.clone(),
            release: release.clone(),
        });
        let run_ctx = RunContext {
            registry: &registry,
            limiter: &limiter,
            model: "m",
            adapter: "fake",
            runtime_config: &runtime_config,
            dry_run: false,
            armed: false,
            adapters: &adapters,
            default_adapter: adapter,
            model_locked: false,
            adapter_timeout_seconds: 120,
            complexity: &complexity,
            decomposition_depth: 0,
            orchestration_dir: dir.as_path(),
            declared_prompts: &declared_prompts,
            effect_names: &effect_names,
            supported: electricity_bytecode::Supported::m0(&[]),
        };
        let token = CancellationToken::new();
        let empty_ctx = Value::None;
        let frame = Frame::root();

        let run = execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &empty_ctx,
            &frame,
            &run_ctx,
            &crate::NullObserver,
            &token,
        );
        let probe = async {
            started.notified().await;
            let guard = run_ctx.limiter.try_acquire(None);
            assert!(
                matches!(guard, Ok(Some(_))),
                "the limiter slot must be free while a model expect's own dispatch is pending, got {guard:?}"
            );
            release.notify_one();
        };
        let (result, ()) = tokio::join!(run, probe);
        result.expect("the run must still succeed once the expect call returns");
    }

    #[tokio::test]
    async fn a_model_mode_expects_raised_adapter_failure_is_an_ordinary_tool_failure() {
        // `default_run_ctx`'s own default adapter is `NoopAdapter`, so
        // the model expect's own call always fails the same way a real
        // run's own unconfigured adapter would -- this probes that
        // `ExpectCondition::Model` reaches `condition_model::
        // evaluate_model_expect` for real (issue #513), never
        // `unreachable!()`, and that a `Raised` failure becomes an
        // ordinary tool failure, its own message (not an `expect
        // failed: ...` summary -- that's only for a false/unparseable
        // answer, never a raised one), with `meta.expect` left absent
        // (popped at attempt start, never written again on this path).
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("parse"));
        params.insert(Value::from("input"), template("2"));
        let mut tool = tool_op(params);
        tool.expect = Some(ExpectCondition::Model {
            template: electricity_bytecode::TemplateText::new(
                "well?",
                false,
                electricity_bytecode::Escape::None,
            ),
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

        let err = execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &Frame::root(),
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                err,
                VmError::Raised { ref message, .. }
                    if message == "_noop adapter: no real adapter is configured for this run"
            ),
            "expected the noop adapter's own failure, got: {err:?}"
        );

        let snapshot = store.snapshot(&store.root);
        let node = snapshot
            .as_dict()
            .unwrap()
            .get(&Value::from("fetch"))
            .unwrap()
            .as_dict()
            .unwrap();
        let meta = node.get(&Value::from("meta")).unwrap().as_dict().unwrap();
        assert_eq!(meta.get(&Value::from("expect")), None);
    }

    /// Captures the *timeout_seconds* a tool's own model `expect:`
    /// dispatches with, replying `"yes"` so the attempt succeeds.
    #[derive(Default)]
    struct TimeoutCapturingAdapter {
        seen: std::cell::Cell<Option<u32>>,
    }

    #[async_trait::async_trait(?Send)]
    impl crate::adapter::Adapter for TimeoutCapturingAdapter {
        fn name(&self) -> &str {
            "fake"
        }

        async fn generate(
            &self,
            _model: &str,
            _prompt: &str,
            _options: &crate::adapter::GenerateOptions,
            call: &crate::adapter::CallContext<'_>,
        ) -> Result<crate::adapter::GenerateResult, crate::adapter::AdapterError> {
            self.seen.set(Some(call.timeout_seconds));
            Ok(crate::adapter::GenerateResult {
                text: "yes".to_string(),
                raw: Value::None,
                tokens_sent: Some(1),
                tokens_received: Some(1),
                finish_reason: None,
                warnings: Vec::new(),
                cost_usd: None,
            })
        }

        async fn check(&self) -> crate::adapter::CheckResult {
            crate::adapter::CheckResult {
                ok: true,
                ..Default::default()
            }
        }
    }

    /// Issue #513 review finding 4: a tool's own model `expect:` must
    /// dispatch with [`resolve_timeout_seconds`]'s own result (this
    /// tool's `timeout_ms: 5000` resolves to 5s) -- never `run_ctx.
    /// adapter_timeout_seconds` (120 here), which is only for `if`/
    /// `while`/a `use` expect instead (that's `use_.rs`'s own call
    /// site, pinned by its own sibling test).
    #[tokio::test]
    async fn a_tools_model_expect_dispatches_with_the_tools_own_resolved_timeout() {
        let mut params = IndexMap::new();
        params.insert(Value::from("mode"), template("parse"));
        params.insert(Value::from("input"), template("{\"ok\": true}"));
        let mut tool = tool_op(params);
        tool.timeout_ms = Some(electricity_bytecode::effects::TimeoutMs::Literal(5000));
        tool.expect = Some(ExpectCondition::Model {
            template: electricity_bytecode::TemplateText::new(
                "ok?",
                false,
                electricity_bytecode::Escape::Html,
            ),
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
        let adapters = crate::adapter::AdapterRegistry::new();
        let complexity = electricity_config::ComplexitySettings::default();
        let dir = std::path::PathBuf::from(".");
        let declared_prompts: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
        let effect_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let adapter = std::rc::Rc::new(TimeoutCapturingAdapter::default());
        let run_ctx = RunContext {
            registry: &registry,
            limiter: &limiter,
            model: "m",
            adapter: "fake",
            runtime_config: &runtime_config,
            dry_run: false,
            armed: false,
            adapters: &adapters,
            default_adapter: adapter.clone(),
            model_locked: false,
            adapter_timeout_seconds: 120,
            complexity: &complexity,
            decomposition_depth: 0,
            orchestration_dir: dir.as_path(),
            declared_prompts: &declared_prompts,
            effect_names: &effect_names,
            supported: electricity_bytecode::Supported::m0(&[]),
        };
        let token = CancellationToken::new();

        execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &Frame::root(),
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .expect("a model expect answered 'yes' must succeed");

        assert_eq!(
            adapter.seen.get(),
            Some(5),
            "a tool's own model expect must dispatch with its own resolved timeout, \
             never run_ctx.adapter_timeout_seconds"
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
            &Frame::root(),
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
                &Frame::root(),
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

    #[tokio::test]
    async fn a_build_error_never_waits_for_the_limiter_or_sets_waiting_for() {
        // Review finding 6 on PR #454: `core/tool.py::ToolRuntime.
        // execute` builds the plugin (`tool.py:702`) *before* the
        // limiter wait (`tool.py:724`), so a per-plugin config error
        // fails the attempt without ever touching the limiter. Proven
        // here by holding the *only* slot in a group this tool
        // dispatches into, then dispatching a provider `resolve_plugin`
        // can never resolve -- if the build happened after acquiring
        // (the old ordering), this call would hang forever waiting for
        // the held slot; instead it must return promptly, having never
        // set `meta.waiting_for` at all.
        let mut tool = tool_op(IndexMap::new());
        tool.provider = "bogus".to_string();
        tool.group = Some("io".to_string());
        tool.retries = electricity_bytecode::RetryPolicy {
            max_attempts: 1,
            backoff_ms: 0,
        };
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Fail)
        };
        let store = crate::Store::new();
        let registry = ToolRegistry::new();
        let limiter = crate::Limiter::with_limits(None, [("io".to_string(), 1)]);
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        // Hold the group's only slot for the whole call -- a build
        // that incorrectly waited on it first would never see it
        // freed within this test's own timeout.
        let holder = limiter.acquire(Some("io")).await.unwrap();

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            execute_tool(
                &op,
                &tool,
                &store,
                &store.root,
                &Value::None,
                &Frame::root(),
                &run_ctx,
                &crate::NullObserver,
                &token,
            ),
        )
        .await
        .expect("a build error must fail immediately, never waiting on the held slot");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .starts_with("Unknown plugin: 'bogus'.")
        );

        let effect_node = store
            .ensure_dict(&store.root, Value::from("fetch"))
            .unwrap();
        let meta_node = store
            .ensure_dict(&effect_node, Value::from("meta"))
            .unwrap();
        assert_eq!(get_leaf(&meta_node, "waiting_for"), Some(Value::None));
        drop(holder);
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
            _call: &electricity_tools::ToolCall<'_>,
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
            &Frame::root(),
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
    async fn a_build_error_fails_one_attempt_and_is_retried_like_any_other_failure() {
        // S1 (M1 gate seam round 2): `registry::Builder` is now
        // fallible, like `plugins/factory.py::_build_shell`/
        // `plugin_env_override`, which validate their own config and
        // raise. `flaky_build` (`test-tools` feature) fails its first
        // build, succeeds on the second -- dispatched entirely through
        // `resolve_plugin`'s own fallback to `electricity_tools::
        // build_plugin` (never registered in `registry` directly), so
        // this exercises the real per-attempt rebuild, not a stand-in.
        let mut plugins = Dict::new();
        let mut flaky_build_cfg = Dict::new();
        flaky_build_cfg.insert(
            Value::from("id"),
            Value::from("a_build_error_fails_one_attempt_and_is_retried"),
        );
        flaky_build_cfg.insert(Value::from("fail_times"), Value::from(1i64));
        plugins.insert(Value::from("flaky_build"), Value::Dict(flaky_build_cfg));
        let mut runtime = Dict::new();
        runtime.insert(Value::from("plugins"), Value::Dict(plugins));
        let runtime_config = Value::Dict(runtime);

        let mut tool = tool_op(IndexMap::new());
        tool.provider = "flaky_build".to_string();
        tool.retries = electricity_bytecode::RetryPolicy {
            max_attempts: 2,
            backoff_ms: 0,
        };
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Fail)
        };
        let store = crate::Store::new();
        let registry = ToolRegistry::new();
        let limiter = crate::Limiter::new();
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &Frame::root(),
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
        assert_eq!(meta.get(&Value::from("waiting_for")), Some(&Value::None));
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
                _call: &electricity_tools::ToolCall<'_>,
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
            &Frame::root(),
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "flaky tool reported failure (ok=False)");
    }

    #[tokio::test]
    async fn meta_binary_is_set_generically_from_any_providers_own_raw_binary_key() {
        // S2 (M1 gate seam round 2): `core/tool.py:822-823`'s own
        // `if "binary" in result.raw: meta["binary"] = result.raw[
        // "binary"]` is generic across every provider, not lane E's
        // own HTTP-family write -- `json` never produces one, so this
        // uses a test-only tool whose `raw` does.
        struct BinaryTool;
        #[async_trait::async_trait(?Send)]
        impl electricity_tools::ToolPlugin for BinaryTool {
            fn name(&self) -> &str {
                "binary_tool"
            }
            async fn execute(
                &self,
                _params: Value,
                _call: &electricity_tools::ToolCall<'_>,
            ) -> Result<electricity_tools::ToolResult, electricity_tools::ToolError> {
                let mut raw = Dict::new();
                raw.insert(Value::from("binary"), Value::from("/usr/bin/widget"));
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
        tool.provider = "binary_tool".to_string();
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Fail)
        };
        let store = crate::Store::new();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(BinaryTool));
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
            &Frame::root(),
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
            meta.get(&Value::from("binary")),
            Some(&Value::from("/usr/bin/widget"))
        );
    }

    #[tokio::test]
    async fn meta_binary_is_popped_at_the_start_of_the_next_attempt() {
        // `core/tool.py:662`'s own `meta.pop("binary", None)` at every
        // attempt start -- a reused node (an unnamed loop's next pass,
        // or this effect's own next attempt) must not keep a prior
        // pass's `binary` sitting next to a provider that never sets
        // one, the same reason `stdout`/`stderr`/`raw` are reset too.
        let mut tool = tool_op(IndexMap::new());
        tool.provider = "json".to_string();
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Fail)
        };
        let store = crate::Store::new();
        let registry = ToolRegistry::new();
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = default_run_ctx(&registry, &limiter, &runtime_config);
        let token = CancellationToken::new();

        let effect_node = store
            .ensure_dict(&store.root, Value::from("fetch"))
            .unwrap();
        let meta_node = store
            .ensure_dict(&effect_node, Value::from("meta"))
            .unwrap();
        set_leaf(&store, &meta_node, "binary", Value::from("/usr/bin/stale"));

        execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &Frame::root(),
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap_err();

        let snapshot = store.snapshot(&store.root);
        let node = snapshot
            .as_dict()
            .unwrap()
            .get(&Value::from("fetch"))
            .unwrap()
            .as_dict()
            .unwrap();
        let meta = node.get(&Value::from("meta")).unwrap().as_dict().unwrap();
        assert_eq!(meta.get(&Value::from("binary")), None);
    }

    /// A test-only [`electricity_tools::ToolPlugin`] that always raises
    /// a typed [`electricity_tools::ToolError`] -- review finding 1 on
    /// PR #454's own probe: "a FailTool returning `ErrorCause::NoReply`/
    /// `PyExc::ValueError` reaches `execute_tool` with both fields
    /// intact".
    struct RaisingTool;

    #[async_trait::async_trait(?Send)]
    impl electricity_tools::ToolPlugin for RaisingTool {
        fn name(&self) -> &str {
            "raising"
        }

        async fn execute(
            &self,
            _params: Value,
            _call: &electricity_tools::ToolCall<'_>,
        ) -> Result<electricity_tools::ToolResult, electricity_tools::ToolError> {
            Err(electricity_tools::ToolError {
                py_class: electricity_tools::PyExc::ValueError,
                message: "no reply from the server".to_string(),
                cause: electricity_tools::ErrorCause::NoReply,
            })
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
    async fn a_raised_tool_errors_class_and_cause_reach_execute_tool_intact() {
        let mut tool = tool_op(IndexMap::new());
        tool.provider = "raising".to_string();
        tool.retries = electricity_bytecode::RetryPolicy {
            max_attempts: 1,
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
        registry.register(Box::new(RaisingTool));
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
            &Frame::root(),
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap_err();

        let VmError::Raised {
            py_class,
            message,
            cause,
            ..
        } = err
        else {
            panic!("expected a VmError::Raised, got a different VmError variant");
        };
        assert_eq!(py_class, electricity_tools::PyExc::ValueError);
        assert_eq!(cause, electricity_tools::ErrorCause::NoReply);
        assert_eq!(message, "no reply from the server");
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
                _call: &electricity_tools::ToolCall<'_>,
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
            &Frame::root(),
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
                &Frame::root(),
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
    // `VmError::Raised` failure or a successful completion once the slot
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
                &Frame::root(),
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
                _call: &electricity_tools::ToolCall<'_>,
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
                &Frame::root(),
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
                &Frame::root(),
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
    // Review finding 6 on PR #467: this test's own sibling above ("...
    // still_runs_a_ready_slot_and_tool") already proves a ready slot
    // wins the *acquire* race even with the token already cancelled --
    // this proves the *other* half, for a dispatch that itself
    // surfaces that same already-cancelled token as a tool failure
    // (exactly what a process-backed tool's `run_tracked` does: it
    // returns `Err(Cancelled)` synchronously, before its first
    // `.await`, turned into `ToolError::message("cancelled")` by
    // `subprocess_tool.rs`/`shell.rs`/`ffmpeg.rs` alike). Against
    // `biased;`, that makes the dispatch `select!`'s `run_tool(...)`
    // branch already `Ready` on its very first poll, so without this
    // fix it would always win over `token.cancelled()` -- this must
    // still come back as `VmError::Cancelled`, not an ordinary,
    // `on_error`-recordable failure.
    async fn a_cancelled_token_before_the_call_with_a_dispatch_that_itself_says_cancelled() {
        struct SynchronouslyCancelledTool;
        #[async_trait::async_trait(?Send)]
        impl electricity_tools::ToolPlugin for SynchronouslyCancelledTool {
            fn name(&self) -> &str {
                "synchronously_cancelled"
            }
            async fn execute(
                &self,
                _params: Value,
                _call: &electricity_tools::ToolCall<'_>,
            ) -> Result<electricity_tools::ToolResult, electricity_tools::ToolError> {
                // No `.await` at all -- mirrors `run_tracked`'s own
                // `if token.is_set() { return Err(ProcError::Cancelled) }`
                // ahead of `Popen`, which never yields either.
                Err(electricity_tools::ToolError::message("cancelled"))
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
        tool.provider = "synchronously_cancelled".to_string();
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Fail)
        };
        let store = crate::Store::new();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(SynchronouslyCancelledTool));
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
            &Frame::root(),
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap_err();
        assert_eq!(err, VmError::Cancelled);
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
                &Frame::root(),
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
                &Frame::root(),
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

    /// A test-only HTTP-family [`electricity_tools::ToolPlugin`]
    /// double, registered as `http` itself (`http_family_provider`'s
    /// own exact-spelling check) -- a scripted queue of `(status,
    /// headers, ok)` results, exercising `meta.status_code` and the
    /// status-classified retry branch without a real socket (issue
    /// #485; the real `HttpPlugin`'s own end-to-end behaviour is
    /// pinned by `electricity-tools`' own `tests/http_plugin.rs`
    /// instead).
    type ScriptedHttpResponse = (i64, Vec<(&'static str, &'static str)>, bool);

    struct HttpShapedTool {
        responses: std::cell::RefCell<std::collections::VecDeque<ScriptedHttpResponse>>,
    }

    #[async_trait::async_trait(?Send)]
    impl electricity_tools::ToolPlugin for HttpShapedTool {
        fn name(&self) -> &str {
            "http"
        }

        async fn execute(
            &self,
            _params: Value,
            _call: &electricity_tools::ToolCall<'_>,
        ) -> Result<electricity_tools::ToolResult, electricity_tools::ToolError> {
            let (status, headers, ok) = self
                .responses
                .borrow_mut()
                .pop_front()
                .expect("HttpShapedTool: scripted response queue exhausted");
            let mut headers_dict = Dict::new();
            for (k, v) in headers {
                headers_dict.insert(Value::from(k), Value::from(v));
            }
            let mut raw = Dict::new();
            raw.insert(Value::from("status"), Value::from(status));
            raw.insert(Value::from("headers"), Value::Dict(headers_dict));
            let mut result = electricity_tools::ToolResult::new(Value::from("v"), Value::Dict(raw));
            result.ok = ok;
            if !ok {
                result.stderr = Some(format!("HTTP {status}: reason"));
            }
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

    fn http_tool_op(max_attempts: u32) -> ToolOp {
        let mut tool = tool_op(IndexMap::new());
        tool.provider = "http".to_string();
        tool.retries = electricity_bytecode::RetryPolicy {
            max_attempts,
            backoff_ms: 0,
        };
        tool
    }

    #[tokio::test]
    async fn a_429_with_retry_after_zero_then_200_retries_once_and_writes_status_code() {
        let tool = http_tool_op(3);
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Fail)
        };
        let store = crate::Store::new();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(HttpShapedTool {
            responses: std::cell::RefCell::new(
                vec![
                    (429i64, vec![("retry-after", "0")], false),
                    (200i64, vec![], true),
                ]
                .into(),
            ),
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
            &Frame::root(),
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
            meta.get(&Value::from("status_code")),
            Some(&Value::from(200i64))
        );
        assert_eq!(
            meta.get(&Value::from("retries_used")),
            Some(&Value::from(1i64))
        );
        assert_eq!(meta.get(&Value::from("error")), Some(&Value::None));
    }

    #[tokio::test]
    async fn a_401_is_not_retried_even_with_retries_configured() {
        let tool = http_tool_op(3);
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Continue)
        };
        let store = crate::Store::new();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(HttpShapedTool {
            responses: std::cell::RefCell::new(vec![(401i64, vec![], false)].into()),
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
            &Frame::root(),
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
            meta.get(&Value::from("status_code")),
            Some(&Value::from(401i64))
        );
        assert!(!meta.contains_key(&Value::from("retries_used")));
        assert_eq!(
            meta.get(&Value::from("error")),
            Some(&Value::from("HTTP 401: reason"))
        );
    }

    #[tokio::test]
    async fn a_500_retries_and_a_differently_spelled_http_provider_does_not_classify_by_status() {
        // Provider spelling (issue #485): " HTTP " still dispatches
        // this same plugin instance (registered under its own
        // normalized name by `ToolRegistry::get`'s own lookup key --
        // this test registers the double under `"http"` and dispatches
        // it by provider `" HTTP "`, which `resolve_plugin` normalizes
        // to `"http"` before the registry lookup), but `tool.provider`
        // itself (unnormalized) is what `http_family_provider` checks,
        // so this is *not* classified HTTP family: no `status_code` is
        // ever written, and a 500 retries unconditionally exactly as
        // any non-HTTP-family provider's failure would.
        let mut tool = http_tool_op(2);
        tool.provider = " HTTP ".to_string();
        let op = Op {
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool.clone()),
            )),
            ..tool_node(OnError::Fail)
        };
        let store = crate::Store::new();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(HttpShapedTool {
            responses: std::cell::RefCell::new(
                vec![(500i64, vec![], false), (200i64, vec![], true)].into(),
            ),
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
            &Frame::root(),
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
        assert!(!meta.contains_key(&Value::from("status_code")));
        assert_eq!(
            meta.get(&Value::from("retries_used")),
            Some(&Value::from(1i64))
        );
    }
}
